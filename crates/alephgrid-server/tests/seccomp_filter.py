# 8.03: simulates the Python runner's classic-BPF seccomp program (runner.py, process_filter) on
# hand-written cases. Syscall numbers are from the kernel's tables and clone flags from glibc,
# not from the code under test. Run by tests/python.rs; usage: python3 -I seccomp_filter.py runner.py
import sys
src = open(sys.argv[1]).read().replace("\nmain()\n", "\n")
ns = {}; exec(compile(src, "runner", "exec"), ns)
def run(code, arch, nr, arg0=0):
    data = {0: nr, 4: arch, 16: arg0 & 0xFFFFFFFF}
    a = 0; pc = 0
    while True:
        c, jt, jf, k = code[pc]
        if c == 0x20: a = data[k]; pc += 1
        elif c == 0x06: return k
        else:
            cond = {0x15: a == k, 0x35: a >= k, 0x45: (a & k) != 0}[c]
            pc += 1 + (jt if cond else jf)
ALLOW, EPERM, ENOSYS, KILL = 0x7FFF0000, 0x50001, 0x50026, 0x80000000
CLONE_THREAD_FLAGS = 0x3d0f00   # glibc pthread_create: CLONE_VM|FS|FILES|SIGHAND|THREAD|SYSVSEM|SETTLS|PARENT_SETTID|CHILD_CLEARTID
FORK_FLAGS = 0x01200011         # glibc fork(): CLONE_CHILD_SETTID|CLONE_CHILD_CLEARTID|SIGCHLD
SPAWN_FLAGS = 0x4111            # posix_spawn: CLONE_VM|CLONE_VFORK|SIGCHLD
cases = {
 "x86_64": (0xC000003E, [("read", 0, 0, ALLOW), ("write", 1, 0, ALLOW), ("mmap", 9, 0, ALLOW), ("execve", 59, 0, EPERM),
            ("execveat", 322, 0, EPERM), ("fork", 57, 0, EPERM), ("vfork", 58, 0, EPERM), ("clone3", 435, 0, ENOSYS),
            ("clone thread", 56, CLONE_THREAD_FLAGS, ALLOW), ("clone fork", 56, FORK_FLAGS, EPERM),
            ("clone spawn", 56, SPAWN_FLAGS, EPERM), ("x32 read", 0x40000000, 0, EPERM), ("futex", 202, 0, ALLOW)]),
 "aarch64": (0xC00000B7, [("read", 63, 0, ALLOW), ("execve", 221, 0, EPERM), ("execveat", 281, 0, EPERM),
            ("clone3", 435, 0, ENOSYS), ("clone thread", 220, CLONE_THREAD_FLAGS, ALLOW), ("clone fork", 220, FORK_FLAGS, EPERM),
            ("openat", 56, 0, ALLOW)]),
}
bad = 0
for m, (arch, cs) in cases.items():
    code = ns["process_filter"](m)
    assert all(0 <= jt < 256 and 0 <= jf < 256 for _, jt, jf, _ in code)
    for name, nr, arg, want in cs:
        got = run(code, arch, nr, arg)
        ok = got == want; bad += not ok
        print(f"{m:8} {name:14} {'ok' if ok else 'WRONG'} {hex(got)}")
    got = run(code, 0x40000003, 1); bad += got != KILL; print(f"{m:8} foreign arch   {'ok' if got == KILL else 'WRONG'}")
print("ALL OK" if not bad else f"{bad} WRONG"); sys.exit(1 if bad else 0)
