# Test matrix

Maps checklist task IDs ([IMPLEMENTATION_CHECKLIST.md](IMPLEMENTATION_CHECKLIST.md)) to their behavior, tests and latest
results. Layers A–H are defined in [TEST_STRATEGY.md](TEST_STRATEGY.md). Paths are relative to the repository root.
`E` = `crates/rowfinity-engine/src/tests/`, `S` = `crates/rowfinity-server/tests/`, `W` = `web/e2e/`.

**Phase 1 verification (2026-10-26, macOS arm64, local).** CI on PR #1 has since run `cargo test --workspace` on Linux: all tests passed; the run then failed only because clippy was missing from the pinned toolchain (fixed in `ec28345`).

| Command | Result |
|---|---|
| `cargo test --workspace` | engine 61 passed; server api 9, journey 2, crash 2, protocol 2 passed |
| `PROPTEST_CASES=400 cargo test -p rowfinity-engine --lib props` | 3 passed (188 s) |
| `cargo test -p rowfinity-server` ×3 | passed every time |
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

**Latest verification (2026-10-26, macOS arm64, local, branch `phase-2-local-spreadsheet`):** `cargo test --workspace` engine 81 passed; server api 9, crash 2, imports 5, journey 2, protocol 4 passed; clippy clean; `npx playwright test` 20 passed in two consecutive runs; `cargo test -p rowfinity-server --test api` 6 consecutive runs passed. CI has not run on this branch.

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
  instance; `max_temp_directory_size` bounds DuckDB spill files. Rowfinity's own accounting covers the sum
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
| `ROWFINITY_SMALL_FS=<64 MB HFS+ image> cargo test --workspace` | engine 100; server unit 13, api 9, backup 3, crash 3, imports 5, journey 2, protocol 4, runtime 11, scheduling 10, shutdown 4, storage_faults 2 (disk full really hit ENOSPC) — all pass |
| `PROPTEST_CASES=300 cargo test -p rowfinity-engine --lib props` | 4 pass (263 s) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cd web && npm run build && cd .. && cargo build -p rowfinity-server && cd web && npx playwright test` | 29 pass in each of two consecutive runs (and three runs by the client agent) |

Cancellation, overload, disk-full, crash/restart and restore preserve consistency, and resource use stays bounded at the limits that are named. Not established: multi-node failover, power-loss durability, OS-level resource limits. CI has not run this branch.

