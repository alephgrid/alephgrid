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
