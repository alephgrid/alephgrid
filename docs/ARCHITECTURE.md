# Architecture

```
┌──────────────────────── browser ────────────────────────┐
│  web/ (TypeScript, no framework, ~50 KB)                │
│   Grid (canvas) ── RowCache (100-row blocks, prefetch)  │
│        │                     │                          │
│        └──── Rpc (one WebSocket per workbook) ──────────┼──┐
└─────────────────────────────────────────────────────────┘  │ JSON
┌─────────────────── rowfinity (one binary) ──────────────┐  │
│  rowfinity-server (axum)                                │◀─┘
│   /ws/{id}        protocol (docs/PROTOCOL.md)           │
│   /api/…          workbooks, streamed import, export    │
│   embedded UI     web/dist via rust-embed               │
│        │ spawn_blocking                                 │
│  rowfinity-engine                                       │
│   Workbook = DuckDB file  ─ tables, views, formulas     │
└────────────────────────────┬────────────────────────────┘
                             ▼
        rowfinity-data/<workbook-id>/data.duckdb
```

## Storage

* One directory per workbook: `workbook.json` (name, created) and `data.duckdb`.
  Deleted workbooks move to `.trash/<id>/` in one rename and stay restorable until
  purged. `.uploads/` and `.exports/` hold per-request scratch directories, cleared at
  startup.
* A user table is an ordinary DuckDB table plus two hidden columns: `_rf_id`, a stable record id
  that is never reused within the table, and `_rf_ord`, the display order. Rows written in order
  get `_rf_ord = position × 2^20`; a table that was only ever appended to is *dense*, so reading
  rows 20,000,000–20,000,100 is a range predicate on `_rf_ord` that DuckDB answers from zone maps.
  After inserts or deletes in the middle, a per-table position index (chunks of ~65,536 rows, each
  identified by its lowest `_rf_ord`, with row counts) maps a position to its key with a binary
  search and one `LIMIT 1 OFFSET k` inside a chunk. Inserting takes keys between the neighbours;
  when a gap runs out, only a window around the insert point is re-spread. Nothing renumbers the
  table, so edits addressed by record id stay correct while rows move.
* Storage format 3 (`storage.rs`): `_rf_meta` (format, revision, bound workbook id), `_rf_tables`
  (next id, density, structure generation, `data_rev`: the revision that last changed the table's
  rows, columns or formulas), `_rf_history` (one entry per committed change), `_rf_queries` and
  `_rf_query_versions` (saved queries, immutable versions), `_rf_materialized` (how a table made
  from a query was computed). Every
  change runs in `Workbook::mutate`: one transaction with the change, revision + 1 and its history
  entry; a `mutation_id` already in the history is not applied again. Older files are copied to
  `data.duckdb.v<N>.bak` and migrated in one transaction; newer ones are refused.
* Formula columns are stored as metadata (`_rf_columns`), not data. Each one is a
  DuckDB expression layered onto the table's projection, so it is computed only for
  the rows being read (the visible window, an aggregate or an export). DuckDB's
  lateral column aliases let formulas reference earlier formula columns.
* Formulas are parsed with DuckDB's own parser (`json_serialize_sql`): a formula must
  be one scalar expression without subqueries. The parse tree gives the location of
  every column reference, so renaming a column rewrites dependent formulas at exactly
  those spans, and deleting a column that formulas use is refused. Both run in one
  transaction with a re-validation of the table's projection.
* Every mutation is a single DuckDB transaction and validates its input first (edits,
  appends, renames, sheet → table). Ad-hoc SQL sees formula columns through CTEs that
  shadow each table; a user's own `WITH` list is extended, not replaced.

## SQL workspace

User SQL enters through one engine function, `sqlpolicy::check_select`, on every path: the
preview (`query`), complete exports (`export_query`), query → table, query → sheet, saved queries,
refreshes, SQL changes and (for functions) formula columns.

* **Policy, from the parse tree.** The statement is parsed by DuckDB's own parser
  (`json_serialize_sql`, which never binds or runs anything) and walked: exactly one query;
  relations are this workbook's user tables or CTEs in scope (scope-aware; a CTE may not reuse a
  system relation's name), never `_rf_*`, other catalogs/schemas, system views, files or URLs;
  table functions from an allow-list (`range`, `generate_series`, `unnest`); no functions that
  read settings, secrets or variables, run or plan SQL text, or touch sequences; named `$params`
  only. The `duckdb` crate's `prepare` runs every statement but the last of a multi-statement
  string, so this check happens before user text is embedded anywhere, and user text is always
  embedded on lines of its own inside parentheses. Table operations (`open_view`, `add_column`,
  `drop_table`, exports …) refuse internal table names too.
* **Database boundary.** When a workbook is opened by the server, its DuckDB instance gets
  `allowed_directories` = that workbook's `.uploads/<id>/`, `.exports/<id>/` and spill directory,
  then `enable_external_access = false` (irreversible while open), with extension auto-install,
  auto-load and community extensions off. Imports, exports, online backups and duplicates stage
  their files in those directories (backups and duplicates then move the file into place).
* **Parameters** are parsed exactly in Rust (integers as i128, decimals as scaled i128, dates,
  timestamps) and bound through the prepared-statement API with that type; values never become
  SQL text. Identifiers are validated and quoted with `sql::ident`.
* **Previews and exports.** A preview reads at most `limit` (≤ 5,000) rows and ~8 MiB in one
  snapshot and reports `truncated`, the row limit, the time taken, the revision it read and each
  source table's data revision. An export runs `COPY (…) TO` inside the workbook's export
  directory from one snapshot (DuckDB streams it; nothing is held in memory), and the file is
  downloaded once (`/api/workbooks/{id}/downloads/{download}`), then deleted; unclaimed files
  expire after an hour. A read cancelled as it finishes publishes nothing (its file is removed).
* **Saved SQL, live queries, materialized results** are three different things: saved queries are
  text + parameters with immutable versions; a *live* saved query is re-run on current data
  whenever it is opened and its result goes stale when a source table's data revision moves; a
  table created from a query is a snapshot with provenance (SQL, parameters, sources and their
  data revisions, the revision it was computed at), listed with `stale` and `edited`, and
  refreshed only on request, all or nothing, with new record ids. A refresh whose result columns
  differ fails with `schema_changed` and changes nothing.
* **SQL changes.** `INSERT INTO`, `UPDATE … SET … WHERE` and `DELETE FROM … WHERE` are split into
  their parts by a small tokenizer (`sqllex.rs`, used for routing only) and reassembled into one
  SELECT that must pass the policy *and* have exactly the expected shape. That SELECT runs once
  into a staging table, values are checked to convert to the target types without rounding, and
  the change is applied like an edit, insert or delete: record ids, order keys and the position
  index are maintained, one revision and history entry, `mutation_id` replay. Everything else
  that changes data or schema is refused (`not_allowed`).

## Sheets (IronCalc)

Sheets are the "human-scale" tier: thousands of cells with Excel formulas, styling
and undo. All sheets of a workbook live in a single IronCalc `UserModel`, so
cross-sheet references work. The model is serialized into the `_rf_sheets` BLOB in
the same DuckDB file, so a workbook is still one file you can copy or back up.

* Edits go through `UserModel::set_user_input`, with evaluation paused for bulk
  edits and resumed once. Every coordinate is validated before the first change. If
  a change fails part-way or the blob cannot be saved, the model is reloaded from the
  file, so memory never runs ahead of what is durable.
* Undo/redo use IronCalc's history, grouped so that one user operation (a paste, an
  import) is one step. The history is in memory, shared by everyone in the workbook,
  and lost on restart or after a reload.
* `sheet_fetch` walks only the cells that exist in each row (`cells_in_row`) and
  returns `[display, align, input?]`, so the client never re-implements number
  formatting or formula evaluation.
* Bridges: `create_sheet_from_query` writes a bounded SQL result into a new sheet,
  keeping text as text and numbers that a double cannot hold as text. A result larger than the
  limit is refused (`too_large`) unless the request agrees to copy only the first rows.
  `sheet_to_table` reads every used row and infers each column's type from the cell
  values (numbers, dates, booleans, otherwise displayed text).
* `.xlsx` import and export use the `ironcalc` crate's reader and writer. Importing
  into an empty workbook adopts the model with its styles; otherwise cell inputs
  are copied into new sheets.

Why not one engine for both tiers? IronCalc keeps every cell in memory (great for
models, wrong for 100M rows). DuckDB is columnar and set-based (great for 100M rows,
wrong for `=B4*C2`). Rowfinity keeps both and makes moving data between them one click.

## Workspaces

`workspaces.json` in the data dir lists workspaces. Each `workbook.json` carries
`workspace`, `starred`, `opened_at`, `updated_at` and a cached `summary` (table,
sheet and row counts) for the home screen. Rename, star and move rewrite only that
JSON file, without opening the database. The server refreshes the summary when a
workbook is opened or its structure changes. Duplicate checkpoints the DuckDB file
while holding the workbook lock, then copies it.

## Imports

Uploads are staged first: the file streams into its own `.uploads/<id>/` directory and
the engine returns a preview (detected columns and the first 20 rows) without creating
anything. Type overrides are applied in DuckDB's CSV reader (so text such as `007` stays
exact) or as casts for Parquet/JSON, from an allow-list of types. The import runs with
a client-chosen job id: the server reports DuckDB's progress for it and can interrupt
it, and only that job. Reader errors become structured problems (line, column, value,
suggested type), so the UI can offer the fix and retry the same upload. `.xlsx` files
preview the sheets they would add and what will not come across (charts, pivots, macros,
links, comments, unsupported functions).

## Views

A view is `(table, sort keys, filters)`.

* **Identity view** (no sort or filter): rows come straight from the table by
  `_rf_id` range. Nothing is materialized. Typing below the last row appends rows.
* **Sorted or filtered view**: the engine materializes only a permutation
  `(_rf_pos, _rf_id)` in one parallel sort, into an in-memory scratch database attached to every
  connection of the workbook (so readers can use it). A window is fetched in two steps:
  positions → ids, then `WHERE _rf_id IN (…)` on the base table. Ties sort by display order.
* Materializations and position indexes are caches stamped with the table's structure
  generation, read in the same snapshot as the data; a stale one is rebuilt on next use, so a read
  never mixes an old order with new rows. Unused materializations expire (`--view-ttl`) and are
  rebuilt transparently; scratch tables are dropped when nothing references them.
* Row inserts or deletes and schema changes make views on that table stale (rebuilt on next use).
  The server broadcasts `table_changed` so other clients refresh. A rename rewrites open
  views' column references; references to dropped columns are removed.
* **Saved views** (`_rf_views`) store a spec (sort, filters, hidden columns) and column
  widths per table. They are live: opening one re-applies the spec to current data.
  Renames rewrite them; a column a saved view sorts or filters by cannot be dropped.
  A snapshot is a separate, explicit action that copies the view's rows into a new table.
* Copies into sheets are bounded (`--sheet-copy-rows`, and one edit's cell budget) and
  report what was left out.

## Wire protocol

JSON over one WebSocket per workbook; see [PROTOCOL.md](PROTOCOL.md). Rows are
sent row-major. Values are exact: integers beyond ±(2^53 − 1) and decimals travel as
strings, everything else as JSON numbers, booleans or ISO text (`wire.rs`). The
client formats them. The TypeScript types are generated from the Rust ones
(`protocol.rs` → `web/src/protocol.ts`), and a test fails when they drift. Measured:
6.9 bytes per cell on the demo table, uncompressed; frames aren't compressed yet.

## Runtime, ownership and lifecycle

A workbook's identity and data live in its directory. The compute that serves it is an
**allocation** (`runtime/`): an engine opened by a provider under a `ComputePolicy`, pinned to an
immutable, content-addressed `RuntimeSpec` (engine version, storage format, components,
capabilities). The only provider so far, **Local**, opens the workbook inside the server process
(isolation class `shared_process`) and holds an exclusive `flock` on `<workbook>/.owner.lock` for the
allocation's lifetime. The **controller** starts allocations on first use (concurrent opens share one
start), records every transition (`requested → starting → running → ready → draining → stopped`, or
`failed`) in `.control/allocations/<id>.json` with operation ids, cancels starts, drains and stops,
and after a restart marks the previous process's allocations lost and removes their scratch. A
workbook held by another process is refused, never opened twice. See DURABILITY.md.

A live allocation (`WorkbookHandle`) has a writer connection, two reader connections and a control
connection. The **host budget** (memory, threads, temp disk) is split equally among live
allocations and applied as DuckDB settings, shrinking others before a new one opens; idle
workbooks are closed (`--idle-timeout`) or evicted to make room, otherwise opening fails with 503.

Deleting a workbook blocks new allocations, stops the live one (draining its work) and renames the
directory into the trash. Shutdown stops admitting work, drains every allocation within
`--shutdown-grace`, checkpoints and records allocations as stopped.

## Client

* `Grid` draws only visible cells on one canvas. Browsers cap element height at
  around 33 M px, so the native scrollbar is scaled to stay under 8 M px. Wheel and
  keyboard scrolling are tracked in exact logical pixels, so precision holds at row
  900,000,000.
* `RowCache` fetches only visible columns (see web/src/cache.ts for its byte bound) and at most 4 blocks at a time.
  Blocks that scrolled away before their turn are dropped from the queue. After a
  remote edit, stale blocks stay on screen until fresh ones arrive (no flicker).
* Edits are optimistic: the cached cell updates at once and is reconciled after the
  server confirms.

## Concurrency

Each request is a job (`sched.rs`). Reads run on reader connections, each in its own snapshot,
and never wait for a change; one reader is kept for interactive reads while bulk reads (sorts,
queries, exports) use the others. Changes run one at a time on the writer. Interactive jobs go
first, bulk jobs that waited 2 s go before them, connections take turns; queues are bounded per
workbook and per connection, jobs have deadlines and can be cancelled. Each connection's unsent
replies are bounded (backpressure, then disconnect). DuckDB parallelizes each query internally.

## Security model

* By default it binds to `127.0.0.1`: only you can reach it.
* `--token` (or `ROWFINITY_TOKEN`) enables a shared bearer token for the API and
  WebSocket.
* User SQL and formulas are checked by the SQL policy (below) and run in a database that is
  closed to the outside world. `--disable-sql` additionally refuses every user-SQL operation.
* Saved queries live in the workbook and are visible to anyone who can open it. There are no
  per-user identities yet (phase 7), so nothing is private beyond that; the unsaved editor draft
  stays in the browser that typed it.
* All workbooks of a server share one process (`shared_process`): resource limits are DuckDB-level,
  not a security boundary between workbooks. See DURABILITY.md for isolation classes.
* Planned: OIDC login, per-workbook and per-space roles (phase 7). Completing the SQL
  restrictions is not organization authorization.
