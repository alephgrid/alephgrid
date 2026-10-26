# WebSocket protocol (v0.2)

Connect to `ws(s)://<host>/ws/<workbook-id>` (add `?token=…` when the server uses a
token). Every frame is a JSON text message.

The TypeScript types in [`web/src/protocol.ts`](../web/src/protocol.ts) are generated from
the Rust types and are the precise definition of every request, event and result
(`Op`, `Event`, `Results`). `cargo test` fails when that file is stale; regenerate with
`UPDATE_BINDINGS=1 cargo test -p rowfinity-server --test protocol`.

## Envelope

```jsonc
// client → server
{ "id": 7, "op": "fetch", "view_id": 1, "row_start": 0, "row_count": 100 }
// server → client
{ "id": 7, "ok": true,  "result": { "row_start": 0, "rows": [[…], …] } }
{ "id": 7, "ok": false, "error": "unknown column \"x\"" }
// server push (no id)
{ "event": "hello", "session": 3, "workbook": { "id": "…", "name": "…", "created_at": 0 } }
{ "event": "cells_changed",  "table": "flights", "origin": 3 }
{ "event": "table_changed",  "table": "flights", "origin": 3 }  // rows inserted/deleted, columns changed
{ "event": "tables_changed", "origin": 3 }   // tables or sheets added/removed/renamed
{ "event": "sheet_changed",  "sheet": 0, "origin": 3 }  // sheet: null after undo/redo
{ "event": "workbook_deleted", "origin": 0 }  // moved to the trash; the server then closes the socket
```

Undo/redo sends `tables_changed` instead of `sheet_changed` when it adds, removes or renames a sheet.

Requests can be pipelined; replies may arrive out of order and are matched by `id`.

## Operations

| op | params | result |
|----|--------|--------|
| `tables` | – | `TableInfo[]` |
| `open_view` | `spec: {table, sort: [{column, desc}], filters: [{column, op, value}]}` | `ViewInfo` |
| `update_view` | `view_id, spec` | `ViewInfo` |
| `view_info` | `view_id` | `ViewInfo` |
| `close_view` | `view_id` | `null` |
| `fetch` | `view_id, row_start, row_count` (≤ 5000) | `{row_start, rows}` |
| `edit` | `view_id, edits: [{row, column, value}]` (`null` value clears the cell) | `{table, cells_changed, rows_appended}`. All-or-nothing: every value is validated first; typing below the last row appends rows in the same transaction; a repeated cell takes its last value; an empty list changes nothing |
| `insert_rows` | `view_id, at, count` | `null` |
| `delete_rows` | `view_id, start, count` | `null` |
| `stats` | `view_id, column, start, end` | `{rows, count, sum?, avg?, min?, max?}`; `sum`, `min`, `max` follow the exact-value rules below |
| `add_column` | `table, name, sql_type` | `null` |
| `add_formula_column` | `table, name, formula` | `null` |
| `update_formula` | `table, name, formula` | `null` |
| `drop_column` | `table, name` | `null`; refused while formula columns use it |
| `rename_column` | `table, name, new_name` | `null`; formulas that use the column are rewritten in the same transaction |
| `query` | `sql, limit?` | `{columns, rows, truncated}` |
| `create_table_from_query` | `name, sql` | `TableInfo` |
| `create_demo_table` | `rows?` | `TableInfo` |
| `drop_table` | `table` | `null` |
| `rename_table` | `table, new_name` | new name |
| `sheets` | – | `SheetInfo[]` (`{index, name, rows, columns}`; rows/columns = used range) |
| `sheet_create` | `name?` | `SheetInfo` |
| `sheet_rename` / `sheet_delete` | `sheet[, name]` | `null` |
| `sheet_fetch` | `sheet, row_start, row_count, column_count` (0-based) | `{row_start, rows}`; each cell `null` or `[display, align, input?]` |
| `sheet_set` | `sheet, inputs: [{row, column, value}]` (`""` clears; `=` starts a formula) | `SheetInfo`. All-or-nothing; one batch is one undo step |
| `sheet_undo` / `sheet_redo` | – | `bool` (whether anything changed). History is shared by everyone in the workbook and kept in memory only |
| `sheet_insert_rows` / `sheet_delete_rows` / `sheet_insert_columns` / `sheet_delete_columns` | `sheet, at, count` | `null` |
| `create_sheet_from_query` | `name, sql` (≤ 100,000 rows) | `SheetInfo` |
| `sheet_to_table` | `sheet, name` | `TableInfo`. Reads every used row; column types come from the cells |

Filter `op`: `eq neq gt gte lt lte contains not_contains starts_with is_empty not_empty`.

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
| GET | `/api/config` | `{version, auth_required, allow_sql}` (no auth) |
| GET/POST | `/api/workbooks` | list (with `size_bytes`, `summary`, `starred`, `workspace`, `opened_at`), or create `{name, workspace?}` |
| GET/PATCH/DELETE | `/api/workbooks/{id}` | info with tables and sheets; patch `{name?, workspace?, starred?}` (`workspace: ""` = unfiled); delete moves it to the trash |
| GET/DELETE | `/api/trash` | list trashed workbooks (with `deleted_at`), or empty the trash permanently |
| DELETE | `/api/trash/{id}` | delete one trashed workbook permanently |
| POST | `/api/trash/{id}/restore` | move it back; returns `WorkbookMeta` |
| POST | `/api/workbooks/{id}/duplicate` | copies tables, formulas and sheets |
| GET | `/api/workbooks/{id}/export.xlsx` | all sheets as one Excel file |
| GET/POST | `/api/workspaces` | list, or create `{name}` |
| PATCH/DELETE | `/api/workspaces/{id}` | rename `{name}`; delete keeps its workbooks (they become unfiled) |
| POST | `/api/workbooks/{id}/import?filename=x.parquet[&table=name]` | raw body, streamed to a private scratch directory (no size limit yet); returns `{kind: "table", table}` or `{kind: "sheets", sheets}` for .xlsx. A failed or interrupted upload leaves no file and no table |
| GET | `/api/workbooks/{id}/export?spec=<json ViewSpec>&format=csv\|parquet\|json` | honours sort and filters |

Auth: `Authorization: Bearer <token>` or `?token=<token>`.
