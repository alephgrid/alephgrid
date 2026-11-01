# Test strategy

Alephgrid holds people's data, so reliability is a feature. The approach borrows from
[SQLite's testing](https://sqlite.org/testing.html): several independent layers, boundary
values, fault injection, randomized operation sequences checked against a simple model,
and recovery checks. It does not claim SQLite's level of assurance. Coverage numbers and
test counts are indicators, not proof.

The per-task evidence lives in [TEST_MATRIX.md](TEST_MATRIX.md).

## Rules for every change

Before implementing a behavior, write down:

1. **Contract**: what a user or client can observe.
2. **Invariants**: what must hold after every operation, including failed ones.
3. **Scenarios**: normal, boundary, invalid, concurrent and failure cases.
4. **Oracle**: an independent way to know the right answer, such as a closed-form value, a
   reference model, a second code path that does not share the logic under test, or hand-checked data.
5. **Acceptance tests**: written before the item is checked off.

When fixing a confirmed bug, add a regression test that fails before the fix whenever that is
feasible. Phase 1's tests were run against the previous commit's code, and 24 of them failed there.

Assert values, persisted state and side effects, not only status codes. Use deterministic
fixtures, explicit seeds, isolated temporary directories and ports. Wait on observable
state (`eventually(...)`, `expect.poll`), never on fixed sleeps. Do not retry flaky tests
to make them pass. Find the cause, or quarantine the test with a written reason and a follow-up.

## Layers

| Layer | Where | What it covers |
|---|---|---|
| A. Unit | `#[cfg(test)]` modules in `crates/*/src` (`wire.rs`, `sql.rs`, `formula.rs`) | Pure transformations: exact-value encoding, input validation, SQL lexing, formula reference mapping |
| B. Integration | `crates/alephgrid-core/src/tests/*` (real DuckDB and IronCalc, file-backed workbooks); `crates/alephgrid-server/tests/api.rs` (real HTTP and WebSocket against an in-process server) | Persistence, import/export, SQL expansion, sheets, deletion lifecycle, uploads |
| C. Browser end to end | `web/e2e/*.spec.ts` (Playwright with the installed Chrome, real `alephgrid` binary) | User journeys through the UI. Checks on-screen results (formula bar, toasts, status bar) and persisted state (exports, after restart or crash) |
| D. Property / state machine | `crates/alephgrid-core/src/tests/props.rs` (proptest) | Random edit, insert, delete, reopen and sort sequences compared after every step with a `Vec` reference model. Failing seeds are shrunk and saved in `proptest-regressions/` |
| E. Differential / metamorphic | `tests/sql.rs`, `tests/precision.rs`, `tests/sheets.rs`, `tests/journey.rs` | Formula columns vs the same expression written inline; closed-form sums; Parquet round trip compared with `EXCEPT`; xlsx export→import equality; export before and after restart |
| F. Fault injection and recovery | `faults::fail_next(point)` in the engine; raw-TCP interrupted uploads; `tests/crash.rs` (SIGKILL of the real binary) | Failed saves, failed mutations, interrupted uploads, process kill during writes |
| G. Fuzz | Not yet implemented, apart from the proptest generators over formula translation and table-name sanitizing. Planned targets: WebSocket frame parsing, CSV/xlsx import adapters, formula translation | — |
| H. Performance / resource | `docs/bench.mjs` (manual); phase 3 resource tests (`runtime.rs` budget sums read back from DuckDB, `scheduling.rs` bounded queues, replies and outboxes) | No performance regression gates yet (phase 9) |

### Oracles and their limits

- Comparing two paths through DuckDB is not fully independent. Where results come from DuckDB,
  tests also use hand-computed values (sums over `0..n`, region totals) or a reference model in Rust.
- Spreadsheet values follow Excel semantics: IEEE doubles with 15 significant digits. SQL
  values follow DuckDB semantics. Numbers that a double cannot hold exactly are carried as
  text when they move into a sheet. This is a documented difference, not a bug.
- NULL vs blank: an empty table cell is SQL NULL; an empty sheet cell has no value; a sheet
  converted to a table maps blanks to NULL.

## Fault simulation scope

- **Simulated:** application-level failures at named points (engine: `save_sheets`, `edit`,
  `rename_column`, `drop_column`, `sheet_to_table`, `history`, `commit`, `migrate`, `backup`; server:
  `write_durably`, `upload_write`, `open_workbook`, and start-stage hooks), dropped client
  connections mid-upload, clients that disappear or never read, `SIGKILL` of the server during and
  between writes, `SIGTERM` during edits and imports, a **genuinely full filesystem**
  (`storage_faults.rs` on a 64 MB disk image or tmpfs named by `ALEPHGRID_SMALL_FS`), read-only
  workbook and data directories, corrupt database files, damaged backups, and a lock held by
  another process.
- **Not simulated:** power loss, `fsync` failures, torn or reordered writes, media errors,
  network filesystems. Process termination keeps the OS page cache, so the crash tests say nothing
  about durability on power loss. That needs a VM or `dm-flakey`-style environment and remains
  open (9.04). See [DURABILITY.md](DURABILITY.md).

## Mutation testing

A targeted mutation script (`python3 tools/mutants.py`, run by hand) disables one safeguard at a time:
exact-input validation, the empty-batch no-op, last-write-wins, reload after failure,
coordinate validation, grouped undo, the dependency check, rename rewriting, WITH merging and
typed conversion. All 10 mutants are killed by the engine suite. Two initially survived because a
second safeguard masked them. The tests were then strengthened, and one of them exposed a real
defect: undo history was lost after rejected requests.

## Tiers

| Tier | Runs | Contents |
|---|---|---|
| Per change (CircleCI `check`) | on request, before merging (`tools/ci.sh check`); locally before every PR | web build + type check; `cargo test --workspace` (unit, integration, property with 96 cases, protocol drift, crash, disk-full); clippy with `-D warnings`; Playwright journeys in Chromium |
| Extended (CircleCI `full`) | on request, before a release or after storage, collaboration or recovery changes (`tools/ci.sh full`) | The check, plus Firefox and WebKit journeys; property tests with 2000 cases; server suites repeated 5× to surface flakiness |
| Release | manual, before tagging | Everything above, plus a clean install, an upgrade/restore and authorization checks once those exist (phases 3, 7, 9). Not yet defined in automation |

"Test implemented" and "test executed successfully" are recorded separately in the matrix.
Local runs do not count as extended or release checks having passed. CI never runs on its own; see [CI.md](CI.md).

## Commands

```bash
cargo test --workspace                                      # A, B, D, E, F (fast tier)
PROPTEST_CASES=2000 cargo test -p alephgrid-core --lib props   # longer D run
UPDATE_BINDINGS=1 cargo test -p alephgrid-server --test protocol # regenerate web/src/protocol.ts
cargo clippy --workspace --all-targets -- -D warnings
(cd web && npm run build)                                   # type check + bundle
cargo build -p alephgrid-server && (cd web && npm run e2e)  # C (needs Google Chrome)
ALEPHGRID_SMALL_FS=/path/to/small/fs cargo test -p alephgrid-server --test storage_faults  # real disk full
```
