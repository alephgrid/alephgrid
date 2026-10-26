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
* Storage format (`storage.rs`): `_rf_meta` (format, revision, bound workbook id), `_rf_tables`
  (next id, density, structure generation), `_rf_history` (one entry per committed change). Every
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
  keeping text as text and numbers that a double cannot hold as text.
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
* Formula columns are restricted to single expressions, but they and the SQL console
  still run DuckDB functions; `--disable-sql` hides only the console.
* The SQL console runs arbitrary DuckDB SQL. DuckDB can read local files and URLs,
  so on a shared server either trust your users or run with `--disable-sql`, inside
  a container whose filesystem holds only the data volume.
* All workbooks of a server share one process (`shared_process`): resource limits are DuckDB-level,
  not a security boundary between workbooks. See DURABILITY.md for isolation classes.
* Planned: OIDC login, per-workbook roles, DuckDB `enable_external_access=false`
  with an allow-list.
