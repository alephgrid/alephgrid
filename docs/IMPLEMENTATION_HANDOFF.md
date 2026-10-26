# Implementation handoff

Updated: 26 October 2026. Branch `phase-3-bounded-execution` (from `phase-2-local-spreadsheet` at `7c9c65e`).
Nothing pushed; no PR. Research docs (`docs/RESEARCH.md`, `OPEN_SOURCE_ASSESSMENT.md`, `SQL_AND_CUSTOM_FUNCTIONS.md`)
are excluded via `.git/info/exclude` and must never be committed.

## State

Phase 3 is complete locally: 3.01–3.09, 3.11–3.15, 3.T01–3.T03 and the 3.10 gate are checked in
IMPLEMENTATION_CHECKLIST.md with evidence in TEST_MATRIX.md (results table, test packages, gate commands).
CI has not run this branch; the first CI run will also be the first Linux run of the phase-3 tests (CI mounts a
64 MB tmpfs for `storage_faults.rs`).

Next phase: 4 (SQL workspace), not started.

## Verify

```bash
ROWFINITY_SMALL_FS=<small fs> cargo test --workspace     # 166 tests; disk-full test runs only with the env var
cargo clippy --workspace --all-targets -- -D warnings
cd web && npm run build && cd .. && cargo build -p rowfinity-server && cd web && npx playwright test   # 29
```
macOS small fs: `hdiutil create -size 64m -fs HFS+ -volname rfsmall -ov rfsmall.dmg && hdiutil attach rfsmall.dmg -mountpoint <dir> -nobrowse`.

## Known gaps

- Power loss / host failure durability untested; limits are DuckDB-level, not OS-level; Local provider only (no
  multi-node failover) — see DURABILITY.md.
- No automated test for the client's "overloaded" retry, the kept-change bounds, or the paste guard.
- One unexplained "server did not start" browser failure early in phase 3 (did not recur in 8+ full runs).
- Each Playwright spec file leaves an empty data directory in TMPDIR (pre-existing `Server` constructor behaviour).

## Temporary resources

A 64 MB disk image may still be attached at the session scratchpad (`rfsmall`); detach with `hdiutil detach`.
A phase-2 dev server may still run on :7878.
