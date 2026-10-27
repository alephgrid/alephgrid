// Generated from the Rust types in crates/rowfinity-engine and crates/rowfinity-server.
// Do not edit: run `UPDATE_BINDINGS=1 cargo test -p rowfinity-server --test protocol`.

/** A cell as sent by the server: integers beyond ±(2^53 - 1) and decimals arrive as exact strings. */
export type CellValue = string | number | boolean | null;

/** One rendered sheet cell: [display, align, input?]; input is present when it differs from display. */
export type SheetCell = [string, Align, string?];

/** A WebSocket request frame. `mutation_id` makes a retried change apply once; `timeout_ms` caps the wait. */
export type Request = Op & { id: number; mutation_id?: string; timeout_ms?: number };

/** Machine-readable reason of a failed request. */
export type ErrorCode = "overloaded" | "deadline_exceeded" | "cancelled" | "too_large" | "capacity" | "unavailable"
  | "not_allowed" | "sql_change" | "bad_parameter" | "schema_changed";

/** A WebSocket reply frame. Changes carry the revision they committed as (`replayed` for a retry already applied). */
export type Reply =
  | { id: number; ok: true; result: unknown; revision?: number; replayed?: boolean }
  | { id: number | null; ok: false; error: string; code?: ErrorCode };

/** Pushed after connecting. */
export type Hello = { event: "hello"; session: number; workbook: WorkbookMeta; revision: number };

export type Aggregate = "count" | "sum" | "average" | "min" | "max";

export type Pivot = { column: string, values: Array<string | null>, };

export type JoinKind = "inner" | "left";

export type AnalysisSpec = { "kind": "summary", table: string, groups: Array<string>, aggregate: Aggregate, value: string | null, pivot: Pivot | null, } | { "kind": "join", left: string, right: string, left_key: string, right_key: string, join: JoinKind, } | { "kind": "profile", table: string, columns: Array<string>, };

export type AnalysisResult = { result: QueryResult, diagnostics: QueryResult | null, };

export type SavedAnalysis = { id: string, name: string, spec: AnalysisSpec, chart: boolean, };

export type ReplaceSpec = { table: string, column: string, find: string, replacement: string, };

export type ReplacePreview = { matches: number, sample: QueryResult, };

export type ReplaceOutcome = { table: string, rows: number, undo_revision: number, };

export type DisplayKind = "general" | "number" | "percent" | "date" | "datetime";

export type DisplayFormat = { kind: DisplayKind, decimals: number, locale: string, timezone: string, };

export type Condition = "greater" | "less" | "equal" | "empty";

export type ColorRule = { column: string, condition: Condition, value: number, color: string, };

export type Presentation = { hidden: Array<string>, frozen: number, formats: { [key in string]: DisplayFormat }, rules: Array<ColorRule>, };

export type FunctionDefinition = { name: string, kind: FunctionKind, arguments: Array<string>, source: string, documentation: string, tests: Array<FunctionTest>, dependencies: { [key in string]: number }, };

export type FunctionKind = "formula" | "sql";

export type FunctionTest = { arguments: Array<string>, expected: string, };

export type FunctionTestResult = { actual: string, expected: string, passed: boolean, };

export type FunctionVersion = { version: number, definition: FunctionDefinition, adapter: string, duckdb_version: string, dependencies: { [key in string]: number }, };

export type WorkbookFunction = { name: string, pinned: number, versions: Array<FunctionVersion>, };

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

export type RowBlock = { row_start: number, 
/**
 * Values of the view's columns `column_start..column_start + rows[i].len()`.
 */
rows: CellValue[][], 
/**
 * Stable record id of each row (see [`CellEdit::record`]).
 */
ids: Array<number>, column_start: number, 
/**
 * Fewer rows than asked for, because the reply reached its size limit; fetch the
 * rest from `row_start + rows.len()`.
 */
truncated: boolean, };

export type CellEdit = { 
/**
 * View position. Ignored when `record` is given, except that rows at or after the
 * end append new rows.
 */
row: number, column: string, 
/**
 * `None` or an empty string clears the cell.
 */
value: string | null, 
/**
 * Record id from a fetch. The edit then lands on that record even if rows were
 * inserted, deleted or re-sorted since the fetch; it fails if the record is gone.
 */
record?: number, };

export type EditOutcome = { table: string, cells_changed: number, rows_appended: number, };

export type Stats = { rows: number, count: number, 
/**
 * Exact: a number, or a string for integers beyond 2^53 and for decimals.
 */
sum?: number | string, avg?: number, min?: CellValue, max?: CellValue, };

export type QueryPreview = { columns: Array<Column>, rows: CellValue[][], truncated: boolean, };

export type ParamType = "text" | "integer" | "decimal" | "double" | "boolean" | "date" | "timestamp";

export type QueryParam = { name: string, type: ParamType, value?: string, };

export type SourceRevision = { table: string, data_rev: number, };

export type QueryResult = { columns: Array<Column>, rows: CellValue[][], 
/**
 * More rows exist than were returned (the row limit or the reply size was reached).
 */
truncated: boolean, 
/**
 * Most rows this preview could hold.
 */
row_limit: number, elapsed_ms: number, 
/**
 * Workbook revision of the snapshot the query read.
 */
revision: number, 
/**
 * User tables the query read, with their data revisions in that snapshot.
 */
sources: Array<SourceRevision>, };

export type TableSchema = { name: string, row_count: number, columns: Array<Column>, };

export type QueryKind = "query" | "live";

export type SavedQuery = { id: string, name: string, kind: QueryKind, 
/**
 * Latest version number (versions start at 1 and never change).
 */
version: number, sql: string, params: Array<QueryParam>, created_at: number, updated_at: number, };

export type QueryVersion = { version: number, sql: string, params: Array<QueryParam>, created_at: number, 
/**
 * Workbook revision that saved it.
 */
revision: number, };

export type MaterializedSource = { table: string, data_rev: number, current_rev?: number, };

export type MaterializedTable = { table: string, 
/**
 * The saved query and version it came from, if any.
 */
query?: string, query_version?: number, sql: string, params: Array<QueryParam>, sources: Array<MaterializedSource>, 
/**
 * Workbook revision at which the rows were computed.
 */
revision: number, refreshed_at: number, 
/**
 * A source changed (or disappeared) since the rows were computed.
 */
stale: boolean, 
/**
 * The table itself was changed by hand since; a refresh replaces those changes.
 */
edited: boolean, };

export type SqlChangeOutcome = { table: string, 
/**
 * `insert`, `update` or `delete`.
 */
kind: string, rows: number, };

export type QueryExport = { 
/**
 * Download it from `/api/workbooks/{id}/downloads/{download}` (once, within an hour).
 */
download: string, file_name: string, rows: number, bytes: number, 
/**
 * Workbook revision of the snapshot that was exported.
 */
revision: number, };

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

export type HistoryEntry = { revision: number, mutation_id?: string, op: string, actor?: string, at_ms: number, 
/**
 * Short description of the change, at most [`SUMMARY_BYTES`] bytes.
 */
summary: string, };

export type Summary = { tables: number, sheets: number, rows: number, };

export type WorkbookMeta = { id: string, name: string, created_at: number, workspace: string | null, starred: boolean, opened_at: number, updated_at: number, summary: Summary, 
/**
 * Set while the workbook is in the trash.
 */
deleted_at?: number, 
/**
 * Id of the [`crate::runtime::RuntimeSpec`] the workbook was last opened with.
 */
runtime_spec?: string, };

export type WorkbookListing = { size_bytes: number, id: string, name: string, created_at: number, workspace: string | null, starred: boolean, opened_at: number, updated_at: number, summary: Summary, 
/**
 * Set while the workbook is in the trash.
 */
deleted_at?: number, 
/**
 * Id of the [`crate::runtime::RuntimeSpec`] the workbook was last opened with.
 */
runtime_spec?: string, };

export type WorkbookPatch = { name?: string, 
/**
 * Workspace id; an empty string moves the workbook out of any workspace.
 */
workspace?: string, starred?: boolean, };

export type Workspace = { id: string, name: string, created_at: number, };

export type Event = { "event": "cells_changed", table: string, origin: number, revision: number, } | { "event": "table_changed", table: string, origin: number, revision: number, } | { "event": "tables_changed", origin: number, revision: number, } | { "event": "sheet_changed", sheet: number | null, origin: number, revision: number, } | { "event": "views_changed", table: string, origin: number, revision: number, } | { "event": "queries_changed", origin: number, revision: number, } | { "event": "workbook_deleted", origin: number, } | { "event": "workbook_closed", reason: string, } | { "event": "resync", } | { "event": "progress", request: number, percentage: number, };

export type Op = { "op": "analyze", spec: AnalysisSpec, } | { "op": "analyses" } | { "op": "save_analysis", saved: string | null, name: string, spec: AnalysisSpec, chart: boolean, } | { "op": "delete_analysis", analysis: string, } | { "op": "materialize_analysis", name: string, spec: AnalysisSpec, } | { "op": "replace_preview", spec: ReplaceSpec, } | { "op": "replace_apply", spec: ReplaceSpec, revision: number, } | { "op": "replace_undo", revision: number, } | { "op": "presentation", table: string, } | { "op": "save_presentation", table: string, presentation: Presentation, } | { "op": "functions" } | { "op": "test_function", definition: FunctionDefinition, } | { "op": "publish_function", definition: FunctionDefinition, } | { "op": "pin_functions", pins: { [key in string]: number }, } | { "op": "tables" } | { "op": "open_view", spec: ViewSpec, } | { "op": "update_view", view_id: number, spec: ViewSpec, } | { "op": "view_info", view_id: number, } | { "op": "close_view", view_id: number, } | { "op": "fetch", view_id: number, row_start: number, row_count: number, column_start?: number, column_count?: number, } | { "op": "edit", view_id: number, edits: Array<CellEdit>, } | { "op": "insert_rows", view_id: number, at: number, count: number, } | { "op": "delete_rows", view_id: number, start: number, count: number, } | { "op": "stats", view_id: number, column: string, start: number, end: number, } | { "op": "add_column", table: string, name: string, sql_type: string, } | { "op": "add_formula_column", table: string, name: string, formula: string, } | { "op": "update_formula", table: string, name: string, formula: string, } | { "op": "drop_column", table: string, name: string, } | { "op": "rename_column", table: string, name: string, new_name: string, } | { "op": "query", sql: string, params?: Array<QueryParam>, limit?: number, } | { "op": "create_table_from_query", name: string, sql: string, params?: Array<QueryParam>, saved?: string, version?: number, } | { "op": "create_demo_table", rows?: number, } | { "op": "drop_table", table: string, } | { "op": "rename_table", table: string, new_name: string, } | { "op": "sheets" } | { "op": "sheet_create", name?: string, } | { "op": "sheet_rename", sheet: number, name: string, } | { "op": "sheet_delete", sheet: number, } | { "op": "sheet_fetch", sheet: number, row_start: number, row_count: number, column_count: number, } | { "op": "sheet_set", sheet: number, inputs: Array<SheetInput>, } | { "op": "sheet_undo" } | { "op": "sheet_redo" } | { "op": "sheet_insert_rows", sheet: number, at: number, count: number, } | { "op": "sheet_delete_rows", sheet: number, at: number, count: number, } | { "op": "sheet_insert_columns", sheet: number, at: number, count: number, } | { "op": "sheet_delete_columns", sheet: number, at: number, count: number, } | { "op": "create_sheet_from_query", name: string, sql: string, params?: Array<QueryParam>, allow_truncation?: boolean, } | { "op": "table_to_sheet", view_id: number, name: string, } | { "op": "saved_views", table?: string, } | { "op": "save_view", saved?: string, name: string, spec: ViewSpec, widths?: { [key in string]: number }, } | { "op": "rename_saved_view", saved: string, name: string, } | { "op": "delete_saved_view", saved: string, } | { "op": "table_from_view", view_id: number, name: string, } | { "op": "sheet_to_table", sheet: number, name: string, } | { "op": "history", after?: number, limit?: number, } | { "op": "schema" } | { "op": "export_query", sql: string, params?: Array<QueryParam>, format?: ExportFormat, } | { "op": "sql_change", sql: string, params?: Array<QueryParam>, } | { "op": "saved_queries" } | { "op": "query_versions", saved: string, } | { "op": "save_query", saved?: string, name: string, kind?: QueryKind, sql: string, params?: Array<QueryParam>, } | { "op": "rename_query", saved: string, name: string, } | { "op": "delete_query", saved: string, } | { "op": "materialized" } | { "op": "refresh_materialized", table: string, } | { "op": "cancel", request: number, };

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

export type Capability = "tables" | "sheets" | "sql" | "python";

export type RuntimeSpec = { engine: string, engine_version: string, 
/**
 * Workbook storage format the runtime reads and writes (`rowfinity_engine::storage`).
 */
storage_format: number, 
/**
 * Versions of the components that compute results.
 */
components: { [key in string]: string }, capabilities: Array<Capability>, };

export type IsolationClass = "shared_process" | "process" | "container" | "virtual_machine";

export type ProviderKind = "local";

export type ComputePolicy = { id: string, 
/**
 * Smallest memory share an allocation may run with; below it, opening another
 * workbook evicts an idle one or fails with `capacity`.
 */
min_memory_bytes: number, 
/**
 * Largest share for one allocation; `None` is a fair share of the host budget.
 */
max_memory_bytes?: number, isolation: IsolationClass, 
/**
 * Close an allocation after this long without sessions or jobs.
 */
idle_timeout_ms: number, providers: Array<ProviderKind>, };

export type AllocationState = "requested" | "starting" | "running" | "ready" | "draining" | "stopped" | "failed";

export type FailureKind = "incompatible" | "capacity" | "owned_elsewhere" | "startup_failed" | "cancelled" | "lost";

export type RuntimeFailure = { kind: FailureKind, message: string, };

export type Reservation = { memory_bytes: number, threads: number, temp_disk_bytes: number, };

export type Transition = { at_ms: number, from: AllocationState, to: AllocationState, operation: string, };

export type RuntimeAllocation = { id: string, workbook: string, spec: string, policy: string, provider: ProviderKind, state: AllocationState, 
/**
 * Ownership epoch; increases with every allocation of the workbook.
 */
epoch: number, reservation: Reservation, 
/**
 * Host and process that own it (`host:pid` for the Local provider).
 */
holder: string, requested_at_ms: number, ready_at_ms?: number, stopped_at_ms?: number, failure?: RuntimeFailure, 
/**
 * Operation ids applied to this allocation, with what they did (bounded).
 */
transitions: Array<Transition>, };

export type OwnershipAssignment = { workbook: string, allocation: string, epoch: number, holder: string, acquired_at_ms: number, 
/**
 * How exclusivity is enforced ("flock" for the Local provider).
 */
mechanism: string, };

export type RuntimeStatus = { workbook: string, spec: RuntimeSpec, spec_id: string, policy: ComputePolicy, allocation?: RuntimeAllocation, ownership?: OwnershipAssignment, sessions: number, };

export type RuntimeAction = "start" | "stop" | "cancel";

export type RuntimeRequest = { action: RuntimeAction, 
/**
 * Repeating an operation id that already moved this workbook's allocation returns the
 * current status and does nothing.
 */
operation_id?: string, };

export type HostBudget = { memory_bytes: number, threads: number, temp_disk_bytes: number, };

export type AllocationUsage = { workbook: string, reservation: Reservation, 
/**
 * Settings read back from the workbook's DuckDB instance.
 */
duckdb_memory_limit: string, duckdb_threads: string, 
/**
 * Jobs waiting or running.
 */
jobs: number, };

export type ResourceUsage = { budget: HostBudget, allocations: Array<AllocationUsage>, reserved: Reservation, upload_bytes: number, upload_budget: number, };

export type BackupEntry = { id: string, name: string, 
/**
 * Revision the copy holds.
 */
revision: number, storage_format: number, bytes: number, sha256: string, };

export type BackupManifest = { format: number, created_at_ms: number, server_version: string, workbooks: Array<BackupEntry>, };

/** Result type of each operation. */
export interface Results {
  analyze: AnalysisResult;
  analyses: SavedAnalysis[];
  save_analysis: SavedAnalysis;
  delete_analysis: null;
  materialize_analysis: TableInfo;
  replace_preview: ReplacePreview;
  replace_apply: ReplaceOutcome;
  replace_undo: ReplaceOutcome;
  presentation: Presentation;
  save_presentation: Presentation;
  functions: WorkbookFunction[];
  test_function: FunctionTestResult[];
  publish_function: WorkbookFunction;
  pin_functions: WorkbookFunction[];
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
  query: QueryResult;
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
  history: HistoryEntry[];
  schema: TableSchema[];
  export_query: QueryExport;
  sql_change: SqlChangeOutcome;
  saved_queries: SavedQuery[];
  query_versions: QueryVersion[];
  save_query: SavedQuery;
  rename_query: SavedQuery;
  delete_query: null;
  materialized: MaterializedTable[];
  refresh_materialized: MaterializedTable;
  cancel: boolean;
}

export type OpName = Op["op"];

/** Parameters of an operation (its request without `op`). */
export type Params<O extends OpName> = Omit<Extract<Op, { op: O }>, "op">;
