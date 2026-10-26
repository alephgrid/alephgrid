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
* A user table is an ordinary DuckDB table plus a hidden `_rf_id BIGINT`. The
  engine keeps `_rf_id` **dense** (0…n-1) and in insertion order: inserts and
  deletes renumber. So `_rf_id` *is* the row position of the unsorted table, and
  reading rows 20,000,000–20,000,100 is a range predicate that DuckDB answers from
  per-row-group min/max (zone maps) without scanning the rest.
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

## Views

A view is `(table, sort keys, filters)`.

* **Identity view** (no sort or filter): rows come straight from the table by
  `_rf_id` range. Nothing is materialized. Typing below the last row appends rows.
* **Sorted or filtered view**: the engine materializes only a permutation,
  `TEMP TABLE (_rf_pos, _rf_id)`, in one parallel sort. A window is fetched in two
  steps: positions → ids (a zone-map range scan on `_rf_pos`), then
  `WHERE _rf_id IN (…)` on the base table, so DuckDB reads full columns only for
  the vectors that contain those ids. Edits change ids' values, never membership,
  so a view stays valid until its spec changes (same as filter views in Sheets).
* Row inserts or deletes and schema changes rebuild every view on that table. The
  server broadcasts `table_changed` so other clients refresh.

## Wire protocol

JSON over one WebSocket per workbook; see [PROTOCOL.md](PROTOCOL.md). Rows are
sent row-major. Values are exact: integers beyond ±(2^53 − 1) and decimals travel as
strings, everything else as JSON numbers, booleans or ISO text (`wire.rs`). The
client formats them. The TypeScript types are generated from the Rust ones
(`protocol.rs` → `web/src/protocol.ts`), and a test fails when they drift. Measured:
6.9 bytes per cell on the demo table, uncompressed; frames aren't compressed yet.

## Workbook lifecycle

A `WorkbookHandle` owns the open DuckDB connection behind a mutex. Deleting a workbook
marks the handle deleted (new operations fail with "this workbook was deleted"), takes
the mutex (so the running operation finishes first), checkpoints, drops the connection
and renames the directory into the trash. While that happens the id is in a "closing"
set, so nothing can reopen the file. Connected clients get `workbook_deleted` and
the server ends their sessions without waiting for them.

## Client

* `Grid` draws only visible cells on one canvas. Browsers cap element height at
  around 33 M px, so the native scrollbar is scaled to stay under 8 M px. Wheel and
  keyboard scrolling are tracked in exact logical pixels, so precision holds at row
  900,000,000.
* `RowCache` keeps up to 400 blocks of 100 rows and fetches at most 4 at a time.
  Blocks that scrolled away before their turn are dropped from the queue. After a
  remote edit, stale blocks stay on screen until fresh ones arrive (no flicker).
* Edits are optimistic: the cached cell updates at once and is reconciled after the
  server confirms.

## Concurrency (v0.2)

Each workbook has one DuckDB connection behind a mutex, and operations run on the
blocking pool. DuckDB parallelizes each query internally, so a single sort uses
every core. Fetches from different users of one workbook queue behind each other.
The planned fix is a pool of read connections, with permutations stored in
per-session schemas instead of TEMP tables.

## Security model (v0.2)

* By default it binds to `127.0.0.1`: only you can reach it.
* `--token` (or `ROWFINITY_TOKEN`) enables a shared bearer token for the API and
  WebSocket.
* Formula columns are restricted to single expressions, but they and the SQL console
  still run DuckDB functions; `--disable-sql` hides only the console.
* The SQL console runs arbitrary DuckDB SQL. DuckDB can read local files and URLs,
  so on a shared server either trust your users or run with `--disable-sql`, inside
  a container whose filesystem holds only the data volume.
* Planned: OIDC login, per-workbook roles, DuckDB `enable_external_access=false`
  with an allow-list.
