# WebSocket protocol (v0.1)

Connect to `ws(s)://<host>/ws/<workbook-id>` (add `?token=…` when the server uses a
token). Every frame is a JSON text message.

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
```

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
| `edit` | `view_id, edits: [{row, column, value}]` (`null` value clears the cell) | `{table, cells_changed, rows_appended}` |
| `insert_rows` | `view_id, at, count` | `null` |
| `delete_rows` | `view_id, start, count` | `null` |
| `stats` | `view_id, column, start, end` | `{rows, count, sum?, avg?, min?, max?}` |
| `add_column` | `table, name, sql_type` | `null` |
| `add_formula_column` | `table, name, formula` | `null` |
| `update_formula` | `table, name, formula` | `null` |
| `drop_column` | `table, name` | `null` |
| `rename_column` | `table, name, new_name` | `null` |
| `query` | `sql, limit?` | `{columns, rows, truncated}` |
| `create_table_from_query` | `name, sql` | `TableInfo` |
| `create_demo_table` | `rows?` | `TableInfo` |
| `drop_table` | `table` | `null` |
| `rename_table` | `table, new_name` | new name |
| `sheets` | – | `SheetInfo[]` (`{index, name, rows, columns}`; rows/columns = used range) |
| `sheet_create` | `name?` | `SheetInfo` |
| `sheet_rename` / `sheet_delete` | `sheet[, name]` | `null` |
| `sheet_fetch` | `sheet, row_start, row_count, column_count` (0-based) | `{row_start, rows}`; each cell `null` or `[display, align, input?]` |
| `sheet_set` | `sheet, inputs: [{row, column, value}]` (`""` clears; `=` starts a formula) | `SheetInfo` |
| `sheet_undo` / `sheet_redo` | – | `bool` (whether anything changed) |
| `sheet_insert_rows` / `sheet_delete_rows` / `sheet_insert_columns` / `sheet_delete_columns` | `sheet, at, count` | `null` |
| `create_sheet_from_query` | `name, sql` (≤ 100,000 rows) | `SheetInfo` |
| `sheet_to_table` | `sheet, name` | `TableInfo` |

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

`kind` ∈ `int number bool date datetime time text`. Values arrive as JSON numbers,
booleans, strings (dates and times in ISO form) or `null`.

## HTTP

| Method | Path | Notes |
|--------|------|-------|
| GET | `/api/config` | `{version, auth_required, allow_sql}` (no auth) |
| GET/POST | `/api/workbooks` | list (with `size_bytes`, `summary`, `starred`, `workspace`, `opened_at`), or create `{name, workspace?}` |
| GET/PATCH/DELETE | `/api/workbooks/{id}` | info with tables and sheets; patch `{name?, workspace?, starred?}` (`workspace: ""` = unfiled); delete |
| POST | `/api/workbooks/{id}/duplicate` | copies tables, formulas and sheets |
| GET | `/api/workbooks/{id}/export.xlsx` | all sheets as one Excel file |
| GET/POST | `/api/workspaces` | list, or create `{name}` |
| PATCH/DELETE | `/api/workspaces/{id}` | rename `{name}`; delete keeps its workbooks (they become unfiled) |
| POST | `/api/workbooks/{id}/import?filename=x.parquet[&table=name]` | raw body, streamed to disk, no size limit; returns `{kind: "table", table}` or `{kind: "sheets", sheets}` for .xlsx |
| GET | `/api/workbooks/{id}/export?spec=<json ViewSpec>&format=csv\|parquet\|json` | honours sort and filters |

Auth: `Authorization: Bearer <token>` or `?token=<token>`.
