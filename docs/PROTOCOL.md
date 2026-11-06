# WebSocket and HTTP protocol

Connect to `ws(s)://<host>/ws/<workbook-id>`. With `--auth accounts` the upgrade request
carries the session cookie (browsers) or `Authorization: Bearer agt_…` (service tokens); a
principal that cannot see the workbook gets 404, and an `Origin` other than the server's own
gets 403. Every frame is a JSON text message.

Each operation needs a capability in the workbook's space (`ws::required_caps`, the table in
[TEAMS.md](TEAMS.md)): reads need `view`, `query` and `export_query` need `query` (and
`export`), changes need `edit`, `publish_function` and `pin_functions` need `publish`. A refused
request gets `code: "forbidden"` and changes nothing; an expired credential gets
`"unauthenticated"`.

The TypeScript types in [`web/src/protocol.ts`](../web/src/protocol.ts) are generated from
the Rust types and are the precise definition of every request, event and result
(`Op`, `Event`, `Results`). `cargo test` fails when that file is stale; regenerate with
`UPDATE_BINDINGS=1 cargo test -p alephgrid-server --test protocol`.

## Envelope

```jsonc
// client → server
{ "id": 7, "op": "fetch", "view_id": 1, "row_start": 0, "row_count": 100 }
{ "id": 8, "op": "edit", "view_id": 1, "edits": […], "mutation_id": "c1-42", "timeout_ms": 10000 }
// server → client
{ "id": 7, "ok": true,  "result": { "row_start": 0, "rows": [[…], …], "ids": [0, 1, …], "column_start": 0, "truncated": false } }
{ "id": 8, "ok": true,  "result": {…}, "revision": 12 }              // a committed change
{ "id": 8, "ok": true,  "result": {…}, "revision": 12, "replayed": true }  // a retry of an applied change
{ "id": 9, "ok": false, "error": "this workbook is busy …", "code": "overloaded" }
// server push (no id)
{ "event": "hello", "session": 3, "workbook": {…}, "revision": 11, "role": "editor" }
{ "event": "cells_changed",  "table": "flights", "origin": 3, "revision": 12 }
{ "event": "table_changed",  "table": "flights", "origin": 3, "revision": 13 }  // rows inserted/deleted, columns changed
{ "event": "tables_changed", "origin": 3, "revision": 14 }   // tables or sheets added/removed/renamed
{ "event": "sheet_changed",  "sheet": 0, "origin": 3, "revision": 15 }  // sheet: null after undo/redo
{ "event": "views_changed", "table": "flights", "origin": 3, "revision": 16 }  // saved views of a table changed
{ "event": "queries_changed", "origin": 3, "revision": 17 }  // saved queries were added, changed or removed
{ "event": "workbook_deleted", "origin": 0 }  // moved to the trash; the server then closes the socket
{ "event": "workbook_closed", "reason": "idle" | "stopped" | "shutdown" }  // reconnect to reopen
{ "event": "committed", "origin": 3, "revision": 18 }  // a change with nothing to refresh
{ "event": "resync", "revision": 19 }  // refresh what is shown: a broad change, or events dropped for this slow client
{ "event": "progress", "request": 9, "percentage": 41.5 }  // a long request of this connection
{ "event": "role_changed", "role": "viewer" }  // this connection's role changed; applies to the next request
{ "event": "access_revoked", "reason": "signed_out" | "no_access" }  // the server then closes the socket; do not reconnect
```

Undo/redo sends `tables_changed` instead of `sheet_changed` when it adds, removes or renames a sheet.

A change can also change tables it is not about: tables whose formulas read sheet cells that got
new values, and tables whose formulas read (look up, aggregate) a table that changed. After the
change's own event, the server sends `cells_changed` for each such table, or `table_changed` when
its columns changed too (a cell's value type changed, or its formulas were rewritten because a
cell they read moved), with `origin: 0`, since no session has applied them yet. When the change
also gave sheet cells that read tables (table cells) new values, it then sends `sheet_changed`
with `sheet: null` and `origin: 0`. `sheet_delete` is refused while table formulas or table
cells on other sheets read the sheet. `sheet_set` turns a formula naming a table into a table
cell, and `sheet_check_formula` checks such formulas as table formulas.

Large tables keep stored formula results. After a change makes them stale the server
recalculates them in the background and sends `table_changed` (`origin: 0`) when done, failed
or cancelled. Until then, views sorted or filtered by formula columns carry `stale` (on
`ViewInfo` and `RowBlock`: `recalculating`, or `recalculation failed: …`) and keep the last
committed results. `cancel_recalculation { table }` stops the running job; `recalculate
{ table }` queues one; both reply at once.

Requests can be pipelined; replies may arrive out of order and are matched by `id`. Requests of one
connection are queued in the order they arrive.

### Scheduling, deadlines, cancellation

Every request is a job for its workbook. Reads (`tables`, `view_info`, `fetch`, `stats`, `sheets`,
`sheet_fetch`, `saved_views`, `history`; and, as bulk reads, `open_view`, `update_view`, `query`)
run on reader connections and never wait for a change; one reader is always kept for interactive
reads. Changes run one at a time. Interactive jobs go before bulk ones (imports, queries, copies,
`create_*`), except bulk jobs that have waited 2 s; connections take turns.

* At most 32 requests per connection and 256 per workbook may wait or run; beyond that the reply is
  `code: "overloaded"` (try again shortly).
* `timeout_ms` caps how long a request may take, waiting included; the server caps it at
  `--request-timeout` (interactive, default 30 s) or `--job-timeout` (bulk, default 1 h). A request
  past its deadline fails with `deadline_exceeded`; a running one is interrupted.
* `{"op": "cancel", "request": 9}` cancels this connection's request 9 (result: whether it was
  found). A waiting request is removed, a running one interrupted; it then fails with
  `code: "cancelled"`. A change that already committed stays committed.
* Closing the connection cancels its waiting and running requests.

### Changes, revisions, retries

Every committed change is one revision, in the same transaction as the change and a history entry.
Replies to changes carry `revision`; change events carry the same number. `mutation_id` (any
string, unique per change) makes retries safe: a change whose id is in the history (the last 10,000
revisions) is not applied again and the reply repeats the recorded result with `replayed: true`.

Every revision is announced to every connection by at least one event, in revision order, so a
skipped number means events were missed: refresh what is shown. Changes say what they are based
on, and are refused with `code: "conflict"` rather than applied over someone else's change: cell
edits carry `base` (the value the user saw), position-addressed changes `generation` (tables) or
`layout` (sheets). A refusal with `"moved": true` addressed rows or cells that moved or are gone.
The full contract, with undo, is [COLLABORATION.md](COLLABORATION.md).

### Error codes

| code | meaning |
|---|---|
| `overloaded` | queue full; retry with backoff |
| `deadline_exceeded` | did not finish in time; nothing changed unless the change had committed |
| `cancelled` | cancelled by the client |
| `too_large` | a size limit (cells per edit, rows per insert); the message names it |
| `capacity` | the server has no resources left to open the workbook |
| `unavailable` | the workbook is closing or the server is stopping; reconnect |
| `not_allowed` | the SQL policy refused the statement (see below); nothing ran |
| `sql_change` | `query` got an INSERT/UPDATE/DELETE: confirm, then send it with `sql_change` |
| `bad_parameter` | a `$name` has no value, is not used, or its value does not parse as its type |
| `schema_changed` | a materialized table's query now produces different columns; nothing changed |
| `unauthenticated` | no valid credential (or it expired) |
| `forbidden` | the caller's role does not allow the operation; nothing changed |
| `not_found` | the workbook, table, sheet or object does not exist or is not visible to the caller |
| `bad_request` | the request is malformed or names something invalid |
| `quota` | a space quota (storage, jobs, workbooks) refuses it |
| `conflict` | the change was based on what someone else changed since (cells, rows, layout); nothing changed. `moved: true`: what it addressed moved or is gone ([COLLABORATION.md](COLLABORATION.md)) |

Error messages never contain the server's data-directory path (it is shown as `<data>`). DuckDB's
`LINE n:` positions refer to the user's own SQL text.

### Limits

Messages above `--max-message` (16 MiB) close the connection. Edits and sheet writes take at most
1,000,000 cells, inserts 1,000,000 rows. Fetches return at most 5,000 rows and 250,000 cells; a reply
stops near 8 MiB and sets `truncated` (fetch the rest from `row_start + rows.length`). All requested
numbers are clamped before anything is allocated. A connection whose unsent replies exceed 16 MiB is
not read until they drain; beyond 64 MiB it is closed.

## Operations

The tables below cover the core operations. The complete list, with every parameter and result
type (including dashboards, validation, transforms, comments, find, and the `sheet_*` formatting,
layout, chart and rule operations), is `Op` and `Results` in
[`web/src/protocol.ts`](../web/src/protocol.ts).

| op | params | result |
|----|--------|--------|
| `tables` | – | `TableInfo[]` |
| `open_view` | `spec: {table, sort: [{column, desc}], filters: [{column, op, value}], hidden: [column]}` | `ViewInfo`; hidden columns are left out of fetch, export and copies |
| `update_view` | `view_id, spec` | `ViewInfo` |
| `view_info` | `view_id` | `ViewInfo` |
| `close_view` | `view_id` | `null` |
| `fetch` | `view_id, row_start, row_count, column_start?, column_count?` | `{row_start, rows, ids, column_start, truncated, generation}`; `ids` are stable record ids, `generation` the arrangement the positions belong to (also on `ViewInfo`) |
| `edit` | `view_id, edits: [{row, column, value, record?, base?}], generation?` (`null` value clears the cell; `record` targets a record id from a fetch, so the edit lands on that row even if rows moved, and fails if it was deleted; `base` is the value the user saw, `null` for empty; `generation` checks rows addressed by position, such as appends) | `{table, cells_changed, rows_appended, first_appended?}` (`first_appended`: record id of the first new row; the others follow in order). All-or-nothing: every value is validated first; typing below the last row appends rows in the same transaction; a repeated cell takes its last value; an empty list changes nothing. `conflict` when a cell no longer holds its `base` or the generation moved |
| `insert_rows` | `view_id, at, count, generation?` | `null` |
| `delete_rows` | `view_id, start, count, generation?` | `null` |
| `undo` | `revision?, redo?` | `UndoOutcome {table, undone, kind, rows}` or `null` when there is nothing to take back. Without `revision`: the caller's own latest table change (`redo`: their latest undo). `conflict` when anything it would take back changed since |
| `stats` | `view_id, column, start, end` | `{rows, count, sum?, avg?, min?, max?}`; `sum`, `min`, `max` follow the exact-value rules below |
| `add_column` | `table, name, sql_type` | `null` |
| `add_formula_column` | `table, name, formula` | `null` |
| `update_formula` | `table, name, formula` | `null` |
| `drop_column` | `table, name` | `null`; refused while formula columns use it |
| `rename_column` | `table, name, new_name` | `null`; formulas that use the column are rewritten in the same transaction |
| `query` | `sql, params?, limit?` (≤ 10,000, default 500) | `QueryResult {columns, rows, truncated, row_limit, elapsed_ms, revision, sources: [{table, data_rev}]}`; a preview, never the whole result when `truncated` |
| `create_table_from_query` | `name, sql, params?, saved?, version?` | `TableInfo`; keeps every row and records provenance (see `materialized`) |
| `create_demo_table` | `rows?` | `TableInfo` |
| `drop_table` | `table` | `null` |
| `rename_table` | `table, new_name` | new name |
| `sheets` | – | `SheetInfo[]` (`{index, name, rows, columns, layout}`; rows/columns = used range; `layout` is the revision that last moved its cells or index) |
| `sheet_create` | `name?` | `SheetInfo` |
| `sheet_rename` / `sheet_delete` | `sheet[, name]` | `null` |
| `sheet_fetch` | `sheet, row_start, row_count, column_count` (0-based) | `{row_start, rows}`; each cell `null` or `[display, align, input?]` |
| `sheet_set` | `sheet, inputs: [{row, column, value, base?}], layout?` (`""` clears; `=` starts a formula; `base` is the input the user saw). Changes that address sheet cells by position take `layout` (see `sheets`) | `SheetInfo`. All-or-nothing; one batch is one undo step |
| `sheet_undo` / `sheet_redo` | – | `bool` (whether anything changed). History is shared by everyone in the workbook and kept in memory only; a step can be undone or redone only by whoever made it (`conflict` otherwise) |
| `sheet_insert_rows` / `sheet_delete_rows` / `sheet_insert_columns` / `sheet_delete_columns` | `sheet, at, count` | `null` |
| `create_sheet_from_query` | `name, sql, params?, allow_truncation?` | `SheetCopy {sheet, rows_copied, total_rows?, truncated, row_limit}`. At most `--sheet-copy-rows` (default 100,000) rows, fewer for wide results; a larger result fails with `too_large` and creates nothing unless `allow_truncation` |
| `table_to_sheet` | `view_id, name` | `SheetCopy`; copies the view's first rows in its sort and filter |
| `table_from_view` | `view_id, name` | `TableInfo`; a snapshot table of the view's current rows |
| `saved_views` | `table?` | `SavedView[]` (`{id, name, spec, widths, updated_at}`) |
| `save_view` | `saved?, name, spec, widths?` | `SavedView`; creates, or updates the view with id `saved`. Names are unique per table |
| `rename_saved_view` / `delete_saved_view` | `saved[, name]` | `SavedView` / `null` |
| `sheet_to_table` | `sheet, name` | `TableInfo`. Reads every used row; column types come from the cells |
| `history` | `after?, limit?` (≤ 1000) | `HistoryEntry[]` (`{revision, mutation_id?, op, actor?, at_ms, summary, undo?}`), oldest first; `undo` (`{table, kind, undoes?, undone_by?}`) is present for table changes that can still be taken back |
| `schema` | – | `TableSchema[]` (`{name, row_count, columns}`), user tables only |
| `export_query` | `sql, params?, format?` (`csv` default, `csv_excel`, `parquet`, `json`; `csv_excel` puts a `'` before text starting with `=`, `+`, `-`, `@`, tab or carriage return, for opening in a spreadsheet application) | `QueryExport {download, file_name, rows, bytes, revision}`: the complete result, from one snapshot, written server-side; fetch it once from `GET /api/workbooks/{id}/downloads/{download}` within an hour |
| `sql_change` | `sql, params?` | `SqlChangeOutcome {table, kind, rows}`: `INSERT INTO t [(cols)] <query or VALUES>`, `UPDATE t SET col = expr, … [WHERE …]` or `DELETE FROM t [WHERE …]` as one change (record ids, revision, history); anything else is `not_allowed` |
| `saved_queries` | – | `SavedQuery[]` (`{id, name, kind, version, sql, params, created_at, updated_at}`), latest version each |
| `query_versions` | `saved` | `QueryVersion[]` (`{version, sql, params, created_at, revision}`) |
| `save_query` | `saved?, name, kind?` (`query`/`live`), `sql, params?` | `SavedQuery`; updating appends a version only when the SQL or parameters changed. Names are unique |
| `rename_query` / `delete_query` | `saved[, name]` | `SavedQuery` / `null` |
| `materialized` | – | `MaterializedTable[]` (`{table, query?, query_version?, sql, params, sources: [{table, data_rev, current_rev?}], revision, refreshed_at, stale, edited}`) |
| `refresh_materialized` | `table` | `MaterializedTable`; recomputes all rows in one change, or fails without changing anything |
| `cancel` | `request` | `boolean` (see above) |

Filter `op`: `eq neq gt gte lt lte contains not_contains starts_with is_empty not_empty in`.

### SQL

Parameters are `$name` placeholders with `params: [{name, type, value?}]`, `type` ∈ `text integer
decimal double boolean date timestamp`; values are strings parsed exactly by the server (no
rounding); an omitted `value` binds NULL. Positional `?`/`$1` are refused.

The SQL policy (all ops that take `sql`, and formula columns for functions): one query (`SELECT`,
`WITH`, `VALUES`, `FROM`-first, set operations, `DESCRIBE`/`SUMMARIZE` of a query) reading this
workbook's tables and its own CTEs. Refused: other statements, more than one statement, `_rf_*`
tables, other catalogs and schemas, system views, files and URLs, table functions other than
`range`, `generate_series`, `unnest`, functions reading settings/secrets/variables or running SQL
text, sequences, `PIVOT` without explicit values. Independently, each workbook's database has
external access disabled. Reads and writes run in the scheduler like other requests: `query` and
`export_query` are bulk reads, `sql_change`, `create_*_from_query` and `refresh_materialized` bulk
changes; all are cancellable.

`ViewInfo`:

```jsonc
{
  "view_id": 1,
  "spec": { "table": "flights", "sort": [], "filters": [] },
  "row_count": 26344656,
  "columns": [{ "name": "fare_usd", "sql_type": "DOUBLE", "kind": "number" },
              { "name": "per_mile", "sql_type": "DOUBLE", "kind": "number", "formula": "=[fare_usd]/[distance_mi]" }],
  "can_append": true
}
```

`kind` ∈ `int number bool date datetime time text`.

### Exact values

JSON numbers are doubles in every browser, so only values that survive that exactly are
sent as numbers:

| Stored type | Wire value |
|---|---|
| integers within ±(2^53 − 1) | JSON number |
| larger integers (BIGINT, UBIGINT, HUGEINT…) | JSON string of the exact digits, e.g. `"9007199254740993"` |
| DECIMAL | JSON string with its scale, e.g. `"1.10"` |
| FLOAT / DOUBLE | JSON number (`"NaN"`, `"inf"`, `"-inf"` as strings) |
| BOOLEAN | JSON boolean |
| DATE / TIMESTAMP / TIME | ISO text; TIMESTAMPTZ is shown in UTC (`2024-01-01 00:00:00+00`) |
| anything else | DuckDB's text form |

Edits are sent as text and rejected when they would be rounded: a fraction into an
integer column, more decimals than a DECIMAL's scale, or a value out of range.

## HTTP

| Method | Path | Notes |
|--------|------|-------|
| GET | `/api/config` | `{version, auth, setup_required, sso, allow_sql, sheet_copy_rows, max_message_bytes, max_upload_bytes, max_edit_cells, profiles}` (no auth) |
| GET/POST | `/api/workbooks` | list (with `size_bytes`, `summary`, `starred`, `space`, `role`, `opened_at`, and `compute`: `sleeping`, `queued`, `starting`, `ready` or `stopping`, read from memory without opening the workbook), or create `{name, space?}` |
| GET/PATCH/DELETE | `/api/workbooks/{id}` | info with tables and sheets; patch `{name?, space?, starred?}`; delete moves it to the trash |
| GET/DELETE | `/api/trash` | list trashed workbooks (with `deleted_at`), or empty the trash permanently |
| DELETE | `/api/trash/{id}` | delete one trashed workbook permanently |
| POST | `/api/trash/{id}/restore` | move it back; returns `WorkbookMeta` |
| POST | `/api/workbooks/{id}/duplicate` | copies tables, formulas and sheets |
| GET | `/api/workbooks/{id}/export.xlsx` | all sheets as one Excel file |
| POST | `/api/workbooks/{id}/import?filename=x.parquet[&table=name]` | raw body, streamed to a private scratch directory (limited by `--max-upload` and the temp budget); returns `{kind: "table", table}` or `{kind: "sheets", sheets}` for .xlsx. A failed or interrupted upload leaves no file and no table |
| GET | `/api/workbooks/{id}/export?spec=<json ViewSpec>&format=csv\|csv_excel\|parquet\|json` | honours sort, filters and hidden columns |
| POST | `/api/workbooks/{id}/uploads?filename=x.csv` | raw body; stages the file and returns `UploadCreated {upload_id, filename, size_bytes, preview}`. Nothing is imported yet. Unclaimed uploads expire after an hour |
| POST | `/api/workbooks/{id}/uploads/{upload}/preview` | `ImportRequest {types?}` → `UploadPreview` with the given column types |
| POST | `/api/workbooks/{id}/uploads/{upload}/import` | `ImportRequest {table?, types?, job?}` → `Imported`. On failure the upload is kept for a retry, and the error body carries `problem {message, line?, column?, value?, suggested_type?}` |
| DELETE | `/api/workbooks/{id}/uploads/{upload}` | discard a staged upload |
| GET | `/api/workbooks/{id}/downloads/{download}` | a finished `export_query` file, once; 404 once taken or expired |
| GET/DELETE | `/api/workbooks/{id}/jobs/{job}` | progress `{running, percentage?, rows_processed, total_rows}` of a running import, or cancel it (only that job; 409 if it is not running) |
| GET | `/api/workbooks/{id}/runtime` | `RuntimeStatus {workbook, spec, spec_id, policy, allocation?, ownership?, sessions, profile, note?, queue_position?, last_activity_ms?, storage_bytes}`; does not start anything and is not activity |
| POST | `/api/workbooks/{id}/runtime` | `RuntimeRequest {action: "start" \| "stop" \| "cancel" \| "restart", operation_id?, wait?, profile?}` → `RuntimeStatus`. `start` needs view, the others edit; `restart` with a different `profile` needs the space's owner role, the profile granted to the space, and an organization admin for a dedicated one (docs/COMPUTE.md). `wait: false` replies once the start is under way. Unknown fields are refused (400): requests name a profile, never resources. Repeating an `operation_id` that already moved this workbook does nothing. The workbook's data is never affected |
| GET | `/api/workbooks/{id}/profiles` | `ProfileChoice[] {profile, granted, allowed, reason?}`: the server's profiles as the caller may use them for this workbook |
| PUT | `/api/spaces/{id}/profiles` | `SpaceProfiles {profiles}` (organization admins): profiles granted to the space besides `standard`; unknown ids are 400 |
| GET | `/api/admin/resources` | host budget, each open workbook's reservation and the DuckDB limits read back from it, jobs, upload bytes |
| GET | `/api/admin/sessions` | open connections with unsent bytes |
| POST | `/api/admin/backups` | back up every workbook into `<data-dir>/.backups/backup-<ms>/` → `{path, name, manifest}` (see DURABILITY.md). One at a time (409 while one runs), at most one per `backup_interval` (409), refused without enough free disk; only the newest `backups_kept` (5) are kept |
| GET | `/api/admin/backups` | the kept backups, newest first: `[{name, created_at_ms, workbooks, bytes}]` |
| DELETE | `/api/admin/backups/{name}` | remove one backup → 204 |

A missing or trashed workbook answers **404**. Opening a workbook can fail with **409** (it needs a newer storage format or a capability this
server lacks), **423** (another process holds it), **429** (its space reached its open-workbooks or
compute quota) or **503** (no capacity after waiting in line, or it could not be opened); the body is `{"error": message}`. Uploads larger than `--max-upload` fail with **413**
(before the body is read when `Content-Length` says so); uploads that would exceed the
temporary-disk budget, or hit a full disk, fail with **507**. Busy queues give **503**, deadlines
**504**.

Auth: the session cookie (browsers) or `Authorization: Bearer <token>`; a token in the query
string is refused. Spaces, organizations, members, invitations, service tokens, libraries, sign-in
and audit routes are described in [TEAMS.md](TEAMS.md); agent connections (`/api/agents…`) and
`/mcp` in [MCP.md](MCP.md).


## Workbook functions

| Operation | Parameters | Result |
|---|---|---|
| `functions` | none | `WorkbookFunction[]` |
| `test_function` | `definition: FunctionDefinition` | `FunctionTestResult[]` |
| `publish_function` | `definition: FunctionDefinition` | `WorkbookFunction` |
| `pin_functions` | `pins: Record<string, number>` | `WorkbookFunction[]` |
| `refresh_functions` | — | `number` (the new refresh epoch): compute volatile Python functions again and retry calls that ran out of time, memory or a worker (needs `edit`) |

Python functions (`kind: "python"`, [PYTHON_FUNCTIONS.md](PYTHON_FUNCTIONS.md)) also declare `returns`
(`number`, `text`, `boolean`) and may be `volatile`; each version records the `runtime`
(`RuntimeIdentity {kind, id, version, packages}`) it was published with. Running their tests
needs `publish`. Their results arrive as `sheet_changed` (sheet `null`) and `table_changed`
events with origin 0. `GET /api/workbooks/{id}/export.xlsx?values=python` writes cells that call
them as their current values.

`FunctionDefinition` has `name`, `kind` (`formula`, `sql` or `python`), `arguments` (names), `source`,
`documentation`, `tests` (`{arguments: string[], expected: string}`), and optional `dependencies`
(a function-name/version map; omitted entries capture the active pin). Replies include canonical
names, immutable versions, exact dependency pins, adapter/DuckDB versions and the active `pinned` version.

Testing is a bulk read with isolated scratch evaluation. Publishing and pinning are bulk mutations
with the existing revision, idempotency, admission and history envelope. They broadcast `resync`
after commit. Publishing a subsequent version does not activate it. A pin map is atomic; every
dependency must match its recorded version. SQL definitions and SQL pin changes require SQL to be
enabled; sheet functions remain available when SQL is disabled. See [FUNCTIONS.md](FUNCTIONS.md)
for input semantics, limits, supported built-ins and export limitations.

## No-code analysis and table presentation

| Operation | Parameters | Result |
|---|---|---|
| `analyze` | `spec: AnalysisSpec` | `AnalysisResult` |
| `analyses` | none | `SavedAnalysis[]` |
| `save_analysis` | `saved: string \| null`, `name`, `spec`, `chart: boolean` | `SavedAnalysis` |
| `delete_analysis` | `analysis: string` | `null` |
| `materialize_analysis` | `name`, `spec` | `TableInfo` |
| `replace_preview` | `spec: ReplaceSpec` | `ReplacePreview` |
| `replace_apply` | `spec`, `revision` from preview | `ReplaceOutcome` |
| `replace_undo` | `revision` from `undo_revision` | `ReplaceOutcome` |
| `presentation` | `table` | `Presentation` |
| `save_presentation` | `table`, `presentation` | `Presentation` |

`AnalysisSpec` is a tagged `summary`, `join` or `profile` plan; its generated type is authoritative.
Analysis and replacement previews are bulk reads. Materialization, replacement and undo are bulk
writes; metadata changes use the writer queue. All writes use the existing revision/idempotency
protocol. Plan/presentation changes broadcast `resync`; replacement/undo broadcast `table_changed`;
materialization broadcasts `tables_changed`. These typed operations work when arbitrary SQL is disabled.

An analysis result holds a bounded `QueryResult` and optional join diagnostics, both read in one
snapshot. Replacement previews hold an affected-cell count and a bounded sample carrying the
snapshot revision. Apply requires that exact current revision. Undo requires both the recorded
undo revision and the current revision to match. Other mutations invalidate undo availability,
including metadata changes. Limits and exact semantics are documented in [ANALYSIS.md](ANALYSIS.md).
