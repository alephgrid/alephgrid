# Alephgrid Python worker (docs/PYTHON_FUNCTIONS.md). Runs inside the sandbox, isolated (-I),
# without site (-S) or bytecode files (-B). Reads one JSON job from stdin:
#   {"source", "entry", "returns", "context", "site": [dirs], "calls": [[value, ...], ...]}
# and writes one JSON line: {"results": [result, ...]}, one result per call, in order.
# Values and results use the server's encoding (crate::external::WorkerValue / CallResult).
import sys

def main():
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
        return type(e).__name__ + (": " + text[0][:500] if text else "")

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
