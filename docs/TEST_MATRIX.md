# Test matrix

Maps checklist task IDs ([IMPLEMENTATION_CHECKLIST.md](IMPLEMENTATION_CHECKLIST.md)) to their behavior, tests and latest
results. Layers A–H are defined in [TEST_STRATEGY.md](TEST_STRATEGY.md). Paths are relative to the repository root.
`E` = `crates/alephgrid-core/src/tests/`, `S` = `crates/alephgrid-server/tests/`, `W` = `web/e2e/`.

**Phase 1 verification (2026-10-26, macOS arm64, local).** CI on PR #1 has since run `cargo test --workspace` on Linux: all tests passed; the run then failed only because clippy was missing from the pinned toolchain (fixed in `ec28345`).

| Command | Result |
|---|---|
| `cargo test --workspace` | engine 61 passed; server api 9, journey 2, crash 2, protocol 2 passed |
| `PROPTEST_CASES=400 cargo test -p alephgrid-core --lib props` | 3 passed (188 s) |
| `cargo test -p alephgrid-server` ×3 | passed every time |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cd web && npm run build` | type check + bundle passed |
| `cd web && npx playwright test` ×4 | 4 passed every time (Chrome 1.64 runner, headless) |
| `python3 tools/mutants.py` | 10/10 mutants killed |
| New phase-1 engine tests run against the pre-fix commit `ff3e736` | 24 failed (expected) |

Not executed: scheduled 2000-case run (`scheduled.yml`), any CI run, Linux/Windows platforms, power-loss or disk-full tests.

## Phase 1

### 1.01 Tagged import response in both upload flows
- **Behavior:** `POST /import` returns `{kind:"table", table}` or `{kind:"sheets", sheets}`. Home and workbook uploads open what was imported.
- **Invariants:** the shape is fixed by the generated `Imported` type; a failed import returns 4xx with a message and creates nothing.
- **Tests:** B `S/api.rs::import_returns_tagged_table_or_sheets` (CSV and xlsx shapes, the evaluated xlsx formula, a bad file, a path-stripped name, a hidden name). C `W/journey.spec.ts` "table journey" (home upload opens the table, success toast).
- **Result:** pass. **Gaps:** the in-workbook "Import" menu path is not driven by the browser test (it shares `uploadFile` and the `Imported` handling, which the type check covers).

### 1.02 Full sheet export, no 5,000-row truncation
- **Behavior:** `sheet_to_table` reads every used row with typed columns (BIGINT/DOUBLE/BOOLEAN/DATE/TIMESTAMP/VARCHAR) and keeps sheet order.
- **Invariants:** row count = used rows − header; values equal the cells; text stays text (leading zeros); blank rows are kept as NULL rows.
- **Tests:** B/E `E/sheets.rs::sheet_to_table_keeps_every_row_around_the_fetch_cap` (1, 4,999, 5,000, 5,001, 20,000 rows; closed-form sums as oracle), `…infers_types_from_cell_values`, `…keeps_dates_as_dates`, `…requires_header_and_data`; `S/journey.rs::sheet_journey…`.
- **Result:** pass; fails on `ff3e736` (5,000 cap, and a temp-CSV path collision between concurrent conversions).
- **Gaps:** cells with time-only formats are not tested (they would be inferred as dates/timestamps). Used ranges above 20M cells are refused rather than streamed.

### 1.03 Atomic append + edit; empty batches are no-ops
- **Behavior:** an edit batch (including rows appended by typing below the end) commits fully or not at all. Values are validated before any change. A repeated cell takes its last value.
- **Invariants:** after a failed edit, rows and values are unchanged in memory and on disk; row ids stay dense.
- **Tests:** B `E/integrity.rs` (empty batch on empty/full table, failed append, unknown/formula column, sorted-view append, last write wins, sorted-view record mapping, NULL clears, Unicode/quotes after reopen). D `E/props.rs::table_matches_reference_model` (edit/insert/delete/reopen/sort sequences vs a `Vec` model; regression seed saved). C "table journey" (a rejected value shows an error toast; the export is unchanged).
- **Result:** pass; fails on `ff3e736` (an empty batch appended a row; a failed append left 4 rows).

### 1.04 Atomic sheet bulk mutations
- **Behavior:** a sheet batch is validated (sheet exists, row < 1,048,576, column < 16,384) before any change. A failure part-way or a failed save reloads the model from the file. One batch is one undo step.
- **Invariants:** memory equals the last durable state after any failure; rejected requests keep undo history; undo/redo recalculate.
- **Tests:** B/F `E/sheets.rs::invalid_cell_in_a_batch_changes_nothing`, `failed_save_rolls_memory_back_to_disk` (fault point `save_sheets`), `undo_reverts_a_whole_batch_and_redo_reapplies_it`, `recalculation_after_undo_redo_and_reopen`, `structural_edits_are_single_undo_steps`. B `S/journey.rs` (a rejected batch over the wire). C "sheet journey" (formula, undo, SIGKILL, reopen).
- **Result:** pass. **Gaps:** undo history is in memory only, shared by all users of a workbook, and lost on restart or after a reload (documented in ARCHITECTURE.md).

### 1.05 Exact decimals and large integers
- **Behavior:** integers within ±(2^53−1) travel as JSON numbers; larger integers and every DECIMAL travel as exact strings. Lossy edits are rejected (`1.5`→INTEGER, `1.234`→DECIMAL(10,2), overflow). Stats sums are exact. TIMESTAMPTZ is shown in UTC. Query→sheet keeps numbers over 15 significant digits as text. The UI formats exact strings without converting them to doubles.
- **Invariants:** no value changes between storage, wire, export and re-import.
- **Tests:** A `wire.rs` unit tests (2^53 boundary, decimal scales, input grammar). B `E/precision.rs` (fetch, edits, type boundaries, dates/leap years, query results, stats, CSV/Parquet/JSON export; Parquet round trip compared with `EXCEPT`). `E/sheets.rs::query_to_sheet_is_bounded_and_keeps_text_exact`. `S/journey.rs` (9007199254740993 through CSV import, edit and export). The TypeScript compiler, via the generated `Stats` type, caught the UI's number-only stats formatting.
- **Result:** pass; fails on `ff3e736`.
- **Gaps:** copy to the clipboard uses the exact wire text but is not browser-tested. Values inside sheets follow Excel's double semantics by design. JSON exports carry exact digits, but consumers may parse them as doubles.

### 1.06 Dependency-aware column rename/delete
- **Behavior:** formulas are parsed by DuckDB (`json_serialize_sql`) and must be a single expression with no subqueries or extra statements. Renaming a column rewrites the references to it at the exact parse locations, keeping the user's text and `[Name]` style, in one transaction. Deleting a referenced column is refused before any change, naming its dependents.
- **Invariants:** after any rename/delete every formula still evaluates; failures change nothing.
- **Tests:** A `formula.rs` unit tests. B `E/formulas.rs` (data and formula column renames, Unicode byte offsets, string literals untouched, refused deletes with the dependent named, case-only rename, subquery and injection rejection, cycles, table rename, reopen persistence). Fault points `rename_column` and `drop_column` exist; no test arms them yet (the transaction path is covered through validation failures).
- **Result:** pass; fails on `ff3e736`.

### 1.07 Formula columns in `WITH` queries
- **Behavior:** formula-column CTEs are prepended to user `WITH` lists (leading comments and `RECURSIVE` handled). A user CTE named like a table wins. Result columns are addressed by position.
- **Tests:** A `sql.rs::leading_with_detection`. B/E `E/sql.rs` (plain/FROM-first/lowercase/semicolon queries, CTE forms, shadowing, joins + nested subquery with a hand-computed oracle, materialization into a table and a sheet, malformed SQL, truncation, hidden `_rf_id`).
- **Result:** pass; fails on `ff3e736`.

### 1.08 Unique upload paths; cleanup of interrupted imports
- **Behavior:** every upload streams into its own `.uploads/<random>/` directory, removed after import, after failure, or when the client disconnects. `.uploads`/`.exports` are cleared at startup. Exports use private scratch directories. Download headers are safe for any file name.
- **Invariants:** concurrent same-name uploads never mix; an interrupted or failed import leaves no file and no table.
- **Tests:** B `S/api.rs::concurrent_same_name_uploads_do_not_collide` (4 parallel uploads, per-tag counts), `interrupted_upload_leaves_no_file_and_no_table` (raw TCP, promised 1 MB, sent 100 B), `startup_removes_stale_scratch_files_but_not_data`, `import_returns…` (a CSV failing after 300k rows leaves no table), `unicode_names_download_safely` (this found a header panic on non-ASCII names).
- **Result:** pass. **Gaps:** disk-full during upload is not simulated; there is no upload size limit yet (3.02).

### 1.09 Deletion lifecycle and trash
- **Behavior:** delete marks the workbook deleted, so new operations fail. It waits for the running operation, checkpoints, closes DuckDB and moves the directory to `.trash/` with one rename. Connected clients get `workbook_deleted` and the server ends their session. Trash list/restore/purge/empty exist in the API and the home screen.
- **Invariants:** no operation runs on a closed database; the workbook cannot be reopened while closing; restore returns identical data; nothing is removed permanently except by purge/empty.
- **Tests:** B `S/api.rs::delete_disconnects_clients_moves_to_trash_and_restores`, `delete_waits_for_the_running_operation` (concurrent 3M-row write: either complete or refused, never torn), `empty_trash_removes_everything_and_restore_survives_restart`. C `W/journey.spec.ts` "trash" and "a workbook deleted elsewhere closes…".
- **Result:** pass. These tests found that a server-closed session waited for the client's close acknowledgement, keeping the workbook referenced; fixed.
- **Gaps:** no automatic trash retention or purge (by design for now); no quota on trash size.

### 1.10 Generated protocol types and contract tests
- **Behavior:** `web/src/protocol.ts` is generated from the Rust types (ts-rs), including every request (`Op`), event, workbook type and an op→result map. `rpc.call` is typed by it.
- **Invariants:** a Rust protocol change without regenerating fails CI; every op has a declared result type; every op's real reply round-trips through its declared type unchanged.
- **Tests:** `S/protocol.rs::protocol_bindings_are_up_to_date`, `every_op_has_a_result_type`; `S/api.rs::every_operation_replies_with_its_declared_type` (all 34 ops, malformed frames); `tsc` over the frontend.
- **Result:** pass.

### 1.11 Gate: import → edit → filter → export → restart → reopen
- **Tests:** B/E `S/journey.rs::table_journey_survives_restart_exactly` (a model maintained independently in the test; failed edits in between; formula column; filtered + sorted export; byte-identical export before and after restart) and `sheet_journey_survives_restart_and_xlsx_roundtrip`. F `S/crash.rs::acknowledged_changes_survive_sigkill` (an unacknowledged 2-cell edit is all-or-nothing) and `kill_during_a_large_write_leaves_the_old_or_new_state`. C `W/journey.spec.ts` (the same journey through the UI with a SIGTERM restart, plus the sheet journey with SIGKILL).
- **Result:** pass.
- **Gaps:** no two-user variant yet (needs revisions, 7.08). Crash tests cover process kill, not power loss. `kill_during_a_large_write…` cannot prove the kill landed mid-write on every run; it checks the invariant either way.

## Phase 2

**Latest verification (2026-10-26, macOS arm64, local, branch `phase-2-local-spreadsheet`):** `cargo test --workspace` engine 81 passed; server api 9, crash 2, imports 5, journey 2, protocol 4 passed; clippy clean; `npx playwright test` 20 passed in two consecutive runs; `cargo test -p alephgrid-server --test api` 6 consecutive runs passed. CI has not run on this branch.

### 2.01 Sheet tabs, rendering, editing, change events
- **Behavior:** sheets open as tabs, render IronCalc's display values (errors included), edit in the grid and formula bar, and other people's edits appear without reloading.
- **Tests:** C `W/sheets.spec.ts` "blank sheet in a new workbook…": `#DIV/0!` and recalculated values read back from the grid's accessible active cell; a second browser page sees both directions of edits live. B `S/api.rs` contract test covers every sheet op.
- **Fix found:** Enter in the cell editor bubbled to the grid and reopened the editor below (`W/journey.spec.ts` "typing a value and pressing Enter…", fails without the fix).

### 2.02 Blank sheet in a new workbook
- **Tests:** C `W/sheets.spec.ts` (empty-state "New blank sheet", typing, formulas) and the 2.10 gate (menu path).

### 2.03 XLSX import/export with compatibility notes
- **Behavior:** before import, the dialog lists sheets and what will not come across (charts, pivots, drawings, macros, external links, connections, comments, slicers, #NAME? cells with example function names). Export says it contains sheets only.
- **Tests:** B `E/xlsx.rs` (round trip compared cell by cell with the source model, two generations; copy-mode import; injected package parts; unknown functions; corrupt files). C `W/import.spec.ts` "xlsx import lists sheets and compatibility notes…".
- **Fix found:** importing next to a same-named sheet renamed the copy but left its formulas pointing at the old sheet (silently wrong values).
- **Gaps:** notes are based on package parts; files from real Excel with those features were not available, so parts are injected into an IronCalc-written file.

### 2.04 Undo/redo with recalculation and reopen
- **Tests:** C `W/sheets.spec.ts` "undo and redo recalculate…" (⌘Z, ⇧⌘Z, toolbar button, reload). B `E/sheets.rs` grouped undo and recalculation tests.
- **Gap:** history is in memory and shared by all editors of a workbook (documented).

### 2.05 Import preview, type overrides, progress, actionable errors
- **Behavior:** staged upload → preview (detected types, first rows) → per-column types → import with DuckDB progress and Stop → errors with line/column/value and a one-click fix that retries the same upload. Unclaimed uploads expire after an hour.
- **Invariants:** a preview creates nothing; failed or cancelled imports create no table; uploads are private to their workbook; only the named job is interrupted.
- **Tests:** A `import.rs` (error explanation, type allow-list). B `E/import.rs` (preview, overrides for CSV/Parquet/JSON, late bad value beyond the 100k sample, invalid overrides, xlsx preview, progress reported during a 2M-row import). B `S/imports.rs` (staging, retry after a problem, workbook isolation, cancel, progress + interrupt of a 6M-row import, expiry). C `W/import.spec.ts` (preview → override → import; error → fix button; cancel at preview; Stop during a 4M-row import and retry).

### 2.06 Tables vs Sheets; bounded conversions with visible limits
- **Behavior:** a Table/Sheet badge explains each kind; copying a query or a table view into a sheet is capped by `--sheet-copy-rows` (default 100,000) and by the cell budget, and the result reports rows copied, source size and truncation.
- **Tests:** B `E/sheets.rs::copies_into_sheets_are_bounded_and_say_so`; contract test for `table_to_sheet`; C `W/kinds.spec.ts`. Measured: 100,000 rows × 5 columns copy in 0.31 s (release) / 1.9 s (debug).
- **Fix found:** optional fields were serialized as `null` where the TypeScript types promise `undefined` (crashed a toast); guarded by `S/protocol.rs::optional_fields_are_omitted_not_null`.

### 2.07 Named views (live) and snapshots
- **Behavior:** saved views per table (sort, filters, hidden columns, widths) in the workbook file; live; snapshot = explicit copy into a new table.
- **Invariants:** saved and open views stay valid across column/table renames; a column a saved view sorts or filters by cannot be dropped; dropping a table removes its views.
- **Tests:** B `E/views.rs` (persist + live after edits, unique names, invalid specs, hidden columns in fetch/export/copies, snapshot independence and order, schema changes on saved and open views). Contract test for all view ops; `S/protocol.rs::no_op_parameter_is_named_id`. C `W/views.spec.ts` (filter + hide, save, reload, re-apply, snapshot export).
- **Fixes found:** open views kept stale column names after rename/drop and failed after commit; concurrent metadata writes shared one temporary file (a delete could fail); an op parameter named `id` collided with the request id.

### 2.08 Grid disposal
- **Tests:** C `W/disposal.spec.ts` counts window/document/media-query listeners, ResizeObservers and open WebSockets across six open/leave cycles with menus. The pre-fix code leaked 18 listeners and 6 observers.

### 2.09 Accessible grid, keyboard navigation, visible focus
- **Tests:** C `W/a11y.spec.ts`: axe (no serious/critical violations on home, workbook, dialog, sheet; it caught badge contrast), ARIA grid with active cell and announcements, visible focus ring, Tab leaving the grid, keyboard menus and tabs, loading a workbook does not take focus away from a control the user moved to (found on CI: the menu test lost focus when the table finished opening). **Gap:** no screen-reader testing with real assistive technology.

### 2.10 Gate
- **Tests:** C `W/sheets.spec.ts` "gate: a new user creates, imports, edits and reopens a table and a sheet with the UI alone" (new workbook → import dialog → table edit with append → blank sheet with formulas → home and back → server restart → values intact), plus the phase-1 journeys.

## Phase 3

Contracts below were written before implementation (TEST_STRATEGY "Rules for every change"). Each task
lists its observable behavior, invariants, scenarios and oracle; **Tests** and **Result** are filled in
as the work is verified. Nothing here is checked off until its tests have run and passed.

### Shared decisions
- **Limits are enforced where they are named.** DuckDB `memory_limit` bounds DuckDB's buffer manager per
  workbook instance, not the process's resident memory; `threads` bounds DuckDB worker threads per
  instance; `max_temp_directory_size` bounds DuckDB spill files. Alephgrid's own accounting covers the sum
  across open workbooks, staged uploads and an estimate of sheet memory. Nothing in phase 3 is an OS-level
  (cgroup/rlimit) limit, and nothing is a security boundary between workbooks.
- **Local provider = in-process allocations.** A local allocation is a workbook engine opened inside the
  server process. Its exclusivity comes from an OS file lock (`flock`) on the workbook directory, held for
  the allocation's lifetime. That is enforceable on one host and a local filesystem only.

### 3.01 Bounded job queues, cancellation, deadlines, priority
- **Behavior:** every workbook request becomes a job with a class (interactive: fetches, view info, sheet
  reads, small edits; bulk: imports, queries, sorts, copies, exports), a deadline and a client request id.
  Interactive reads run on reader connections and do not wait behind a running bulk job. Writes run one at
  a time. Within a class, sessions are served round-robin. A full queue rejects new jobs with code
  `overloaded`. `cancel {request}` removes a queued job or interrupts the running one; a job past its
  deadline fails with `deadline_exceeded` (queued) or is interrupted (running). Closing a session cancels
  its queued jobs.
- **Invariants:** queued jobs per workbook and per session never exceed their bounds; a cancelled or expired
  job changes nothing (or, for a write that already committed, reports success); cancelling job A never
  interrupts job B; every accepted request gets exactly one reply.
- **Scenarios:** saturation from one session and from many; one flooding session vs a light one (the light
  one still progresses); a fetch during a long import or sort; cancel before start, while running, after
  completion (no-op), of someone else's request id (no effect); deadline while queued and while running;
  disconnect with queued work.
- **Oracle:** per-job timestamps and outcomes recorded by the test; table contents checked with SQL after
  cancellations; reply ids counted against request ids.

### 3.02 Bounded uploads, requests, responses, pasted cells, dimensions
- **Behavior:** limits on upload bytes (checked against `Content-Length` before reading and while
  streaming), WebSocket message bytes, cells per edit/paste, rows × columns per fetch, response bytes per
  reply, rows per insert, query preview rows. Oversized requests fail with `too_large` and a message naming
  the limit; dimensions are clamped before any allocation, with overflow-safe arithmetic.
- **Invariants:** no request allocates in proportion to an unchecked client number; a rejected upload leaves
  no file; responses never exceed the byte bound (a fetch returns fewer rows and says so).
- **Scenarios:** `row_start = u64::MAX`, `row_count = u64::MAX`, `column_count = u32::MAX`, start + count
  overflow; exactly-at-limit and limit+1 uploads, with and without Content-Length; 1 MB text cells; wide
  tables (500 columns).
- **Oracle:** closed-form expected row counts; byte length of the serialized reply; directory listings.

### 3.03 Host-wide resource budgets
- **Behavior:** a host budget (memory, threads, temporary disk) is shared by every open workbook. Opening a
  workbook reserves a share; DuckDB settings of every open workbook are rebalanced so their sum stays
  within the budget. Staged uploads reserve temporary disk. When no share is left, idle workbooks are
  evicted, otherwise the open fails with `capacity` (HTTP 503).
- **Invariants:** at every observable point, Σ memory_limit ≤ memory budget, Σ threads ≤ thread budget,
  Σ (temp limits + staged upload bytes) ≤ temp budget.
- **Scenarios:** 1, 2, N workbooks; budget for exactly N; N+1 with and without an idle workbook; uploads
  exceeding the temp budget; concurrent opens.
- **Oracle:** `current_setting('memory_limit'|'threads'|'max_temp_directory_size')` read from each DuckDB
  instance, summed by the test.

### 3.04 Idle eviction and view expiry
- **Behavior:** a workbook with no sessions and no jobs for `--idle-timeout` is drained (checkpointed) and
  closed; its data stays. Permutations of views unused for `--view-ttl` are dropped and rebuilt
  transparently on next use. Views per session and per workbook are bounded.
- **Invariants:** eviction never closes a workbook that has a session or a queued/running job; work arriving
  during eviction either runs on the old allocation or waits for a new one, never on a closed one.
- **Scenarios:** eviction racing new connections and HTTP requests (repeated); view expiry then fetch (same
  rows); many open views.
- **Oracle:** fetched rows equal before and after expiry/eviction; allocation state history.

### 3.05 Visible column ranges and a byte-bounded client cache
- **Behavior:** `fetch` takes an optional column range; the client requests only visible columns (plus
  margin), and its cache is bounded by an estimate of bytes over fresh and stale blocks together.
- **Invariants:** cache bytes ≤ bound after every insertion; visible blocks are never evicted for invisible
  ones; values for a column range equal the corresponding slice of a full fetch.
- **Scenarios:** 500-column table; very long text cells; scrolling sideways; refresh with stale blocks.
- **Oracle:** full-width fetch sliced by the test; byte estimate recomputed independently.

### 3.06 Stable record ids separate from display order
- **Behavior:** each row has an immutable `_rf_id` (never reused within its table) and an ordering key
  `_rf_ord`. Inserting or deleting rows changes neither other rows' ids nor their stored data; positional
  access uses arithmetic for untouched (dense) tables and a chunked order index otherwise. Fetches return
  record ids, and edits may address a record instead of a position, so an edit lands on the intended row
  even after someone else inserted rows above it.
- **Invariants:** display order = order of `_rf_ord`; ids unique and stable; no tail renumbering of the base
  table on insert/delete (only bounded local rebalancing of ordering keys when a gap is exhausted).
- **Scenarios:** random edit/insert/delete/sort/reopen sequences; repeated inserts at one position (gap
  exhaustion); deletes of first/last/middle; migrated v1 tables.
- **Oracle:** `props.rs` reference model extended with record ids (a `Vec<(id, row)>`).

### 3.07 Revisions, idempotent mutation ids, operation history
- **Behavior:** each workbook has a revision, incremented by exactly one per committed mutation in the
  same transaction as the change, together with a history entry (revision, mutation id, op, session, time,
  bounded summary). A mutation carrying a `mutation_id` that is already in the history is not applied
  again; the reply repeats the original revision and result with `replayed: true`. Replies and change
  events carry the revision.
- **Invariants:** revisions are strictly increasing with no gaps; failed mutations do not change the
  revision or the history; history and data never disagree after a crash.
- **Scenarios:** duplicate and retried mutation ids (including after reconnect and restart), concurrent
  sessions, injected failure after the change and before the history write, kill during commit.
- **Oracle:** history rows vs the test's own log of acknowledged mutations; data checked with SQL.

### 3.08 Versioned migrations; consistent backup and restore
- **Behavior:** workbooks record a storage format version. Opening an older workbook migrates it in one
  transaction after keeping a copy of the original file; opening a newer one is refused. Backups copy every
  workbook from a transaction snapshot (writers keep working), with `workbook.json`, workspaces and a
  manifest with checksums. Restore verifies and only writes into an empty data directory.
- **Invariants:** a migration is all-or-nothing; a backup contains each workbook exactly as of one
  revision; restore yields identical data and metadata or fails without writing.
- **Rollback limits (to document):** no downgrade path; the pre-migration copy is the only rollback.
- **Scenarios:** the committed v1 fixture written by the phase-2 code; a crash during migration; a newer
  version; backup during a stream of edits; restore onto a clean directory; corrupted backup; non-empty
  target.
- **Oracle:** v1 fixture contents listed in its README; restored data compared with `EXCEPT` and history.

### 3.09 Graceful shutdown on SIGTERM
- **Behavior:** stop admitting (new requests get `shutting_down`), let running and queued work finish
  within `--shutdown-grace`, interrupt what remains, checkpoint, release locks, record allocations as
  stopped, exit 0.
- **Invariants:** every acknowledged mutation survives; unacknowledged ones are absent or complete; no
  partial imports.
- **Scenarios:** SIGTERM idle, during an import, during a stream of edits, twice.
- **Oracle:** the test's log of acknowledged mutations vs data and history after restart.

### 3.11–3.14 Runtime objects, provider contract, Local provider, lifecycle
- **Behavior:** `RuntimeSpec` (immutable, content-addressed: engine version, storage format, components,
  required capabilities), `ComputePolicy` (resources, isolation class, idle timeout, providers),
  `RuntimeAllocation` (state, operation id, provider, reservation, failure) and `OwnershipAssignment`
  (workbook, allocation, epoch). Workbooks pin a spec; their identity and data do not depend on any
  allocation. `RuntimeProvider` offers idempotent ensure, status, drain, stop, capability reporting and
  typed failures. The Local provider persists allocation records, reconciles them at startup and
  distinguishes requested → starting → running → ready → draining → stopped/failed.
- **Invariants:** at most one live allocation per workbook; duplicate ensure/stop with the same or
  different operation ids never create a second allocation; failed startup releases its reservation, lock
  and scratch files; after a controller restart no record claims a live allocation that does not exist.
- **Scenarios:** duplicate starts (concurrent), stop twice, cancel before ready, startup failure (corrupt
  file, injected fault), incompatible spec (newer storage format, missing capability), controller restart
  with stale records and orphaned scratch, data retained after stop.
- **Oracle:** allocation records on disk, lock state probed from a second file handle, reservations summed.

### 3.15 Durability contract and isolation classes
- **Deliverable:** docs/DURABILITY.md: when an edit is acknowledged, what survives a process crash, a host
  crash/power loss and storage loss, which of those are tested, and the trust classes (same-process,
  separate process, container, VM) with what each does and does not isolate.

### Results (2026-10-26, macOS arm64, local, branch `phase-3-bounded-execution`)

| Task | Tests | Result | Found and fixed / gaps |
|---|---|---|---|
| 3.01 | A `sched.rs` unit (5: write order, reserved reader, round-robin + aging, bounds, withdraw/interrupt); B `S/scheduling.rs` reads during a long change + targeted cancel, deadlines queued and running, saturation (31 accepted, 9 `overloaded`, one reply each, revisions in order), fairness (the light session runs second, not 21st); `E/storage.rs::readers_are_not_blocked_by_a_running_change` | pass | **Fixed:** requests were queued from spawned tasks, so pipelined edits of one connection could run out of order (saturation test, revisions `[3,2,…]`). |
| 3.02 | A `limits.rs` unit; B `E/sheets.rs::oversized_sheet_fetches_are_clamped_before_allocation` (failed before the fix: "reserved 4294967295 rows"); `S/scheduling.rs` clamped/overflowing dimensions, 500-column slice equals full fetch, 1 MB cells stop near 8 MiB with `truncated`, `too_large` inserts, oversized message closes only that connection, upload size (Content-Length and streamed) and budget limits incl. injected disk-full write, a client that never reads is cut off | pass | **Fixed:** sheet fetch reserved `row_count` rows before clamping and kept every non-empty row at full requested width. Gap: after a slow client is cut off its peak outbox is not asserted (only that it is closed and others continue). |
| 3.03 | A `budget.rs` unit (5); B `S/runtime.rs::open_workbooks_share_the_budget_and_never_exceed_it` (sums of `memory_limit` and `threads` read back from every DuckDB instance at 1–4 workbooks, refusal at 5 with nothing idle, eviction of an idle one to make room), `sheet_memory_comes_out_of_duckdbs_share` | pass | **Fixed:** the sheet estimate was documented but never applied. Limits are DuckDB-level, not OS-level (DURABILITY.md). |
| 3.04 | B `S/runtime.rs` idle close and reopen (not while a session exists), eviction racing requests until ≥ 10 evictions (71 requests, 10 evictions in one run, all served correctly); `E/storage.rs` view expiry rebuilds identical rows, scratch tables dropped | pass | Views per workbook bounded at 256. |
| 3.05 | C `W/cache.spec.ts` (no page; fake rpc + real server): only visible ± one 32-column block requested, ≤ 4 in flight, byte bound held over 60 scroll steps with refreshes, visible blocks never evicted under a 1-byte limit, truncated replies continued, cached values and record ids equal a full-width fetch of a sorted 80-column view; C `W/wide.spec.ts` 300 columns: far-right cell value correct, every fetch frame has `column_count` ≤ 32, middle columns never fetched; B `S/scheduling.rs` column slice equals full fetch | pass | **Fixed:** the cache counted refreshed blocks' bytes twice. |
| 3.06 | D `E/props.rs` reference model with record ids, revisions and index verification (48 cases per run; 300-case run passed in 263 s), `inserts_at_one_place_rebalance_order_keys`; B `E/storage.rs` no renumbering on insert/delete in 100k rows, local rebalancing (< 200 of 5,000 keys moved), edits by record after rows move and on deleted records, tie order | pass | |
| 3.07 | B `E/storage.rs` one revision + history entry per change, retried mutation id applied once (also after reopen), injected `history`/`commit` failures roll back data, revision and sheet memory; `S/scheduling.rs` contiguous revisions across two sessions and in events, replay across reconnect and restart; `S/backup.rs` history in restored copies | pass | History keeps the last 10,000 revisions (documented replay window). |
| 3.08 | B `E/storage.rs` committed v1 fixture (written by the phase-2 server) migrates with an identical `.v1.bak` and every recorded value, failed migration leaves format 1, newer format refused untouched, backup during 150 changes holds one revision; `S/backup.rs` three online backups during 300 acknowledged edits (revisions e.g. 19, 67, 132) each restored to exactly the edits acknowledged up to it, damaged/manifest-less/non-empty refusals without writing, offline backup fails closed and round-trips byte-identically | pass | No downgrade; `.bak` or a backup is the only rollback (DURABILITY.md). |
| 3.09 | F `S/shutdown.rs` (real binary): idle SIGTERM exit 0 with records `stopped`, SIGTERM during edits keeps every acknowledged edit and none half-applied, SIGTERM during a 4M-row import leaves a complete table or none (observed: none, reply 400), second signal exits 130 at once | pass | **Fixed:** a stop arriving mid-send dropped the frame being sent, so sessions could miss `workbook_closed`. |
| 3.11–3.14 | A `runtime/mod.rs` unit (transition table exhaustive, bounded transitions, incompatibility reasons); B `S/runtime.rs` spec pinning and data outliving compute, 8 concurrent opens → one start, cancel before ready, three failed starts (corrupt file, injected stage failure, disk-full record write), incompatible capability and storage format, restart reconciliation with a foreign lock holder (423, then opens), one server per data dir; F `S/crash.rs::a_killed_servers_allocations_are_reconciled_on_restart` | pass | **Fixed:** two self-deadlocks (a lock guard kept alive by `if let`/`for` scrutinees) found by the first test runs. Local provider only; no multi-node failover. |
| 3.15 | docs/DURABILITY.md | written | Power loss and host failure untested (stated). |

### Client side of 3.01, 3.07, 3.11 (browser)
- **Tests:** C `W/resilience.spec.ts`: a workbook whose file is corrupt shows the server's reason with "Try again", which recovers once fixed (also with the page already open); an edit across a SIGTERM restart and an edit typed while the server was down are each applied once (mutation ids, record ids), checked by export; a long SQL query shows progress and Cancel ends it with "Cancelled.". **Fixed (server):** a deletion during session setup was never reported (`workbook_deleted`), making the "deleted elsewhere" journey flaky; sessions now subscribe first. Missing workbooks answer 404.
- **Gaps:** the client's "overloaded" retry and the bounds on kept changes (30 s, 100) have no automated test; the paste guard has no browser test (headless clipboard).

### 3.T01 (3.01–3.05)
Queue saturation, fair progress, cancellation races, slow consumers, overflow-sized fetches, wide/text-heavy cache bounds, idle eviction and host-wide accounting: `S/scheduling.rs`, `S/runtime.rs`, `sched.rs`/`budget.rs`/`limits.rs` unit tests, `W/cache.spec.ts`, `W/wide.spec.ts`. **Pass.**

### 3.T02 (3.06–3.09)
Randomized edits against a reference model with stable ids and revisions (`E/props.rs`, 300 cases), duplicate mutations (`E/storage.rs`, `S/scheduling.rs`), revision monotonicity, migration rollback limits (`E/storage.rs` fixture, failure, newer format; DURABILITY.md), consistent backup/restore under activity (`S/backup.rs`, `E/storage.rs`), SIGTERM (`S/shutdown.rs`). **Pass.**

### 3.T03 (3.08–3.15)
Injected write/disk-full/permission failures (`S/storage_faults.rs` on a real 64 MB filesystem; `arm_fault_in` points), crash during persistence (`S/crash.rs`), SIGTERM, duplicate starts, cancel-before-ready, controller restart and failed-allocation cleanup (`S/runtime.rs`, `S/crash.rs`). **Pass.** Not simulated: power loss, `fsync` failure, torn writes, media errors, network filesystems (TEST_STRATEGY, DURABILITY.md).

### 3.10 Gate (2026-10-26, macOS arm64, local)

| Command | Result |
|---|---|
| `ALEPHGRID_SMALL_FS=<64 MB HFS+ image> cargo test --workspace` | engine 100; server unit 13, api 9, backup 3, crash 3, imports 5, journey 2, protocol 4, runtime 11, scheduling 10, shutdown 4, storage_faults 2 (disk full really hit ENOSPC) — all pass |
| `PROPTEST_CASES=300 cargo test -p alephgrid-core --lib props` | 4 pass (263 s) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cd web && npm run build && cd .. && cargo build -p alephgrid-server && cd web && npx playwright test` | 29 pass in each of two consecutive runs (and three runs by the client agent) |

Cancellation, overload, disk-full, crash/restart and restore preserve consistency, and resource use stays bounded at the limits that are named. Not established: multi-node failover, power-loss durability, OS-level resource limits. CI has not run this branch.


## Phase 4

Contracts below were written before implementation (TEST_STRATEGY "Rules for every change"). **Tests** and
**Result** are filled in as each item is verified; nothing is checked off before its tests have run and passed.

### Shared decisions
- **One entry point for user SQL.** Every path that runs SQL a client wrote (the console preview, exports,
  query-to-table, query-to-sheet, saved-query refresh, SQL changes, formula columns) first goes through the
  engine's SQL policy (`sqlpolicy.rs`), which analyses the statement with DuckDB's own parser
  (`json_serialize_sql`) — never by keyword matching — and only then builds the SQL that runs. The policy is
  enforced in the engine, so the WebSocket, HTTP and any embedding all get it; the UI only mirrors it.
- **The database boundary.** Each workbook's DuckDB instance runs with `enable_external_access = false`
  (which cannot be switched back on from SQL), `allowed_directories` limited to that workbook's own scratch
  directories (uploads, exports, spill), extension auto-install/auto-load off and community extensions off.
  Application imports and exports use those directories; user SQL cannot name files at all.
- **Statements that run are the statements that were checked.** The `duckdb` crate's `prepare` executes every
  statement but the last of a multi-statement string. User SQL is therefore checked to be exactly one
  statement before it is embedded anywhere, and is always embedded inside parentheses on its own lines.
- **Values are bound, identifiers are quoted.** Parameters (`$name`) are bound through DuckDB's prepared
  statement API with an explicit type; identifiers chosen by users (table and column names) are validated and
  quoted with `sql::ident`; nothing a client sends is pasted into SQL as a value.
- **No new privacy claims.** The server has one shared token and no user identities (phase 7). Saved queries
  live in the workbook and are visible to anyone who can open it; the unsaved editor draft stays in the
  browser that typed it. Nothing here is "private" beyond that.

### 4.01 Schema browser and autocomplete
- **Behavior:** a `schema` read returns every user table with its columns (name, SQL type, kind, formula) in
  one snapshot; internal tables and hidden columns are never listed. The SQL panel shows them as a tree and
  inserts correctly quoted identifiers; the editor offers table names after `FROM`/`JOIN` and the columns of
  tables named in the statement elsewhere, navigable with the keyboard.
- **Invariants:** quoted insertions round-trip (`"a ""b"""` names the column `a "b"`); suggestions never
  include `_rf_*` names; the list matches `tables` + `view_info`.
- **Scenarios:** no tables; Unicode, mixed-case, reserved-word and quote-containing names; formula columns;
  a table renamed or dropped while the panel is open (`tables_changed` refreshes it).
- **Oracle:** names created by the test; executing the inserted identifier must select that column.

### 4.02 Bound parameters and clear errors
- **Behavior:** `query` and every other user-SQL op take `params: [{name, type, value}]` for `$name`
  placeholders, `type` ∈ text, integer, decimal, double, boolean, date, timestamp; `value: null` binds NULL.
  Values are parsed exactly in Rust (integers as i128, decimals as scaled i128) and bound with that DuckDB
  type. A missing value, an unknown parameter, a value that does not parse as its type, or positional
  `?`/`$1` placeholders fail before running with a message naming the parameter. Parse and runtime errors are
  reported with DuckDB's message, without host paths of the server's data directory.
- **Invariants:** a parameter value is never part of SQL text; hostile strings (`'; DROP TABLE t; --`,
  quotes, backslashes, NUL-free Unicode, 1 MB text) select exactly the rows equal to that string.
- **Scenarios:** quotes, `%`/`_` in LIKE, Unicode, empty string vs NULL, 2^63 and 10^30 integers, decimal
  scale beyond the column, dates out of range, type mismatch against the column, repeated `$p`.
- **Oracle:** rows inserted by the test with known values; counts computed in Rust.

### 4.03 Bounded previews, truncation, timing, cancellation, full export
- **Behavior:** a preview returns at most `limit` (≤ 5,000) rows and stops near 8 MiB; it says `truncated`,
  `row_limit`, `elapsed_ms` and the `revision` it read. `cancel` stops the preview it names and nothing else.
  `export_query {sql, params, format}` writes the *complete* result to a file inside the workbook's export
  directory from one snapshot (streamed by DuckDB, never held in memory), then returns a one-time
  download id; `GET /api/workbooks/{id}/downloads/{download}` streams the file once and deletes it.
  Unclaimed exports expire. A failed, cancelled or disconnected export leaves no file.
- **Invariants:** an export holds exactly the rows of the query at one revision; a preview never claims to
  be complete when it is not.
- **Scenarios:** 0 rows, exactly `limit`, `limit + 1`, 100,000 rows; cancel queued and running; two
  concurrent queries, cancel one; deadline; client disconnect during export; data changed between preview
  and export (export reports the later revision).
- **Oracle:** closed-form row counts and sums over `range(n)`; exported CSV/Parquet read back and compared.

### 4.04 Saved queries with versions
- **Behavior:** `save_query {saved?, name, sql, params, kind}` creates a query or appends an immutable new
  version when the SQL or parameters changed; `rename_query` changes only the name; `delete_query`;
  `saved_queries` lists name, kind, latest version and timestamps; `query_versions` lists the versions;
  each change is one revision with a history entry and a `queries_changed` event. Names are unique.
- **Invariants:** versions are numbered 1, 2, … without gaps and never change; reopening returns the latest
  version unless one is named; saved queries survive restart and are included in backups.
- **Scenarios:** save unchanged text (no new version), rename to a taken name, delete then recreate, a
  hostile name, restart, concurrent saves from two sessions.
- **Oracle:** the test's own log of saves.

### 4.05 Result to table, bounded result to sheet
- **Behavior:** `create_table_from_query` keeps every row and records how it was made (4.06).
  `create_sheet_from_query` copies at most the sheet limit; when the result is larger it fails with
  `too_large` *unless* the request says `allow_truncation: true`, so the UI states the limit before
  anything is dropped. Both take parameters.
- **Invariants:** a failed or cancelled conversion creates no table or sheet and no revision.
- **Scenarios:** 0 rows, limit − 1, limit, limit + 1; exact big integers and decimals; NULLs; duplicate and
  odd column names; cancellation mid-way.
- **Oracle:** closed-form sums; values compared with a preview of the same query.

### 4.06 Saved SQL vs live query vs materialized result
- **Behavior:** three different things, stored and labelled differently: a *saved query* (text and
  parameters, versions); a *live query* (a saved query of kind `live`: opening it always runs it on current
  data and the result says which revision it read; a change to one of its source tables marks the shown
  result stale); a *materialized result* (a table created from a query, with provenance in
  `_rf_materialized`: query id/version or SQL, parameters, source tables, the revision it was computed at,
  each source table's data revision, refresh status). `materialized` lists them with `stale` computed from
  the source tables' current data revisions. `refresh_materialized` re-runs the recorded SQL and replaces the
  table's rows in one transaction (new record ids, one revision), or fails without changing anything when a
  source table is missing or the result's columns changed (`schema_changed`). *Changed during
  implementation:* the failure is returned to the client, not recorded (recording it would need a second
  committed change); the table simply stays `stale`.
- **Invariants:** a refresh is all or nothing; editing a materialized table by hand marks it `edited` so a
  refresh warns that it overwrites those edits; every table-changing operation advances that table's data
  revision.
- **Scenarios:** refresh after source edits; source column renamed; source dropped; refresh cancelled;
  materialized table renamed; restart between materialize and refresh.
- **Oracle:** sums recomputed from the source by the test.

### 4.07 SQL and formula capability restrictions
- **Behavior:** user SQL must be one SELECT (including `WITH`, `VALUES`, `FROM`-first, set operations).
  It may read user tables and its own CTEs only: no `_rf_*` tables, no other catalogs or schemas
  (`rf_scratch`, `system`, `temp`, `information_schema`, `pg_catalog`), no file or URL replacement scans,
  no table functions other than an allow-list (`range`, `generate_series`, `unnest`), no scalar functions
  on a deny-list (settings, environment, secrets, dynamic SQL execution, sequences, variables), no PIVOT
  without explicit values (DuckDB runs a hidden `CREATE TYPE` for it). A CTE may not reuse the name of a
  system relation. Formula columns get the same function policy. The database boundary (shared decisions)
  holds even if the parser-level check missed something.
- **Invariants:** a rejected statement changes nothing: revision, history, files on disk, settings
  (`current_setting` read back by the test) and table contents are unchanged.
- **Scenarios:** `read_csv`/`read_text`/`glob`/`'file.csv'`/`'https://…'`; `COPY … TO`; `ATTACH`;
  `INSTALL`/`LOAD`; `SET`/`RESET`/`PRAGMA`/`CALL`; `SELECT 1; DROP TABLE t` (and the wrapped variants that
  break out of a subquery); `query('…')`, `json_execute_serialized_sql`; `current_setting`, `getenv`,
  `duckdb_settings()`; `_rf_history`, `main._rf_views`, `rf_scratch.*`; CTE named `duckdb_settings`;
  mixed case, comments, quoted identifiers; the same through formula columns and through direct HTTP and
  WebSocket requests that skip the UI.
- **Oracle:** state snapshots before and after (revision, history length, table checksums, settings,
  directory listings).

### 4.08 SQL changes through operations that keep Alephgrid's bookkeeping
- **Behavior:** `INSERT INTO t [(cols)] <query or VALUES>`, `UPDATE t SET col = expr, … [WHERE cond]` and
  `DELETE FROM t [WHERE cond]` typed in the SQL panel are recognised (`query` replies with code
  `sql_change` and the statement kind; *narrowed during implementation:* it does not count the affected
  rows in advance, the UI asks for confirmation and the outcome reports the rows changed); `sql_change {sql, params}` applies them as one Alephgrid change:
  new rows get record ids and order keys, updated cells are cast exactly like edits, deleted rows leave
  other ids untouched, the table's structure generation and data revision advance, one revision and history
  entry, a `table_changed` event, and a `mutation_id` makes a retry apply once. Each expression is checked by
  the SQL policy. Anything else that changes data or schema (`CREATE`, `DROP`, `ALTER`, `UPDATE … FROM`,
  `RETURNING`, `ON CONFLICT`, writes to formula columns or `_rf_*` columns, more than one statement) is
  refused with a message that says what to use instead.
- **Invariants:** after any accepted change, `_rf_id` values are unique and stable for untouched rows,
  `_rf_ord` order is consistent with the position index, formula columns still evaluate; after any refused
  change, nothing differs.
- **Scenarios:** insert into an empty table, a dense and a non-dense table, with column list and without;
  update with NULL, exact decimals and a type mismatch (refused whole); delete everything / nothing; a WHERE
  using a subquery over another table; a retried mutation id; concurrent edits from another session.
- **Oracle:** a Rust reference model of the table (as in `props.rs`), index verification (`verify_index`).

### Results (2026-10-26, Linux x86_64 cloud container, local; branch `claude/eager-ramanujan-lns8s1`)

| Task | Tests | Result | Found and fixed / gaps |
|---|---|---|---|
| 4.01 | B `E/sqlwork.rs::schema_lists_user_tables_with_columns_and_nothing_internal` (Unicode, quoted, reserved and mixed-case names; schema equals the view's columns; inserted identifiers select their columns); B `S/sqlworkspace.rs` `schema` over the wire; C `W/sql.spec.ts` "browse the schema…", "names inserted from the tree are quoted…", "completion offers tables after FROM…" (keyboard, ARIA combobox/listbox) | pass | — |
| 4.02 | A `sqlpolicy.rs` unit (exact integer/decimal/date/timestamp parsing, dates vs DuckDB for 7 boundary dates, positional placeholders refused, params must match placeholders); B `E/sqlwork.rs::parameters_are_bound_not_spliced` (hostile strings, LIKE, NULL vs empty, 2^53+1, decimals, repeated `$p`), `parameter_errors_name_the_parameter` (missing, unused, bad value, type mismatch, `LINE n` in the user's text); B `S/sqlworkspace.rs::queries_bind_parameters_and_explain_refusals_with_codes` (codes `bad_parameter`/`not_allowed`/`sql_change`, data-dir paths redacted); C hostile `$region` value sent only in `params` | pass | **Fixed:** a decimal parameter such as `-0.000001` was bound as `DECIMAL(6,6)` and came back as `-.000001`; decimals now keep one integer digit. |
| 4.03 | B `E/sqlwork.rs::previews_are_bounded_and_say_what_they_read` (0, limit, limit+1, 20,000 rows; 5,000 cap; revision and source data revisions), `exports_hold_every_row_of_one_snapshot` (100,000 rows to CSV/Parquet/JSON read back against closed-form sums, a failing export); B `S/sqlworkspace.rs::cancel_hits_one_query_and_interactive_reads_are_not_starved` (queued vs running cancel, other session unaffected, fetch < 5 s during three long queries, deadline, disconnect), `exports_hold_every_row_download_once_and_leave_nothing_behind` (120,000-row CSV parsed and summed, once-only download, other workbook's id 404, expiry, cancelled/failing/disconnected exports leave no file, 20 cancel-vs-completion races); C "a superseded run's reply never replaces the newer result" (failed with the guard removed), "previews say when they are truncated…", "export writes every row…" | pass | **Fixed:** a read whose COMMIT failed (a cancel landing as it finished) left its reader connection inside a transaction, so every later read on it failed (`E/capabilities.rs::a_failed_snapshot_commit_does_not_poison_the_connection` fails on the old code, passes now); a cancelled read that finished anyway now publishes nothing and its export file is removed; the decision is taken after the request leaves the cancel table, so a cancel that answered `true` never sees a result (the 20-iteration race in `sqlworkspace.rs` caught it); reads of `_rf_meta`/`_rf_history` no longer swallow errors (an interrupt there silently aborted the transaction and surfaced as an uncoded error); a job that fails after a cancel or deadline is reported as such; a stopped job is interrupted again every 500 ms until it ends, because an interrupt landing between two statements is lost (seen as a cancelled export that ran on for minutes). |
| 4.04 | B `E/sqlwork.rs::saved_queries_keep_immutable_versions_across_reopen` (versions 1–3, unchanged saves, renames, clashes, refusals leave the fingerprint unchanged, reopen, delete); B `S/sqlworkspace.rs::saved_queries_and_materializations_persist_and_notify` (`queries_changed` to another session, restart); C "saved queries survive a reload…" | pass | No per-user privacy (no identities before phase 7); stated in UI and docs. |
| 4.05 | B `E/sqlwork.rs::sheet_copies_never_drop_rows_silently` (limit−1, limit, limit+1 refused without `allow_truncation`, accepted with it, big integers as text, refused copies change nothing); B `E/sql.rs` materialized formula values (existing); C "a result larger than a sheet asks first…", `W/kinds.spec.ts` (now confirms before truncating) | pass | — |
| 4.06 | B `E/sqlwork.rs::materialized_tables_record_sources_and_refresh_all_or_nothing` (provenance, stale after a source edit, injected refresh failure changes nothing, new record ids, `edited`, `schema_changed`, renamed/dropped sources, reopen, drop), `every_table_change_advances_its_data_revision_and_nothing_else_does` (12 operations; saves, renames and failures do not), `E/storage.rs::v2_fixture_migrates_to_format_3` (committed fixture written by the phase-3 server); F `S/crash.rs::a_killed_materialization_or_refresh_is_all_or_nothing` (SIGKILL during a 20M-row refresh and a new materialization); C "a live query runs on open and turns stale…", "a materialized table keeps exact values, survives a restart, and refreshes when stale" | pass | Refresh failures are returned, not recorded (contract narrowed above). |
| 4.07 | A `sqlpolicy.rs` (allowed query forms; refused statements, files/URLs, catalogs, internal tables, CTE scoping and shadowing, functions), `sqllex.rs`, `formula.rs`; B `E/capabilities.rs::every_user_sql_path_refuses_the_same_statements_and_changes_nothing` (40 statements × 7 entry points, fingerprint of every table incl. `_rf_*`, settings and files), `formulas_get_the_same_function_policy`, `earlier_statements_of_a_multi_statement_string_never_run`, `the_database_refuses_files_extensions_and_settings_once_restricted` (database layer alone, past the policy), `internal_tables_are_not_reachable_through_table_operations`; D `random_statements_never_change_state_through_queries` (proptest over hostile fragments); B `S/sqlworkspace.rs::every_open_workbook_is_locked_down_but_application_files_still_work` (settings read back, per-workbook directories, imports/exports/duplicate/backup still work, internal tables refused over WS and HTTP); C "refused statements change nothing…" | pass | **Fixed:** (1) the `duckdb` crate's `prepare` runs every statement but the last: on the phase-3 code, query → sheet with `SELECT 1; DROP TABLE t; SELECT 1` dropped `t` with no revision or history (shown with a scratch test in the baseline worktree, not committed; previews were saved by their snapshot rollback); (2) table operations took internal names: `columns("_rf_history")` read metadata and `drop_table("_rf_views")` was possible (regression test failed before the fix); (3) the first policy walker missed CTE bodies (DuckDB 2.0 stores them at `query_node`); found by the tests before anything shipped, now fail-closed. |
| 4.08 | A `sqlchange.rs` unit (statement splitting, refused forms); B `E/capabilities.rs::sql_changes_keep_record_ids_order_index_revisions_and_history` (reference model of record ids and values, index verified, formula columns, other tables in WHERE, one revision each, mutation-id replay, reopen), `sql_changes_that_would_bypass_bookkeeping_are_refused_whole` (29 statements incl. shape break-outs, rounding, internal columns/tables; injected failures roll back), `queries_name_the_change_path_for_dml`; B `S/sqlworkspace.rs::sql_changes_apply_once_with_revisions_and_events`; C "…a change is applied only after confirmation" | pass | — |

**Baseline before phase 4** (`HEAD` = `bbd4c2c`, separate worktree and target directory, run while this
container was also compiling): engine 100 passed; server unit 13, api 9, backup 3, crash 3, imports 5,
journey 2, protocol 4 passed; `runtime.rs` 10 passed, 1 failed:
`eviction_racing_new_work_never_serves_from_a_closed_workbook` stopped on its own precondition (6 evictions
within its 60 s cap, it requires 10) under CPU contention — its correctness assertions held. Cargo stopped
there (no `--no-fail-fast`), so the baseline did not run scheduling, shutdown or storage_faults. The same test
passed in every later run here.

**Phase-3 tests corrected (races in the tests, found under load here):** `scheduling.rs::uploads_are_bounded…`
now accepts a reset connection for the 507 case exactly as it already did for 413 (the server answers while
the client still sends; nothing stored is still asserted); `scheduling.rs::a_client_that_does_not_read…` now
waits for the slow session to be registered before waiting for it to be closed (otherwise "one session
left" could hold before it existed).

**Client fix found by the full browser suite:** `views.spec.ts` failed 1 time in 2 after phase 4: the Views
menu was built from saved views that were still loading after a reload (a pre-existing race made more likely
by the extra requests the workspace makes when a table opens). The menu now waits for the load; 5/5 alone and
in the full suite afterwards.

### 4.T01 (4.01–4.06)
Schema/autocomplete, bound parameters and hostile strings, joins/CTEs/calculated columns and exact values
against hand-computed results, truncated previews, cancellation, complete exports, saved queries and
materializations across refresh/reopen/restart, in engine, HTTP/WebSocket and browser tests listed above. **Pass.**

### 4.T02 (4.07–4.08)
Direct SQL and formula attempts on files, network, extensions, settings and internal metadata through every
entry point and over the wire without the UI; multi-statement and break-out forms; the database layer tested
on its own; SQL changes against a reference model; rejections leave the fingerprint (tables incl. internal
ones, settings, files) unchanged. **Pass.**

### 4.09 Gate (2026-10-26, Linux x86_64, local cloud container)

| Command | Result |
|---|---|
| `PROPTEST_CASES=96 ALEPHGRID_SMALL_FS=<64 MB tmpfs> cargo test --workspace --no-fail-fast` | engine 134; server unit 13, api 9, backup 3, crash 4, imports 5, journey 2, protocol 4, runtime 11, scheduling 10, shutdown 4, sqlworkspace 6, storage_faults 2 — all pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cd web && npm run build` | type check + bundle pass |
| `E2E_CHROME_PATH=/opt/pw-browsers/chromium-1243/chrome-linux64/chrome npx playwright test` (headless Chromium 1243; Google Chrome is not installed here) | 41 pass (12 in `sql.spec.ts`); also 41/41 in the run before the last server fixes |
| Repeated: `sqlworkspace` + `scheduling` suites | 8/8 and 3/3 consecutive passes after the cancellation fixes |

Joins, aggregates, CTEs, calculated columns, parameters, cancellation, export and materialization are verified
end to end (engine, HTTP/WebSocket and browser). Not established: CI on this branch, Google Chrome (CI's
browser), macOS, scheduled 2000-case property runs, power loss. Organization authorization is phase 7.

## Phase 5

Verified 27 October 2026 on macOS arm64 with the real DuckDB/IronCalc engines and headless Chrome.
The contract and limitations are in [FUNCTIONS.md](FUNCTIONS.md).

| Checklist | Behavior and evidence |
|---|---|
| 5.01–5.04 | `E/functions.rs::lambda_recalculates_versions_and_reopens`, `ranges_nested_calls_edits_and_undo_use_engine_semantics`: real named LAMBDAs, cell and range arguments, nested calls, hand-computed answers, recalculation and undo. Browser `W/functions.spec.ts` defines, tests and publishes through the editor. |
| 5.05 | Browser function journey types `=MAR`, accepts Tab completion, verifies signature/documentation, calls the formula and reads its result. Invalid SQL is shown in the editor status. |
| 5.06 | `dependency_versions_upgrade_and_rollback_atomically`, `workbook_scope_and_recreated_sheets_keep_exact_pins`: publication leaves active versions alone; incompatible single upgrades fail; version maps upgrade/rollback atomically; another workbook is unaffected. Browser journey activates v2 and rolls back v1. |
| 5.07 | `formula_blanks_errors_dates_and_array_results_are_explicit`, `sql_columns_refresh_and_null_decimal_date_semantics`, `validation_conflicts_and_engine_boundaries_are_atomic`: blank/error handling, exact DECIMAL output, dates, array spill and occupied-cell refusal, name conflicts and execution-engine boundaries. |
| 5.08 | SQL persistence/validation and column tests cover real macros, query calls, calculated-column invalidation, NULL/DECIMAL/DATE, statement/subquery/settings refusals. Browser SQL editor journey publishes and queries the macro. |
| 5.09 | Engine tests cover indirect cycles, save/reopen, version changes and XLSX named-LAMBDA round trips. XLSX intentionally omits the Alephgrid registry and SQL macros; imported names cannot silently become registry functions. |
| 5.11 | `incompatible_runtime_and_missing_pins_refuse_reopen`; runtime `incompatibility_names_the_reason`: exact adapter/DuckDB/IronCalc components, invalid pins, unsupported execution adapters and capability mismatches refuse execution. |
| 5.T01–5.T02 | All tests above, `invalid_formulas_and_failed_publish_leave_no_changes`, `failed_commit_rolls_back_registry_macros_and_sheet_model` (injected history/commit failures), `storage_v3_migrates_before_function_publication`, `oversized_definitions_and_expanding_dependency_graphs_are_refused`. |
| 5.10 | Browser journey defines, tests, calls, upgrades, rolls back and reloads a workbook function. Engine range test edits an input and independently checks the recalculated result. |
| Transport and recovery | `S/functions.rs::publish_retries_once_notifies_other_clients_and_survives_restart`: idempotent retry does not publish twice, observer gets resync, invalid pin leaves history unchanged, process restart keeps the formula/pin. `S/api.rs` exercises every operation/result shape. Browser SQL-disabled test permits formula functions and refuses SQL publication. |

Validation: `cargo test --workspace` passed; the final focused function run passed **14 engine tests**;
`cargo clippy --workspace --all-targets -- -D warnings` passed; generated protocol drift tests passed;
`npm run build` passed; `cargo build -p alephgrid-server` passed; the full browser suite passed **44 tests**,
then all **3 function journeys** passed again against the final rebuilt server.

Limits of this evidence: no certification against Microsoft Excel itself or every function/argument combination;
no new Linux/Windows run performed locally. The disk-full test in the workspace suite requires
`ALEPHGRID_SMALL_FS` on CI and skips its full-disk scenario on this machine. Current-time/volatile,
recursive and unsupported formula constructs are refused; Python and shared-space function authorization
are outside phase 5. The Local provider is not a hostile-code sandbox. Test-preview assertions for arrays
cover the anchor cell; the engine test separately verifies spill cells and collision handling.

## Phase 6

Verified 27 October 2026 on macOS arm64 with real DuckDB and headless Chrome.
Supported semantics and initial limits are documented in [ANALYSIS.md](ANALYSIS.md).

| Checklist | Behavior and evidence |
|---|---|
| 6.01–6.04, 6.T01 | `E/analysis.rs::summary_pivot_profile_and_join_have_independent_oracles` checks hand-computed aggregates, pivot categories, NULLs, duplicates and joins. `empty_sources_skew_and_quoted_identifiers` covers empty inputs, skew, quoting and million-row join cardinality with bounded output. `saved_analysis_refreshes_and_snapshot_is_bounded` checks refresh and rejects oversized snapshots. |
| 6.05 | `replace_preview_apply_undo_restart_and_conflicts` verifies preview, exact replacement, undo, reopening and stale revisions. `replacement_limits_and_undo_failure_are_atomic` tests row/byte expansion limits and injected undo commit failure. `validation_and_faults_leave_state_unchanged` checks invalid plans and history/commit rollback. |
| 6.06, 6.T02 | `presentation_persists_and_bounds_are_enforced` verifies persistence and limits. Browser `W/analysis.spec.ts` checks exact decimal/large-integer display, locale and timezone semantics, hidden/frozen columns after reopening, horizontal hit testing and conditional formatting. `schema_drift_is_explicit_and_presentation_keeps_a_visible_column` covers removed fields and saved ID reuse prevention. |
| 6.07 | Browser journey imports two CSVs, joins, materializes, summarizes, charts, saves and reopens without SQL; editing a source and refreshing changes the result. Missing saved-plan columns remain explicit errors even after another control is edited. Axe checks the analysis dialog; existing keyboard/accessibility journeys cover the grid. |
| Transport and recovery | `S/analysis.rs::no_code_analysis_and_replacement_work_without_arbitrary_sql` verifies typed operations with arbitrary SQL disabled, mutation retry protection, observer events, saved-plan restart and durable undo. `S/api.rs` covers every new operation/result contract. |

Validation: **231 Rust tests passed** with `CARGO_INCREMENTAL=0 cargo test --workspace -j 1`;
Clippy with `--all-targets -- -D warnings`, generated protocol drift checks, frontend type checking/bundling
and server build passed. The final full browser suite passed **49/49 tests**, including all five new analysis tests.

Evidence limits: no new Linux/Windows run, no Microsoft Excel certification, and no exhaustive locale or
browser matrix. The full-disk fault scenario requires `ALEPHGRID_SMALL_FS` and skips that scenario locally.
The first browser run passed 48/49; the import journey exposed a test readiness race (the tab appears before
its view opens), now synchronized on loaded rows. An initial Rust link failed because local build artifacts
filled the disk; clearing generated artifacts and rebuilding with incremental compilation disabled succeeded.

### Phase 6 CI timing regression follow-up (28 October 2026)

CI run 40536789345 exposed two UI races: function activation refreshed the version selector
while a rollback selection was being made, and table controls were enabled before the view
finished opening. Version selection is now disabled through mutation/refresh, and table
controls remain disabled until a view is available. The existing function and saved-view
journeys hold specific WebSocket replies to exercise these states deterministically.
Both readiness assertions failed against the old build; after the fixes, frontend type
checking/bundling, server rebuild and all **49 browser tests passed** locally. No retries or
longer timeouts were added. Remote CI results must be checked separately.

CI run 40583917349 passed the web build but stopped in the shutdown integration suite
before E2E ran: workbook creation received connection refused immediately after the TCP
readiness probe. The helper had released a temporary port before spawning its child,
allowing parallel tests to race for that port or probe another process. It now launches
with port 0 and waits for the child's reported bound address; the CLI prints the actual
bound port. Startup failure is bounded and kills/reaps the child with stderr retained.
The four shutdown tests passed in three consecutive local runs (12 executions); targeted
Clippy for the binary and shutdown test passed. The initial sandboxed attempt was unable
to bind local sockets; verification used authorized local-process execution. Remote Linux
CI remains the confirmation for the reported environment.

## Phase F — catalog and P0 baseline (first increment)

The scope is [FORMULA_CATALOG.md](FORMULA_CATALOG.md), not full spreadsheet certification.

| Area | Evidence |
|---|---|
| Initial F.01 manifest | `E/catalog.rs::catalog_registry_and_metadata_match_the_pinned_engine`: 562 unique names, pinned dependency/version, exact registry status, table-status boundary and metadata requirements. |
| F.02 baseline | `catalog_examples_match_independent_expectations`: 44 hand-calculated P0 examples evaluated through real Alephgrid sheets and checked again after reopening. |
| Lookup boundary cases | `lookup_modes_errors_and_source_changes_have_golden_results`: duplicate reverse selection, missing/default, approximate next-larger, wildcards, index/range errors, recalculation and undo/reopen. |
| Initial F.03 completion | `W/catalog.spec.ts`: built-in Tab insertion and actual range-based lookup, ordinary/quoted text suppression, persistence and no sheet completion in table context. Existing `functions.spec.ts` covers custom function completion and publishing/version changes. |
| Search/help UX | `W/catalog.spec.ts`: at most 40 results, paging, filtering, empty states, explicit unverified/unsupported status and axe serious/critical checks. |

Known failure: inline-array XLOOKUP returns `#N/A` instead of the independently expected `20`
on pinned IronCalc 0.9.1. The desired-result test is retained with an explicit `ignore` reason
and the limitation is visible in help. It is outside the passing baseline; F.02, F.03 and the
phase F analyst gate are not complete. The initial browser probe exposed this gap; assertions
were not changed to certify the incorrect value. The supported browser journey uses ranges.

Validation on 28 October 2026 (macOS arm64): workspace suite **234 passed**; final catalog
suite **3 passed / 1 explicitly ignored known-gap test**; explicit execution of that ignored
test reproduced the documented incorrect `#N/A`. All-target Clippy with warnings denied,
frontend type checking/bundling and server rebuild passed. Final browser suite **52/52 passed**.
The first completion probe used inline arrays and exposed the engine gap; the final browser
journey verifies the supported range form. No full Excel/Sheets compatibility or large-table
formula support is claimed. Linux and the small-filesystem disk-full scenario remain CI checks.

### Phase F reference editor increment

`W/references.spec.ts` checks the four F4 anchor states, multi-token selections, quoted sheet
names and strings, function-name boundaries (including whitespace before `(`), structured
reference exclusion and coordinate limits. The browser journey drags A1:A3 into a formula
for C1, checks that C1 stays the destination, evaluates to 60, checks Escape and reopening,
and exercises inline F4. Reference labels pass serious/critical axe checks in light/dark modes.
The initial focused catalog/functions/references run passed all 9 tests. Scope and remaining
editor limitations are in [FORMULA_EDITOR.md](FORMULA_EDITOR.md). No Rust evaluator or protocol
behavior changed in this increment; the prior 234-test Rust baseline and known ignored
array-input XLOOKUP regression remain applicable.

Final editor-increment validation: frontend type checking/bundling and server rebuild passed;
full browser suite **55/55 passed** on macOS arm64/Chrome. No retries or increased timeouts.

### F.02 — mode-level formula evidence

- `tests::catalog::certification_cases_match_independent_expectations`: 121 manifest
  cases across 44 P0 functions; independent expected results, unique IDs, explicit
  verified/gap statuses and file-backed reopening. Known gaps are excluded from
  passing assertions and remain documented with their desired results.
- `web/e2e/catalog.spec.ts`: inspect verified lookup cases and the inline-array
  XLOOKUP gap through Formula help; retain search, pagination and accessibility checks.
- `web/e2e/sheets.spec.ts`: wait for accessible selection coordinates before reading
  the cell value, with bounded retries when opening a view resets selection.
- This increment is case-level evidence, not full function/mode certification.

Executed locally on 28 October 2026: catalog tests **4 passed / 6 explicitly ignored**;
all six ignored desired-result tests were run separately and failed as documented;
focused catalog/functions/sheets browser suite **9/9 passed**, followed by **3/3 catalog
checks** after adding the unequal-range evidence assertion. Frontend typecheck/build
and server build passed. These counts distinguish test functions from the 115 passing
mode fixtures and do not count known failures as supported behavior.

## Phase F (F.T01–F.T03)

Expected values come from hand calculation, documented Excel rules or IronCalc used as an
independent engine (differential tests), never from the code under test.

| Requirement | Evidence |
|---|---|
| **F.T01** golden values per function and mode | `tests::catalog::certification_cases_match_independent_expectations` (351 cases: 44 P0 and 32 P1 functions, 344 verified, 7 known gaps that fail if they change), `catalog_examples_match_independent_expectations` (76) |
| missing and duplicate keys, first/last matches | catalog XLOOKUP/VLOOKUP/MATCH cases; `table_lookups_agree_with_the_sheet_engine`, `tests::table_formulas` duplicates |
| wildcard escapes | catalog `a~*b` cases, `approximate_and_wildcard_lookups_agree_with_the_sheet_engine` (`~*`), COUNTIFS `a~*b` |
| mixed types, blanks, errors | coercion matrix (156 cases over typed F1:G8), `sheet_cells_agree_with_the_sheet_engine` (7 kinds of cell value) |
| unequal ranges | `sumifs/countifs/averageifs_should_reject_unequal_ranges` |
| Unicode | lookups with `ärger`/`Ärger`, sheet names `'Prix é'` (parse tests), `TEXTSPLIT`/`TEXTBEFORE` cases |
| absolute and mixed references | `e2e/references.spec.ts` (F4 cycling), sheet-cell `$` handling in `table_formula::tests` |
| keyboard-only formula entry, undo, reopen | `e2e/names.spec.ts`, `e2e/commands.spec.ts`, `e2e/tableformulas.spec.ts` (formula bar), `patched_formula_boundaries_survive_reopen`, every certification case checked again after reopening |
| **F.T02** independent evaluator versus batched plans | differential tests against IronCalc: 99 row formulas, 10 + 11 + 6 lookups, 58 aggregates, 24 + 28 F.07 lookups, 99 sheet-cell formulas, each over 60 typed rows |
| duplicate/cardinality invariants, stable ordering | one result per row asserted in every differential test; `lookups_follow_reordered_sources_and_reject_stale_caches` (persisted order, inserts) |
| cycles, rename/delete/reorder of sources | `tests::table_formulas` (circular references, renames, refused deletes), `tests::tablecells::circular_references_between_sheets_and_tables_are_refused`, reorder test above |
| cross-engine errors | sheet-cell `=1/0` into table formulas; table cells showing `#N/A`, `#REF!`; `tests::links` |
| concurrent source edits, cancellation, fault rollback | `tests::results` (interrupted build installs nothing, superseded builds, failed builds keep results), `a_failed_recalculation_leaves_tables_and_sheets_as_they_were`, server `stored_results_are_recalculated_in_the_background`, e2e cancel |
| stale-cache rejection | `cached_lookups_are_rebuilt_only_when_their_source_changes`, `cached_lookups_are_shared_only_when_other_connections_can_see_them`, stored-result stamps |
| **F.T03** spill collision and size refusal, dynamic arrays | `table_results_spill_into_sheets` (`#SPILL!`, 10,000-cell bound), P1 spill cases, `named_ranges_feed_dynamic_arrays` |
| pivot/filter totals, chart bounds | `tests::reports`, `e2e/analysis.spec.ts` (100-group chart limit, line chart values) |
| validation bypass attempts | `validation_rules_hold_on_every_write_path` (edit, append, SQL change), `e2e/tools.spec.ts` |
| XLSX provenance fixtures and incompatibility reports, round trip | `tests::xlsx` round trip, `the_published_sample_workbook_opens_with_its_documented_values`, `xlsx_downloads_say_what_they_leave_out_or_replace`, `e2e/tools.spec.ts` (XLSX notes) |
| real-browser journeys | the e2e suite in Chromium, WebKit and Firefox; `e2e/analyst.spec.ts` (F.15) |

## Phase 7 (7.01–7.06, 7.17; part of 7.T01)

Expected permissions are written out by hand in the tests from the role table in
[TEAMS.md](TEAMS.md), never read from `ws::required_caps` or the route handlers. Tests start real
servers (in-process, and the binary for the browser) with real sessions, tokens and a mock OpenID
provider that signs ID tokens with a fixture RSA key and checks PKCE.

| Requirement | Evidence |
|---|---|
| **7.01** organization → space → workbook; workspace migration | `teams::workspaces_migrate_to_spaces` (live and trashed workbooks, a clashing *General*, ids kept, `workspaces.json` moved, idempotent restart, restore into the migrated space); `teams::backups_restore_identities_and_roles` (control database in backups, damaged copy refused) |
| **7.02** bootstrap admin, password and OIDC sign-in, sessions, logout, service tokens | `setup_code_creates_the_instance_admin_once`, `password_sign_in_refuses_guessing` (same answer for unknown email and wrong password, lockout after 5), `password_change_needs_the_current_password` (other sessions end), `sessions_expire` (HTTP and an open WebSocket), `service_tokens_are_scoped_to_their_space_and_role` (one space, its role, header only, revocation closes connections, expiry, forged secret), `single_sign_on_maps_claims_to_users_and_roles`; `e2e/teams.spec.ts` (setup code from the console, sign-in, wrong password, sign-out) |
| **7.03** roles with inheritance and sharing rules | `every_http_route_follows_the_role_table` (every route of the router, checked against a hand-written list, × anonymous, viewer, editor, owner, organization member without a role and admin; `/ws/{id}` and workbook deletion have their own tests), `membership_rules` (organization members only, last admin kept, admins own every space, removal from the organization removes space roles, spaces deleted only when empty, trash included, editors restore but do not purge) |
| **7.04** every HTTP route and WebSocket operation | the route matrix above; `every_websocket_operation_follows_the_role_table` (every declared operation as viewer and editor; refused operations change nothing); `ids_from_another_space_are_not_found` (workbook, space, token, trash, move, WebSocket and download ids from another space answer exactly like missing ones; listings filtered) |
| **7.05** space membership, invitations, group mapping | `invitations_are_single_use_and_bound_to_their_email` (forged secret, wrong signed-in user, used once, existing users sign in first, revoked, admins only), group roles in the OIDC test; `e2e/teams.spec.ts` (invite into a space as viewer, accept in a second browser, server refuses a rename, removal sends the open workbook away) |
| **7.06** no secrets in URLs or replies | `browser_and_proxy_protections` (cross-site `Origin` and missing `Origin` with a cookie refused, same-origin fetch accepted, cross-site WebSocket refused, foreign `Host` refused, cookie `HttpOnly`/`SameSite=Lax`), token in a query string refused, `secrets_stay_out_of_replies_and_the_audit_log_attributes_actions` (no hashes, passwords, token or session secrets in nine admin/listing replies; data directory redacted from HTTP errors; audit actors, spaces, workbooks) |
| **7.17** claim mapping and revocation, identity headers | the OIDC test: identity by issuer + subject (a changed email is the same user), group removal at the identity provider takes effect at the next sign-in, refusals for wrong audience, wrong issuer, expiry, another nonce, another authorized party, a stranger's key, an unsigned token, an unverified email, a password account's email, an unknown `state`; `next` limited to this server's paths. `browser_and_proxy_protections` refuses five proxy identity headers on private and public routes. `open_connections_lose_access_when_it_is_revoked` (demotion → `role_changed` and refusal, removal, sign-out, disabling, moving the workbook to another space → `access_revoked`) |

Not covered yet: quotas (7.07), audit of mutations/exports/publication (7.10), sharing of saved
objects (7.11); TLS termination and `Secure` cookies behind a real https proxy are configured by
`--public-url` but only checked by reading the cookie attributes over http.
