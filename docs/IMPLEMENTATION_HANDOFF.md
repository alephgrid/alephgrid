# Implementation handoff: phase 3

Updated: 26 October 2026. Branch `phase-3-bounded-execution` (from `phase-2-local-spreadsheet` at `7c9c65e`).
Nothing pushed. Research docs (`docs/RESEARCH.md`, `OPEN_SOURCE_ASSESSMENT.md`, `SQL_AND_CUSTOM_FUNCTIONS.md`) are
excluded via `.git/info/exclude` and must never be committed.

## Commits on this branch

| Commit | Content |
|---|---|
| `fbc0b50` | Enhanced checklist and enterprise architecture decision (user-provided, unchanged) |
| `7b1e5ef` | Engine storage v2: stable ids (`_rf_id` + `_rf_ord`, position index), revisions/history, `mutate`, migrations v1→v2 with `.v1.bak`, readers, scratch DB, view expiry, snapshot backup, limits; TEST_MATRIX phase-3 contracts |
| `d401a5e` | Runtime objects, Local provider (flock), controller (single-flight, cancel, drain/stop, op ids, reconcile), host budget, idle eviction, runtime HTTP endpoints, flags |
| `d8d8a68` | Scheduler (lanes, priority, fairness, bounded queues, deadlines, cancel), size limits, outbox backpressure, revisions/mutation ids/codes on the wire |

## Phase 3 status (no checklist box ticked yet)

Implemented and tested server/engine side: 3.01, 3.02, 3.03, 3.04, 3.06, 3.07, 3.11, 3.12, 3.13, 3.14.
Remaining:
- **3.05 client + client side of 3.01/3.07/3.11**: delegated to a background agent (web/src, web/e2e only):
  column-range fetch, byte-bounded cache, mutation-id resend after reconnect, codes, workbook_closed/resync/progress
  events, SQL cancel button, failure-state UI, paste guard, browser tests. Review its diff, run the suite, commit.
- **3.08 backup/restore**: engine `Reader::backup_to` exists (COPY FROM DATABASE in one txn). Still to build: data-dir
  backup (manifest with sha256 + revision per workbook, workbook.json, workspaces.json), `rowfinity backup <dir>`
  (offline, fails closed if a server holds the data dir) and `POST /api/admin/backups` (online), `rowfinity restore
  <dir> --data-dir <empty>` verifying checksums and refusing non-empty targets; tests incl. backup during edits,
  corrupt backup, non-empty target. Document rollback limits (no downgrade; `.v1.bak` is the only way back).
- **3.09 SIGTERM**: main.rs drains on the first signal, exits on the second, caps waiting for open connections.
  Tests still to write (real binary, like crash.rs): SIGTERM idle; during an import (no partial table after
  restart); during a stream of acknowledged edits (all present); twice.
- **3.15**: write docs/DURABILITY.md (ack = DuckDB COMMIT incl. revision/history; process crash tested by SIGKILL;
  power loss / host failure not tested; storage loss needs backups; isolation classes from runtime/mod.rs).
- **3.T03 storage faults**: real small filesystem on macOS (`hdiutil create -size 30m -fs HFS+ -attach`) or Linux
  tmpfs, gated by an env var pointing at it; permission failures (read-only data dir). App-level faults exist:
  `state::arm_fault_in("write_durably"|"upload_write"|"open_workbook", dir, n)`, engine `faults::fail_next("history"|
  "commit"|"migrate"|"backup"|...)`.
- Docs: PROTOCOL.md (new ops, envelope fields, codes, events, HTTP endpoints), ARCHITECTURE.md (storage v2, runtime,
  scheduler, budgets), TEST_MATRIX results per task, checklist boxes, then gate 3.10.

## Commands and last results (macOS arm64, local)

- `cargo test --workspace`: engine 100; server unit 13, api 9, crash 3, imports 5, journey 2, protocol 4, runtime 10,
  scheduling 10 — all pass (`d8d8a68`).
- `PROPTEST_CASES=300 cargo test -p rowfinity-engine --lib props`: 4 pass (263 s).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cd web && npm run build && cd .. && cargo build -p rowfinity-server && cd web && npx playwright test`: 21 pass
  (four consecutive runs). One earlier run had a single "server did not start" failure that did not recur; cause unknown.
- `UPDATE_BINDINGS=1 cargo test -p rowfinity-server --test protocol` regenerates web/src/protocol.ts.

## Known gaps found so far

- A raw client that keeps its TCP socket open after the server closed a session delayed the in-process test server's
  graceful stop; main.rs now caps the wait (grace + 10 s). Root cause not pinned down.
- Limits are DuckDB-level (buffer manager, threads, spill files) plus our accounting; not OS-level.

## Running processes / temporary resources

- A dev server from phase 2 may still run on :7878 (background task) with data in the session scratchpad.
- The client agent builds `target/debug/rowfinity` and runs Playwright; wait for it before running e2e.
