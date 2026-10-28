# Rowfinity

**An open-source spreadsheet for data that doesn't fit in a spreadsheet.**
Open CSV and Parquet files with hundreds of millions of rows, then scroll, sort,
filter, edit, add formula columns and query them with SQL, in your browser. Next to
those big tables, build ordinary Excel-style sheets with real formulas. It runs
on your laptop or your own server. Your data never leaves it, and there are no seats,
tiers or paywalls.

```bash
rowfinity flights.parquet          # opens http://127.0.0.1:7878 with the file loaded
rowfinity --demo-rows 100000000    # or try it on 100M generated rows
```

Rowfinity is a self-hostable alternative to cloud big-data spreadsheets: your data
and credentials stay on your machines, workbooks open without cold starts, and
export, SSO and every other feature are free.

## Why it's fast

* **DuckDB underneath.** Every workbook is one DuckDB file: columnar, compressed,
  and parallel across all cores. Imports are DuckDB's own CSV/Parquet/JSON readers.
* **Nothing is "loaded".** Opening a workbook does not read the data into memory. The
  grid asks only for the ~40 rows on screen, and a hidden dense row id makes any
  window a zone-map-pruned range read, even at row 900,000,000.
* **Sorting materializes a permutation, not a copy.** Formula columns are lazy
  expressions, computed only for rows you look at, aggregate or export.
* **Canvas grid with block caching.** About 50 KB of TypeScript, no framework, and
  a scrollbar that keeps precision past the browser's height limit.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for details.

### Measured (20M-row table, 9 columns, debug build, 8-core / 8 GB laptop)

| Operation | Time |
|---|---|
| Generate 20M rows | 8.7 s |
| Reopen the workbook after a restart | 3 ms (no rehydrate) |
| Fetch 100 rows at row 0 / 10M / 19.99M | 3–11 ms |
| Filter `carrier = DL AND origin = SEA` (125k matches) | 34 ms |
| Sort 20M rows by a float column | 0.95 s |
| Fetch 100 rows from the sorted view | ~100 ms |
| Sum/avg/min/max of a column over all 20M rows | 28 ms |
| GROUP BY carrier via SQL | 39–57 ms |
| Edit 1 cell / paste 1,000 cells | 3 ms / 43 ms |
| Insert / delete a row mid-table | 312 ms / 173 ms |

Reproduce with `rowfinity --demo-rows 20000000` and the script in `docs/bench.mjs`.

## Features (v0.1)

| | |
|---|---|
| Import | CSV/TSV (auto-detected), Parquet, JSON/JSONL, gzip as tables; **XLSX as editable sheets**. Drag and drop or CLI. Streamed, no size limit |
| Grid | Virtual canvas grid, keyboard navigation, range selection, column resize and auto-fit, light and dark themes |
| Edit | Typed cell edits with validation, append by typing below the last row, insert and delete rows, paste TSV from Excel or Sheets, copy ranges |
| Columns | Add, rename and delete columns; **formula columns** (`=[fare_usd] / [distance_mi]`, any DuckDB expression) |
| Views | Multi-column sort, filters (equals, contains, greater than, empty, …) applied to the whole dataset |
| Analysis | Live sum, avg, count, min and max of the selection; column summary; **SQL workspace** (schema browser, autocomplete, `$parameters`, cancellable previews, complete exports, saved and live queries with versions, refreshable query tables, INSERT/UPDATE/DELETE that keep history); SQL reads the workbook's own tables only |
| **Sheets** | Excel-style formula grids on [IronCalc](https://github.com/ironcalc/IronCalc): `=SUM`, `XLOOKUP`, cross-sheet references, undo/redo, insert/delete rows and columns with reference shifting. Stored in the same workbook file |
| Bridges | SQL result → sheet, sheet → columnar table (types sniffed), all sheets → .xlsx |
| Export | CSV, Parquet or JSON lines for any table or the current filtered and sorted view; .xlsx for sheets. Free |
| Workspaces | Group workbooks by team or project, star, recent, search, sort by size or date, rename, duplicate, move, delete |
| Collaboration | Several people on one workbook see each other's edits live |
| Hosting | One binary with the UI embedded, Docker image, token auth, data in a plain directory |

## Quick start

### From source

Requires Rust 1.88+ and Node 20+.

```bash
git clone https://github.com/<you>/rowfinity && cd rowfinity
(cd web && npm install && npm run build)       # build the UI (embedded into the binary)
cargo build --release
./target/release/rowfinity --demo-rows 10000000
```

The first build compiles DuckDB from source, which takes several minutes.

### Docker (for a team)

```bash
docker compose up -d        # edit ROWFINITY_TOKEN in docker-compose.yml first
open "http://localhost:7878/?token=change-me"
```

### Options

| Flag | Env | Default |
|------|-----|---------|
| `--data-dir` | `ROWFINITY_DATA_DIR` | `./rowfinity-data` |
| `--host` / `--port` | `ROWFINITY_HOST` / `ROWFINITY_PORT` | `127.0.0.1` / `7878` |
| `--token` | `ROWFINITY_TOKEN` | none (only safe on localhost) |
| `--memory-limit` | `ROWFINITY_MEMORY_LIMIT` | DuckDB default (80% of RAM, spills to disk) |
| `--threads` | `ROWFINITY_THREADS` | all cores |
| `--disable-sql` | `ROWFINITY_DISABLE_SQL` | SQL console enabled |
| `--demo-rows N` | | generate a demo workbook |

## Formulas

Formula columns use DuckDB's SQL expression language. `[Column Name]` refers to a
column, and the leading `=` is optional:

```
=[fare_usd] / [distance_mi]
=CASE WHEN [dep_delay_min] > 15 THEN 'late' ELSE 'on time' END
=strftime([flight_date], '%Y-%m')
=[origin] || '→' || [dest]
```

Formula columns can be sorted and filtered on, and can reference earlier formula
columns.

For free-form work, add a **sheet** (the `+` button next to the tabs). Sheets use
Excel syntax (`=SUM(B2:B9)`, `=XLOOKUP(...)`, `=Budget!B4*2`) and are evaluated by
IronCalc. Typical flow: aggregate 100M rows with SQL, choose *Open in sheet*, then
model on top of the result.

### User-defined functions

Choose **Formula help** to search built-in availability, signatures and tested examples. The formula bar
suggests 44 reviewed sheet functions, including `XLOOKUP`, `VLOOKUP` and `SUMIFS`.
See the [catalog scope](docs/FORMULA_CATALOG.md): registry recognition is distinct from full compatibility,
and spreadsheet-style formulas over DuckDB tables remain planned.

Choose **Functions** in a workbook to define and test reusable sheet formulas or SQL scalar
expressions. Call `=MARGIN(B2,C2)` in a sheet, or a SQL function in queries and calculated columns.
Definitions have immutable versions, explicit upgrade/rollback and pinned dependencies.
See [the function guide](docs/FUNCTIONS.md) for examples, supported expressions and XLSX limitations.

### No-code analysis

Use **Analyze** to build and save summaries, pivots, charts, joins and column profiles without
writing SQL. **Find / replace** previews text changes and offers guarded undo; **Format table**
saves number/date formats, hidden/frozen columns and conditional rules. Results remain bounded
and calculations run on the server. See [the analysis guide](docs/ANALYSIS.md) for workflows and limits.

## Development

```bash
cargo run -p rowfinity-server -- --demo-rows 1000000   # API on :7878
cd web && npm run dev                                  # UI with hot reload on :5173 (proxies to :7878)
cargo test --workspace
```

Layout:

```
crates/rowfinity-engine   DuckDB tables, views, formula columns, IronCalc sheets (pure Rust library)
crates/rowfinity-server   axum HTTP + WebSocket server, CLI, embedded UI
web/                      TypeScript client (canvas grid, row cache, UI)
docs/                     architecture, protocol, roadmap
```

## Roadmap

See [docs/ROADMAP.md](docs/ROADMAP.md). Highlights: login and per-workspace roles,
sheet formulas that read tables, pivot tables, charts, Postgres,
Snowflake, BigQuery and S3 connectors (DuckDB extensions), OIDC login with roles, a
desktop app (Tauri) and a pure in-browser mode (DuckDB-WASM).

## License

[Apache-2.0](LICENSE). Use it, host it and build on it, commercially or not.
