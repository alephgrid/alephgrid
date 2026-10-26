// Generated from the Rust types in crates/rowfinity-engine and crates/rowfinity-server.
// Do not edit: run `UPDATE_BINDINGS=1 cargo test -p rowfinity-server --test protocol`.

/** A cell as sent by the server: integers beyond ±(2^53 - 1) and decimals arrive as exact strings. */
export type CellValue = string | number | boolean | null;

/** One rendered sheet cell: [display, align, input?]; input is present when it differs from display. */
export type SheetCell = [string, Align, string?];

/** A WebSocket request frame. */
export type Request = Op & { id: number };

/** A WebSocket reply frame. */
export type Reply = { id: number; ok: true; result: unknown } | { id: number | null; ok: false; error: string };

/** Pushed after connecting. */
export type Hello = { event: "hello"; session: number; workbook: WorkbookMeta };

export type Kind = "int" | "number" | "bool" | "date" | "datetime" | "time" | "text";

export type Column = { name: string, sql_type: string, kind: Kind, 
/**
 * Present for computed (formula) columns.
 */
formula?: string, };

export type TableInfo = { name: string, row_count: number, column_count: number, };

export type SortKey = { column: string, desc: boolean, };

export type FilterOp = "eq" | "neq" | "gt" | "gte" | "lt" | "lte" | "contains" | "not_contains" | "starts_with" | "is_empty" | "not_empty";

export type Filter = { column: string, op: FilterOp, value: CellValue, };

export type ViewSpec = { table: string, sort: Array<SortKey>, filters: Array<Filter>, 
/**
 * Columns left out of the view (and of its exports); the data is untouched.
 */
hidden: Array<string>, };

export type SavedView = { id: string, name: string, spec: ViewSpec, 
/**
 * Column widths in pixels, by column name.
 */
widths: Record<string, number>, updated_at: number, };

export type ViewInfo = { view_id: number, spec: ViewSpec, row_count: number, columns: Array<Column>, 
/**
 * True when rows can be appended by typing below the last row.
 */
can_append: boolean, };

export type RowBlock = { row_start: number, rows: CellValue[][], };

export type CellEdit = { row: number, column: string, 
/**
 * `None` or an empty string clears the cell.
 */
value: string | null, };

export type EditOutcome = { table: string, cells_changed: number, rows_appended: number, };

export type Stats = { rows: number, count: number, 
/**
 * Exact: a number, or a string for integers beyond 2^53 and for decimals.
 */
sum?: number | string, avg?: number, min?: CellValue, max?: CellValue, };

export type QueryPreview = { columns: Array<Column>, rows: CellValue[][], truncated: boolean, };

export type ExportFormat = "csv" | "parquet" | "json";

export type Align = "left" | "right" | "center";

export type SheetInfo = { index: number, name: string, 
/**
 * Used range, 0-based exclusive bounds.
 */
rows: number, columns: number, };

export type SheetBlock = { row_start: number, 
/**
 * `rows[i][j]` is `null` for empty cells.
 */
rows: (SheetCell | null)[][], };

export type SheetInput = { row: number, column: number, 
/**
 * Text exactly as typed: `42`, `hello`, `=SUM(A1:A9)`. Empty clears the cell.
 */
value: string, };

export type SheetCopy = { sheet: SheetInfo, rows_copied: number, 
/**
 * Rows in the source, when known without running the query to the end.
 */
total_rows?: number, 
/**
 * True when rows were left out because of `row_limit`.
 */
truncated: boolean, row_limit: number, };

export type Imported = { "kind": "table", table: TableInfo, } | { "kind": "sheets", sheets: Array<SheetInfo>, warnings: Array<string>, };

export type Summary = { tables: number, sheets: number, rows: number, };

export type WorkbookMeta = { id: string, name: string, created_at: number, workspace: string | null, starred: boolean, opened_at: number, updated_at: number, summary: Summary, 
/**
 * Set while the workbook is in the trash.
 */
deleted_at?: number, };

export type WorkbookListing = { size_bytes: number, id: string, name: string, created_at: number, workspace: string | null, starred: boolean, opened_at: number, updated_at: number, summary: Summary, 
/**
 * Set while the workbook is in the trash.
 */
deleted_at?: number, };

export type WorkbookPatch = { name?: string, 
/**
 * Workspace id; an empty string moves the workbook out of any workspace.
 */
workspace?: string, starred?: boolean, };

export type Workspace = { id: string, name: string, created_at: number, };

export type Event = { "event": "cells_changed", table: string, origin: number, } | { "event": "table_changed", table: string, origin: number, } | { "event": "tables_changed", origin: number, } | { "event": "sheet_changed", sheet: number | null, origin: number, } | { "event": "views_changed", table: string, origin: number, } | { "event": "workbook_deleted", origin: number, };

export type Op = { "op": "tables" } | { "op": "open_view", spec: ViewSpec, } | { "op": "update_view", view_id: number, spec: ViewSpec, } | { "op": "view_info", view_id: number, } | { "op": "close_view", view_id: number, } | { "op": "fetch", view_id: number, row_start: number, row_count: number, } | { "op": "edit", view_id: number, edits: Array<CellEdit>, } | { "op": "insert_rows", view_id: number, at: number, count: number, } | { "op": "delete_rows", view_id: number, start: number, count: number, } | { "op": "stats", view_id: number, column: string, start: number, end: number, } | { "op": "add_column", table: string, name: string, sql_type: string, } | { "op": "add_formula_column", table: string, name: string, formula: string, } | { "op": "update_formula", table: string, name: string, formula: string, } | { "op": "drop_column", table: string, name: string, } | { "op": "rename_column", table: string, name: string, new_name: string, } | { "op": "query", sql: string, limit?: number, } | { "op": "create_table_from_query", name: string, sql: string, } | { "op": "create_demo_table", rows?: number, } | { "op": "drop_table", table: string, } | { "op": "rename_table", table: string, new_name: string, } | { "op": "sheets" } | { "op": "sheet_create", name?: string, } | { "op": "sheet_rename", sheet: number, name: string, } | { "op": "sheet_delete", sheet: number, } | { "op": "sheet_fetch", sheet: number, row_start: number, row_count: number, column_count: number, } | { "op": "sheet_set", sheet: number, inputs: Array<SheetInput>, } | { "op": "sheet_undo" } | { "op": "sheet_redo" } | { "op": "sheet_insert_rows", sheet: number, at: number, count: number, } | { "op": "sheet_delete_rows", sheet: number, at: number, count: number, } | { "op": "sheet_insert_columns", sheet: number, at: number, count: number, } | { "op": "sheet_delete_columns", sheet: number, at: number, count: number, } | { "op": "create_sheet_from_query", name: string, sql: string, } | { "op": "table_to_sheet", view_id: number, name: string, } | { "op": "saved_views", table?: string, } | { "op": "save_view", saved?: string, name: string, spec: ViewSpec, widths?: { [key in string]: number }, } | { "op": "rename_saved_view", saved: string, name: string, } | { "op": "delete_saved_view", saved: string, } | { "op": "table_from_view", view_id: number, name: string, } | { "op": "sheet_to_table", sheet: number, name: string, };

export type ColumnType = { name: string, sql_type: string, };

export type PreviewColumn = { name: string, 
/**
 * The type the column will be imported as (detected, or the override).
 */
sql_type: string, kind: Kind, 
/**
 * What the reader detected, before overrides.
 */
detected: string, };

export type FilePreview = { 
/**
 * "CSV", "Parquet" or "JSON".
 */
format: string, suggested_name: string, columns: Array<PreviewColumn>, 
/**
 * The first rows, converted as they will be imported.
 */
rows: CellValue[][], 
/**
 * Types a user can choose from.
 */
type_choices: Array<string>, };

export type UploadPreview = { "kind": "table", preview: FilePreview, } | { "kind": "sheets", sheets: Array<SheetInfo>, warnings: Array<string>, };

export type ImportProblem = { message: string, line?: number, column?: string, value?: string, 
/**
 * A type that would accept the failing value (always Text for conversion errors).
 */
suggested_type?: string, };

export type UploadCreated = { upload_id: string, filename: string, size_bytes: number, preview: UploadPreview, };

export type ImportRequest = { 
/**
 * Table name (tables only); defaults to the file name.
 */
table?: string, types: Array<ColumnType>, 
/**
 * Client-chosen id for `GET/DELETE /api/workbooks/{id}/jobs/{job}` while it runs.
 */
job?: string, };

export type JobStatus = { running: boolean, percentage?: number, rows_processed: number, total_rows: number, };

/** Result type of each operation. */
export interface Results {
  tables: TableInfo[];
  open_view: ViewInfo;
  update_view: ViewInfo;
  view_info: ViewInfo;
  close_view: null;
  fetch: RowBlock;
  edit: EditOutcome;
  insert_rows: null;
  delete_rows: null;
  stats: Stats;
  add_column: null;
  add_formula_column: null;
  update_formula: null;
  drop_column: null;
  rename_column: null;
  query: QueryPreview;
  create_table_from_query: TableInfo;
  create_demo_table: TableInfo;
  drop_table: null;
  rename_table: string;
  sheets: SheetInfo[];
  sheet_create: SheetInfo;
  sheet_rename: null;
  sheet_delete: null;
  sheet_fetch: SheetBlock;
  sheet_set: SheetInfo;
  sheet_undo: boolean;
  sheet_redo: boolean;
  sheet_insert_rows: null;
  sheet_delete_rows: null;
  sheet_insert_columns: null;
  sheet_delete_columns: null;
  create_sheet_from_query: SheetCopy;
  table_to_sheet: SheetCopy;
  saved_views: SavedView[];
  save_view: SavedView;
  rename_saved_view: SavedView;
  delete_saved_view: null;
  table_from_view: TableInfo;
  sheet_to_table: TableInfo;
}

export type OpName = Op["op"];

/** Parameters of an operation (its request without `op`). */
export type Params<O extends OpName> = Omit<Extract<Op, { op: O }>, "op">;
