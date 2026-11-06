# Alephgrid Python worker (docs/PYTHON_FUNCTIONS.md). Runs inside the sandbox, isolated (-I),
# without site (-S) or bytecode files (-B). With --forbid-processes (Linux, under bubblewrap) it
# first installs a seccomp filter that refuses new programs and processes. Reads one JSON job from stdin:
#   {"source", "entry", "returns", "context", "site": [dirs], "calls": [[value, ...], ...]}
# and writes one JSON line: {"results": [result, ...]}, one result per call, in order.
# Values and results use the server's encoding (crate::external::WorkerValue / CallResult).
import sys


# Linux system call numbers (the kernel's tables) and seccomp architecture ids.
ARCHES = {
    "x86_64": (0xC000003E, {"execve": 59, "execveat": 322, "fork": 57, "vfork": 58, "clone": 56, "clone3": 435}),
    "aarch64": (0xC00000B7, {"execve": 221, "execveat": 281, "clone": 220, "clone3": 435}),
}
LD_W_ABS, JEQ, JGE, JSET, RET = 0x20, 0x15, 0x35, 0x45, 0x06
ALLOW, KILL, ERRNO = 0x7FFF0000, 0x80000000, 0x00050000
EPERM, ENOSYS, CLONE_THREAD = 1, 38, 0x00010000


def process_filter(machine):
    """The classic-BPF seccomp program for `machine`, as (code, jt, jf, k) tuples: kill on a
    foreign architecture; refuse execve, execveat, fork, vfork and x32 calls (EPERM); clone3
    reports ENOSYS so the C library falls back to clone; clone is allowed only for a thread
    (CLONE_THREAD); everything else is allowed."""
    if machine not in ARCHES:
        raise OSError("no process filter for the %s architecture" % machine)
    arch, nr = ARCHES[machine]
    # Instructions with symbolic jump targets; a 1-tuple marks a label.
    prog = [(LD_W_ABS, None, None, 4),                      # seccomp_data.arch
            (JEQ, None, "kill", arch),
            (LD_W_ABS, None, None, 0)]                      # seccomp_data.nr
    if machine == "x86_64":
        prog.append((JGE, "eperm", None, 0x40000000))      # x32 system calls
    for name in ("execve", "execveat", "fork", "vfork"):
        if name in nr:
            prog.append((JEQ, "eperm", None, nr[name]))
    prog += [(JEQ, "enosys", None, nr["clone3"]),
             (JEQ, "clone", None, nr["clone"]),
             (RET, None, None, ALLOW),                      # any other call
             ("clone",), (LD_W_ABS, None, None, 16),       # clone's flags: low word of args[0]
             (JSET, "allow", "eperm", CLONE_THREAD),
             ("allow",), (RET, None, None, ALLOW),
             ("eperm",), (RET, None, None, ERRNO | EPERM),
             ("enosys",), (RET, None, None, ERRNO | ENOSYS),
             ("kill",), (RET, None, None, KILL)]
    labels, code = {}, []
    for ins in prog:
        if len(ins) == 1:
            labels[ins[0]] = len(code)
        else:
            code.append(ins)
    jump = lambda i, label: 0 if label is None else labels[label] - i - 1
    return [(c, jump(i, t), jump(i, f), k) for i, (c, t, f, k) in enumerate(code)]


def forbid_new_processes():
    """Linux: install process_filter() so neither this worker nor the function can start a
    program or a process (docs/PYTHON_FUNCTIONS.md). It cannot be removed once installed and is
    installed before the function's source is read. Raises if it cannot be installed; the caller
    then refuses to run anything (fail closed)."""
    import ctypes, os

    code = process_filter(os.uname().machine)

    class Filter(ctypes.Structure):
        _fields_ = [("code", ctypes.c_ushort), ("jt", ctypes.c_ubyte), ("jf", ctypes.c_ubyte), ("k", ctypes.c_uint32)]

    class Program(ctypes.Structure):
        _fields_ = [("len", ctypes.c_ushort), ("filter", ctypes.POINTER(Filter))]

    filters = (Filter * len(code))(*[Filter(*ins) for ins in code])
    program = Program(len(code), filters)
    libc = ctypes.CDLL(None, use_errno=True)
    libc.prctl.argtypes = [ctypes.c_int, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong]
    PR_SET_NO_NEW_PRIVS, PR_SET_SECCOMP, SECCOMP_MODE_FILTER = 38, 22, 2
    if libc.prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "PR_SET_NO_NEW_PRIVS failed")
    if libc.prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, ctypes.addressof(program), 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "installing the process filter failed")


def main():
    if "--forbid-processes" in sys.argv:
        try:
            forbid_new_processes()
        except BaseException as e:
            sys.stderr.write("the Python sandbox could not forbid new processes: %s\n" % e)
            sys.exit(70)
    import json
    job = json.loads(sys.stdin.buffer.read())
    for d in job.get("site", []):
        sys.path.append(d)
    import datetime, decimal, math

    def decode(v):
        if v == "null":
            return None
        (kind, x), = v.items()
        if kind == "number":
            return float(x)
        if kind == "integer":
            return int(x)
        if kind == "decimal":
            return decimal.Decimal(x)
        if kind == "text":
            return x
        if kind == "boolean":
            return bool(x)
        if kind == "date":
            return datetime.date.fromisoformat(x)
        if kind == "array":
            return [[decode(c) for c in row] for row in x]
        raise ValueError("unknown value " + kind)

    def error(code, message, transient=False):
        return {"error": {"code": code, "message": message, "transient": transient}}

    def name_of(value):
        return {bool: "a logical", int: "an integer", float: "a number", str: "text", list: "a list",
                tuple: "a tuple", dict: "a dict"}.get(type(value), type(value).__name__)

    returns = job["returns"]
    expected = {"number": "a number", "text": "text", "boolean": "a logical"}[returns]

    def encode(value):
        if value is None:
            return "empty"
        if returns == "number":
            if isinstance(value, bool):
                return error("#VALUE!", "the function returned a logical, expected a number")
            if isinstance(value, int):
                if abs(value) > 2 ** 53:
                    return error("#NUM!", "the function returned an integer beyond 2^53, which a number cannot hold exactly")
                return {"number": float(value)}
            if isinstance(value, (float, decimal.Decimal)):
                f = float(value)
                if not math.isfinite(f):
                    return error("#NUM!", "the function returned a number that is not finite")
                return {"number": f}
        elif returns == "text":
            if isinstance(value, str):
                if len(value) > 32767:
                    return error("#VALUE!", "the function returned more than 32767 characters")
                return {"text": value}
        elif returns == "boolean":
            if isinstance(value, bool):
                return {"boolean": value}
        return error("#VALUE!", "the function returned %s, expected %s" % (name_of(value), expected))

    def describe(e):
        text = str(e).strip().splitlines()
        where = ""
        # The line of the function's own source where it happened (a syntax error names its
        # line in its message already).
        if not isinstance(e, SyntaxError):
            tb, line = e.__traceback__, None
            while tb is not None:
                if tb.tb_frame.f_code.co_filename == "<function>":
                    line = tb.tb_lineno
                tb = tb.tb_next
            if line is not None:
                where = " (line %d)" % line
        return type(e).__name__ + (": " + text[0][:500] if text else "") + where

    # What the function prints, even straight to file descriptor 1, goes to stderr, never into
    # the results.
    import os
    results_out = os.fdopen(os.dup(1), "w")
    os.dup2(2, 1)
    sys.stdout = sys.stderr
    namespace = {"__name__": "alephgrid_function"}
    try:
        exec(compile(job["source"], "<function>", "exec"), namespace)
        fn = namespace.get(job["entry"])
        if not callable(fn):
            raise NameError("the source does not define %s()" % job["entry"])
    except MemoryError:
        out = [error("#CALC!", "the function ran out of memory", True)] * len(job["calls"])
    except BaseException as e:
        out = [error("#VALUE!", "the source could not run: " + describe(e))] * len(job["calls"])
    else:
        out = []
        for call in job["calls"]:
            try:
                out.append(encode(fn(*[decode(a) for a in call])))
            except MemoryError:
                out.append(error("#CALC!", "the function ran out of memory", True))
            except BaseException as e:
                out.append(error("#VALUE!", describe(e)))
    results_out.write(json.dumps({"results": out}, allow_nan=False))
    results_out.write("\n")
    results_out.flush()

main()
