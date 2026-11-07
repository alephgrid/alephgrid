"""Checks that keep the CI test tiers honest (docs/TEST_STRATEGY.md, docs/CI.md).

Run from the repository root: `python3 tools/test_gate.py`. It fails when

* a Rust test is `#[ignore]`d, or a Playwright test is skipped, marked `fixme` or `fail`,
  without a row in the "Skipped and quarantined tests" table of docs/TEST_STRATEGY.md
  (quarantined rows need an owner and a follow-up), or a row names nothing that exists;
* a Playwright test is focused with `.only`, or Playwright is configured to retry;
* a suite a tier depends on (authorization, protocol, reconnect, crash recovery,
  install/restore, browser journeys, property and fuzz targets) is missing or has no tests;
* a test command in .circleci/config.yml is made to pass regardless (`|| true`, `|| :`,
  `; true`, `|| exit 0`).

Standard library only, so it runs in any CI job.
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
STRATEGY = ROOT / "docs/TEST_STRATEGY.md"
TABLE_HEADING = "## Skipped and quarantined tests"
KINDS = {"benchmark", "generator", "platform", "quarantined"}

# Suites the PR, nightly and release tiers rely on (9.10). Each must exist and hold tests.
REQUIRED = {
    "authorization": ["crates/alephgrid-server/tests/teams.rs", "crates/alephgrid-server/tests/security.rs",
                      "web/e2e/teams.spec.ts", "web/e2e/security.spec.ts"],
    "protocol": ["crates/alephgrid-server/tests/protocol.rs", "crates/alephgrid-server/tests/api.rs"],
    "reconnect": ["crates/alephgrid-server/tests/collaboration.rs", "web/e2e/resilience.spec.ts",
                  "web/e2e/collaboration.spec.ts"],
    "crash recovery": ["crates/alephgrid-server/tests/crash.rs", "crates/alephgrid-server/tests/shutdown.rs",
                       "crates/alephgrid-server/tests/storage_faults.rs"],
    "install and restore": ["crates/alephgrid-server/tests/backup.rs", "tools/release/smoke.sh"],
    "browser journeys": ["web/e2e/journey.spec.ts"],
    "property and fuzz": ["crates/alephgrid-core/src/tests/props.rs", "crates/alephgrid-core/src/tests/fuzz.rs"],
}
TEST_MARKERS = {".rs": re.compile(r"#\[(tokio::)?test\b|proptest!"), ".ts": re.compile(r"^\s*test\(", re.M),
                ".sh": re.compile(r".")}

problems = []


def rel(p):
    return p.relative_to(ROOT).as_posix()


def allowlist():
    text = STRATEGY.read_text()
    if TABLE_HEADING not in text:
        problems.append(f"docs/TEST_STRATEGY.md has no '{TABLE_HEADING}' section")
        return []
    rows = []
    for line in text.split(TABLE_HEADING, 1)[1].splitlines()[1:]:
        if line.startswith("## "):
            break
        if not line.startswith("|") or set(line) <= set("|- "):
            continue
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if cells[0] == "Kind":
            continue
        if len(cells) != 6:
            problems.append(f"TEST_STRATEGY.md skip table: expected 6 columns: {line}")
            continue
        kind, where, what, reason, owner, follow = cells
        where, what = where.strip("`"), what.strip("`").strip('"')
        if kind not in KINDS:
            problems.append(f"TEST_STRATEGY.md skip table: unknown kind {kind!r} (one of {sorted(KINDS)})")
        if not reason or reason == "—":
            problems.append(f"TEST_STRATEGY.md skip table: {where} {what} has no reason")
        if kind == "quarantined" and (owner in ("", "—") or follow in ("", "—")):
            problems.append(f"quarantined {where} {what} needs an owner and a follow-up")
        rows.append({"kind": kind, "where": where, "what": what, "seen": False})
    return rows


def find(rows, where, what):
    for r in rows:
        if r["where"] == where and r["what"] == what:
            r["seen"] = True
            return True
    return False


def check_rust(rows):
    for path in sorted(ROOT.glob("crates/**/*.rs")):
        lines = path.read_text().splitlines()
        for i, line in enumerate(lines):
            if not re.match(r"\s*#\[ignore\b", line):
                continue
            name = next((m.group(1) for l in lines[i + 1:i + 8] if (m := re.search(r"\bfn\s+(\w+)", l))), "?")
            if not find(rows, rel(path), name):
                problems.append(f"{rel(path)}:{i + 1}: #[ignore] on {name} is not listed in docs/TEST_STRATEGY.md")


SKIP = re.compile(r"\btest(?:\.describe)?\.(skip|fixme|fail)\((.*)")
ONLY = re.compile(r"\b(test|describe)(\.describe)?\.only\(")


def check_playwright(rows):
    for path in sorted(ROOT.glob("web/e2e/**/*.ts")):
        for i, line in enumerate(path.read_text().splitlines()):
            if ONLY.search(line):
                problems.append(f"{rel(path)}:{i + 1}: a focused test (.only) runs nothing else")
            m = SKIP.search(line)
            if not m:
                continue
            reason = re.findall(r"""(["'`])((?:(?!\1).)*)\1""", m.group(2))
            reason = reason[-1][1] if reason else "?"
            if not find(rows, rel(path), reason):
                problems.append(f"{rel(path)}:{i + 1}: test.{m.group(1)} ({reason!r}) is not listed in docs/TEST_STRATEGY.md")
    config = (ROOT / "web/playwright.config.ts").read_text()
    if re.search(r"^\s*retries\s*:\s*[1-9]", config, re.M):
        problems.append("web/playwright.config.ts retries failing tests; fix or quarantine them instead")


def check_required():
    for what, paths in REQUIRED.items():
        for p in paths:
            f = ROOT / p
            if not f.is_file():
                problems.append(f"{what}: {p} is missing")
            elif not TEST_MARKERS[f.suffix].search(f.read_text()):
                problems.append(f"{what}: {p} holds no tests")


FORGIVING = re.compile(r"\|\|\s*(true|:|exit 0)\b|;\s*true\s*$")
TEST_COMMAND = re.compile(r"cargo (test|clippy)|playwright test|mutants\.py|test_gate\.py|licenses\.py|smoke\.sh|verify\.sh")


def check_ci():
    config = ROOT / ".circleci/config.yml"
    for i, line in enumerate(config.read_text().splitlines()):
        if not line.lstrip().startswith("#") and TEST_COMMAND.search(line) and FORGIVING.search(line):
            problems.append(f".circleci/config.yml:{i + 1}: a test command must fail the job: {line.strip()}")


rows = allowlist()
check_rust(rows)
check_playwright(rows)
check_required()
check_ci()
for r in rows:
    if not r["seen"]:
        problems.append(f"docs/TEST_STRATEGY.md lists {r['where']} {r['what']!r}, which is no longer skipped or no longer exists")
for p in problems:
    print(p)
quarantined = sum(r["kind"] == "quarantined" for r in rows)
print(f"test gate: {len(rows)} listed skips ({quarantined} quarantined), {sum(map(len, REQUIRED.values()))} required suites, "
      f"{'FAILED' if problems else 'ok'}")
sys.exit(1 if problems else 0)
