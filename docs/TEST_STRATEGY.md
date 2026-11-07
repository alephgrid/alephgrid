# Test strategy

Alephgrid holds people's data, so reliability is a feature. The approach borrows from
[SQLite's testing](https://sqlite.org/testing.html): several independent layers, boundary
values, fault injection, randomized operation sequences checked against a simple model,
and recovery checks. It does not claim SQLite's level of assurance. Coverage numbers and
test counts are indicators, not proof.

Per-task evidence lives beside each task in the
[implementation checklist](IMPLEMENTATION_CHECKLIST.md); for phases 1–8 and F the contracts
and recorded results are also in [TEST_MATRIX.md](TEST_MATRIX.md).

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
| G. Fuzz | `crates/alephgrid-core/src/tests/fuzz.rs` (bounded proptest targets, stable Rust) | The table-formula parser and the functions built on it, the sheet formula checker, CSV import (quoted round trip, arbitrary bytes) and cells pasted from other apps. Bounded by `FUZZ_CASES` and `FUZZ_SECONDS`; failures are shrunk and kept in `proptest-regressions/`. Not covered yet: WebSocket frame parsing, xlsx import, the browser's clipboard parsing |
| H. Performance / resource | `tools/bench/bench.mjs` (seeded datasets, 1/5/20 clients over the public protocol, p50/p95/p99 and RSS; run by hand, [OPERATIONS.md](OPERATIONS.md#benchmarks)); phase 3 resource tests (`runtime.rs` budget sums read back from DuckDB, `scheduling.rs` bounded queues, replies and outboxes) | No performance regression gates yet (phase 9) |

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

A targeted mutation script (`python3 tools/mutants.py`, nightly and release tiers) disables one
safeguard at a time: exact-input validation, the empty-batch no-op, last-write-wins, reload after
failure, coordinate validation, grouped undo, the dependency check, rename rewriting, WITH merging
and typed conversion. Two initially survived because a second safeguard masked them. The tests
were then strengthened, and one of them exposed a real defect: undo history was lost after
rejected requests. On 7 Nov four mutants no longer applied (the code had moved) and were
re-pointed; each search text must now occur once. All ten are killed (the empty-batch
mutant since an empty edit stopped committing a revision).

## Tiers

CI never runs on a push (owner decision, 9.16): the PR and release tiers start on request,
the nightly tier on a CircleCI schedule the owner turns on ([CI.md](CI.md)). Every tier fails
on any failing test; nothing is retried or allowed to fail.

| Tier | Runs | Contents |
|---|---|---|
| PR (CircleCI `pr`, `run_check`) | on request before merging (`tools/ci.sh check`); locally before every PR | web build + type check; `cargo test --workspace` (unit, integration, protocol drift, authorization, reconnect, crash, backup/restore, disk-full; property and fuzz targets at 96 and 64 cases); clippy with `-D warnings`; licence notices; the test gate (`tools/test_gate.py`); Playwright journeys in Chromium |
| Nightly (CircleCI `nightly`, `run_nightly` or the schedule) | nightly once the owner adds the schedule; on request after storage, collaboration or recovery changes (`tools/ci.sh nightly`) | The PR tier, plus Firefox and WebKit journeys; install check (`tools/release/smoke.sh` on the built binary); property tests with 2,000 cases; fuzz targets up to 200,000 cases or 5 minutes each; server suites repeated 5× to surface flakiness; targeted mutation testing (`tools/mutants.py`) |
| Release (CircleCI `release`, on a version tag) | `tools/ci.sh release vX.Y.Z` | Everything in nightly on the tagged commit, plus each platform binary built and smoke-tested, then the artifact check (`tools/release/verify.sh`: every archive and SBOM present and matching its checksum, archives hold the binary and the notices of the tagged source, the Linux x86_64 binary reports the version) before the draft release. A clean-machine install, upgrade/restore and authorization pass is still manual (9.T01, 9.T03) |

"Test implemented" and "test executed successfully" are recorded separately: a check
listed here is implemented; [CI.md](CI.md#what-has-actually-run) records which tiers have
actually run. Local runs do not count as nightly or release checks having passed.

## Skipped and quarantined tests

`tools/test_gate.py` (PR tier) fails when a test is `#[ignore]`d or skipped without a row
here, or a row names nothing that exists. `quarantined` rows are tests taken out because
they fail or flake; each needs an owner and a follow-up.

| Kind | Where | Test or skip reason | Reason | Owner | Follow-up |
|---|---|---|---|---|---|
| benchmark | `crates/alephgrid-core/src/tests/table_formulas.rs` | `lookup_read_timings` | Opt-in timing run (F.06/F.T04), release build and a large table | maintainers | run by hand with `--ignored` |
| benchmark | `crates/alephgrid-core/src/tests/table_formulas.rs` | `analyst_benchmark` | Opt-in F.T04 benchmark against its budgets | maintainers | run by hand with `--ignored` |
| benchmark | `crates/alephgrid-server/tests/python.rs` | `bench_a_million_rows_with_100k_distinct_values` | Opt-in 8.06 timing run over a million rows | maintainers | run by hand with `--ignored` |
| generator | `crates/alephgrid-core/src/tests/compat.rs` | `write_sample_workbook` | Writes `docs/samples/analyst-sample.xlsx`; not a check | maintainers | — |

## Commands

```bash
cargo test --workspace                                      # A, B, D, E, F (fast tier)
PROPTEST_CASES=2000 cargo test -p alephgrid-core --lib props   # longer D run
FUZZ_CASES=200000 FUZZ_SECONDS=300 cargo test -p alephgrid-core --lib tests::fuzz -- --nocapture  # longer G run
python3 tools/test_gate.py                                  # skips listed, required suites present
UPDATE_BINDINGS=1 cargo test -p alephgrid-server --test protocol # regenerate web/src/protocol.ts
cargo clippy --workspace --all-targets -- -D warnings
(cd web && npm run build)                                   # type check + bundle
cargo build -p alephgrid-server && (cd web && npm run e2e)  # C (needs Google Chrome)
ALEPHGRID_SMALL_FS=/path/to/small/fs cargo test -p alephgrid-server --test storage_faults  # real disk full
```
