// Generated from the Rust types in crates/alephgrid-core and crates/alephgrid-server.
// Do not edit: run `UPDATE_BINDINGS=1 cargo test -p alephgrid-server --test protocol`.

/** A cell as sent by the server: integers beyond ±(2^53 - 1) and decimals arrive as exact strings. */
export type CellValue = string | number | boolean | null;

/** One rendered sheet cell: [display, align, input?]; input is present when it differs from display. */
export type SheetCell = [string, Align, string?];

/** A WebSocket request frame. `mutation_id` makes a retried change apply once; `timeout_ms` caps the wait. */
export type Request = Op & { id: number; mutation_id?: string; timeout_ms?: number };

/** Machine-readable reason of a failed request. */
export type ErrorCode = "overloaded" | "deadline_exceeded" | "cancelled" | "too_large" | "capacity" | "unavailable"
  | "not_allowed" | "sql_change" | "bad_parameter" | "schema_changed"
  | "unauthenticated" | "forbidden" | "not_found" | "bad_request" | "quota" | "conflict";

/** A WebSocket reply frame. Changes carry the revision they committed as (`replayed` for a retry already applied).
 *  A `conflict` with `moved` addressed rows, columns or cells that moved or are gone: refresh, nothing to overwrite. */
export type Reply =
  | { id: number; ok: true; result: unknown; revision?: number; replayed?: boolean }
  | { id: number | null; ok: false; error: string; code?: ErrorCode; moved?: boolean };

/** Pushed after connecting. */
export type Hello = { event: "hello"; session: number; workbook: WorkbookMeta; revision: number; role: SpaceRole };

export type Rule = { "kind": "one_of", values: Array<string>, } | { "kind": "between", min?: number, max?: number, } | { "kind": "required" };

export type Validation = { column: string, rule: Rule, };

export type ValidationReport = { rules: Array<Validation>, 
/**
 * Rows breaking each rule now, in rule order.
 */
violations: Array<number>, };

export type Transform = { "kind": "dedupe", keys: Array<string>, } | { "kind": "split", column: string, delimiter: string, parts: number, } | { "kind": "trim", column: string, inner: boolean, } | { "kind": "convert", column: string, to: string, };

export type TransformPreview = { 
/**
 * The first rows of the result.
 */
sample: QueryPreview, 
/**
 * Rows the result would have.
 */
rows: number, 
/**
 * Rows left out (dedupe) or values that did not convert (convert).
 */
affected: number, };

export type FormulaInspection = { table: string, column: string, formula: string, 
/**
 * Columns of the table itself, whole columns of other tables, and sheet cells it reads.
 */
reads_columns: Array<string>, reads_tables: Array<string>, reads_cells: Array<string>, 
/**
 * Formula columns (`table.column`) and sheet cells that read this column.
 */
read_by: Array<string>, 
/**
 * How it is evaluated: `lookup join`, `aggregate join`, `nearest-match join`, `row
 * expression`.
 */
plan: Array<string>, 
/**
 * Rows of the table whose value is an error, by code.
 */
errors: Array<[string, number]>, };

export type Aggregate = "count" | "sum" | "average" | "min" | "max";

export type Pivot = { column: string, values: Array<string | null>, };

export type Measure = { aggregate: Aggregate, value?: string, name: string, };

export type Calculated = { name: string, formula: string, };

export type ChartKind = "bar" | "line" | "scatter" | "column" | "area" | "pie";

export type Slicer = { table: string, column: string, values: Array<string | null>, };

export type Tile = { analysis: string, x: number, y: number, w: number, h: number, };

export type Dashboard = { id: string, name: string, tiles: Array<Tile>, slicers: Array<Slicer>, };

export type TileResult = { tile: Tile, analysis?: SavedAnalysis, result?: AnalysisResult, 
/**
 * Why the tile shows nothing (a deleted analysis, a renamed column, …).
 */
error?: string, };

export type DashboardResult = { dashboard: Dashboard, tiles: Array<TileResult>, };

export type JoinKind = "inner" | "left";

export type AnalysisSpec = { "kind": "summary", table: string, groups: Array<string>, aggregate: Aggregate, value: string | null, pivot: Pivot | null, } | { "kind": "join", left: string, right: string, left_key: string, right_key: string, join: JoinKind, } | { "kind": "profile", table: string, columns: Array<string>, } | { "kind": "report", table: string, groups: Array<string>, measures: Array<Measure>, calculated: Array<Calculated>, totals: boolean, filters: Array<Filter>, pivot?: Pivot, };

export type AnalysisResult = { result: QueryResult, diagnostics: QueryResult | null, };

export type SavedAnalysis = { id: string, name: string, spec: AnalysisSpec, chart: boolean, 
/**
 * How a chart draws it (F.11): bars (default), lines or points.
 */
chart_kind: ChartKind, };

export type ReplaceSpec = { table: string, column: string, find: string, replacement: string, };

export type ReplacePreview = { matches: number, sample: QueryResult, };

export type ReplaceOutcome = { table: string, rows: number, undo_revision: number, };

export type DisplayKind = "general" | "number" | "percent" | "date" | "datetime";

export type DisplayFormat = { kind: DisplayKind, decimals: number, locale: string, timezone: string, };

export type Condition = "greater" | "less" | "equal" | "empty";

export type ColorRule = { column: string, condition: Condition, value: number, color: string, };

export type Presentation = { hidden: Array<string>, frozen: number, formats: { [key in string]: DisplayFormat }, rules: Array<ColorRule>, };

export type ResultType = "number" | "text" | "boolean";

export type RuntimeIdentity = { 
/**
 * `python`.
 */
kind: string, 
/**
 * Content hash of its manifest (interpreter version, packages and their hashes).
 */
id: string, 
/**
 * The interpreter's version.
 */
version: string, 
/**
 * Packages and versions, `name==version`.
 */
packages: Array<string>, };

export type Limits = { timeout_ms: number, cpu_ms: number, memory_bytes: number, output_bytes: number, 
/**
 * Workers running at once on the server.
 */
workers: number, };

export type FunctionDefinition = { name: string, kind: FunctionKind, arguments: Array<string>, source: string, documentation: string, tests: Array<FunctionTest>, dependencies: { [key in string]: number }, 
/**
 * Python: what the function returns (required).
 */
returns?: ResultType, 
/**
 * Python: the result may change with the same arguments (clock, randomness); computed
 * again only on `refresh_functions`.
 */
volatile?: boolean, };

export type FunctionKind = "formula" | "sql" | "python";

export type FunctionTest = { arguments: Array<TestValue>, expected: TestValue, };

export type FunctionTestResult = { actual: string, expected: string, passed: boolean, 
/**
 * What kind of value `actual` is.
 */
kind: TestValueKind, 
/**
 * Why an error result is an error, when known (Python: the exception and its line).
 */
message?: string, };

export type FunctionVersion = { version: number, definition: FunctionDefinition, adapter: string, duckdb_version: string, dependencies: { [key in string]: number }, 
/**
 * Python: the runtime it was published with; calls need exactly this runtime.
 */
runtime?: RuntimeIdentity, };

export type WorkbookFunction = { name: string, pinned: number, versions: Array<FunctionVersion>, };

export type TestValue = string | TypedTestValue;

export type TypedTestValue = { "type": "blank" } | { "type": "text", value: string, } | { "type": "error", value: string, };

export type TestValueKind = "blank" | "number" | "text" | "boolean" | "error" | "other";

export type FunctionUsage = { name: string, 
/**
 * Sheet cells whose formula calls it, the first [`USAGE_CELLS`].
 */
cells: Array<FunctionUseCell>, 
/**
 * All such cells.
 */
cell_count: number, 
/**
 * Calculated table columns whose formula calls it.
 */
columns: Array<FunctionUseColumn>, 
/**
 * Active versions of other workbook functions that call it, and the version they call.
 */
dependents: Array<FunctionDependent>, 
/**
 * The Python runtime this server has: a Python version published with another one
 * cannot compute here.
 */
runtime?: RuntimeIdentity, 
/**
 * Why Python functions cannot run in this workbook now (turned off for its space).
 */
python_unavailable?: string, };

export type FunctionUseCell = { sheet: string, 
/**
 * The cell's address, `B2`.
 */
cell: string, formula: string, };

export type FunctionUseColumn = { table: string, column: string, };

export type FunctionDependent = { name: string, 
/**
 * Its active version.
 */
version: number, 
/**
 * The version of the function in question that it was published against.
 */
uses: number, };

export type Kind = "int" | "number" | "bool" | "date" | "datetime" | "time" | "text";

export type FormulaLanguage = "sql" | "sheet";

export type Column = { name: string, sql_type: string, kind: Kind, 
/**
 * Present for computed (formula) columns.
 */
formula?: string, 
/**
 * `sheet` for a formula written in spreadsheet syntax; absent for DuckDB SQL formulas.
 */
formula_language?: FormulaLanguage, };

export type CellErrorMark = { row: number, column: number, code: string, };

export type TableInfo = { name: string, row_count: number, column_count: number, };

export type SortKey = { column: string, desc: boolean, };

export type FilterOp = "eq" | "neq" | "gt" | "gte" | "lt" | "lte" | "contains" | "not_contains" | "starts_with" | "is_empty" | "not_empty" | "in";

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
can_append: boolean, 
/**
 * Why the view's formula values are not current, when it is ordered or filtered by
 * formula columns whose stored results are being recalculated or failed to be (F.09).
 */
stale?: string, 
/**
 * The table's structure generation the view's positions belong to. Positions are
 * stable while it stays the same; send it with changes that address rows by position
 * (`insert_rows`, `delete_rows`, appending edits) so they are refused, not misapplied,
 * after someone else inserted, deleted or reordered rows.
 */
generation: number, };

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
truncated: boolean, 
/**
 * Cells of spreadsheet-formula columns that hold an error (their value is `null`).
 */
errors?: Array<CellErrorMark>, 
/**
 * As [`ViewInfo::stale`]: the formula values are the last committed results.
 */
stale?: string, 
/**
 * As [`ViewInfo::generation`]: the generation these positions were read at.
 */
generation: number, };

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
record?: number, 
/**
 * The value the cell had when the user started editing it, as fetched (`null` for
 * empty). When given and the cell holds something else now, someone else changed it:
 * the whole edit is refused with [`Conflict`] and nothing is written. Leave it out to
 * overwrite whatever is there.
 */
base?: CellValue, };

export type EditOutcome = { table: string, cells_changed: number, rows_appended: number, 
/**
 * Record id of the first appended row; the others follow it in order, at the view
 * positions after the old last row. Later edits of those rows can then go by record.
 */
first_appended?: number, };

export type Stats = { rows: number, count: number, 
/**
 * Exact: a number, or a string for integers beyond 2^53 and for decimals.
 */
sum?: number | string, avg?: number, min?: CellValue, max?: CellValue, };

export type ColumnValues = { values: Array<ValueCount>, 
/**
 * Distinct values matching the search, listed or not.
 */
distinct: number, 
/**
 * Rows the other filters keep.
 */
rows: number, truncated: boolean, };

export type ValueCount = { 
/**
 * The value as text; `None` for empty cells.
 */
value: string | null, count: number, };

export type FindHit = { row: number, columns: Array<number>, };

export type FindResult = { matches?: number, hits: Array<FindHit>, };

export type CellStyle = { b?: boolean, i?: boolean, u?: boolean, 
/**
 * Strikethrough.
 */
s?: boolean, color?: string, fill?: string, 
/**
 * Font size in points, when not the default 12.
 */
size?: number, 
/**
 * `left`, `center`, `right` (and other spreadsheet alignments).
 */
h?: string, 
/**
 * `top`, `center`, `bottom`.
 */
v?: string, wrap?: boolean, 
/**
 * The number format code, when not `general`.
 */
num_fmt?: string, 
/**
 * Borders drawn on the cell: any of `t`, `r`, `b`, `l`.
 */
borders?: string, };

export type StyledCell = { row: number, column: number, style: CellStyle, };

export type StylePatch = { b?: boolean, i?: boolean, u?: boolean, s?: boolean, color?: string, fill?: string, size?: number, h?: string, v?: string, wrap?: boolean, num_fmt?: string, };

export type SheetSearch = { text: string, match_case?: boolean, 
/**
 * The whole cell must equal the text.
 */
whole?: boolean, 
/**
 * Look in formulas (the cell's input) instead of shown values.
 */
formulas?: boolean, };

export type CellComment = { id: string, row: number, column: number, author: string, created_ms: number, edited_ms?: number, text: string, resolved: boolean, 
/**
 * The thread's first comment, for a reply.
 */
parent?: string, };

export type SortBy = { column: number, desc?: boolean, };

export type SheetColorRule = { area: CellArea, condition: string, value: string, color: string, };

export type SheetRules = { colors: Array<SheetColorRule>, validations: Array<SheetValidation>, };

export type SheetValidation = { area: CellArea, list?: Array<string>, min?: number, max?: number, };

export type SheetChart = { 
/**
 * Chosen by the client; unique within the sheet.
 */
chart: string, kind: string, title: string, 
/**
 * The data: first column categories, first row series names.
 */
data: CellArea, 
/**
 * Top-left cell the chart is placed at, and its size in grid pixels.
 */
row: number, column: number, width: number, height: number, };

export type SheetMatches = { sheet: number, name: string, matches: number, };

export type Axis = "rows" | "columns";

export type LineSize = { 
/**
 * 0-based row or column.
 */
index: number, size: number, hidden: boolean, };

export type MergeMode = "merge" | "center" | "across" | "unmerge";

export type SheetLayout = { frozen_rows: number, frozen_columns: number, columns: Array<LineSize>, rows: Array<LineSize>, 
/**
 * True when more rows differ than a layout lists ([`MAX_LISTED_ROWS`]).
 */
truncated: boolean, 
/**
 * Merged ranges; the top-left cell holds the value and the others are covered.
 */
merged: Array<CellArea>, };

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

export type ExportFormat = "csv" | "csv_excel" | "parquet" | "json";

export type Align = "left" | "right" | "center";

export type SheetInfo = { index: number, name: string, 
/**
 * Used range, 0-based exclusive bounds.
 */
rows: number, columns: number, 
/**
 * The revision that last inserted, deleted or moved this sheet's rows, columns or
 * cells, or shifted its index. Send it with changes that address cells by position
 * (`layout`): they are refused, not misapplied, once it moved on.
 */
layout: number, 
/**
 * Columns kept in view while scrolling sideways (S.05).
 */
frozen_columns: number, 
/**
 * Rows kept in view while scrolling down (S.05).
 */
frozen_rows: number, 
/**
 * The tab's colour, `#RRGGBB` (S.07).
 */
color?: string, };

export type SheetBlock = { row_start: number, 
/**
 * `rows[i][j]` is `null` for empty cells.
 */
rows: (SheetCell | null)[][], 
/**
 * As [`SheetInfo::layout`]: the layout these addresses were read at.
 */
layout: number, 
/**
 * Cells of this block whose style is not the default (S.01), empty cells included.
 */
styles?: Array<StyledCell>, };

export type SheetInput = { row: number, column: number, 
/**
 * Text exactly as typed: `42`, `hello`, `=SUM(A1:A9)`. Empty clears the cell.
 */
value: string, 
/**
 * What the cell showed in its editor when the user started editing (its input, or
 * its display when it has no input; empty for an empty cell). When given and the
 * cell holds something else now, someone else changed it: the whole request is
 * refused with [`Conflict`](crate::Conflict) and nothing is written.
 */
base?: string, };

export type SheetCopy = { sheet: SheetInfo, rows_copied: number, 
/**
 * Rows in the source, when known without running the query to the end.
 */
total_rows?: number, 
/**
 * True when rows were left out because of `row_limit`.
 */
truncated: boolean, row_limit: number, };

export type NamedRange = { name: string, scope: number | null, 
/**
 * What the name refers to, e.g. `=Sheet1!$A$1:$B$4`.
 */
refers_to: string, };

export type CellError = { 
/**
 * The error as displayed, e.g. `#DIV/0!`.
 */
code: string, 
/**
 * The cell that produced it, e.g. `Sheet1!B3`; empty for a typed error value.
 */
origin: string, message: string, };

export type CellDetail = { input: string, display: string, error: CellError | null, };

export type FormulaProblem = { message: string, position: number, };

export type CellArea = { row: number, column: number, rows: number, columns: number, };

export type FillDirection = "down" | "right";

export type PasteMode = "all" | "formulas" | "values" | "formats";

export type Imported = { "kind": "table", table: TableInfo, } | { "kind": "sheets", sheets: Array<SheetInfo>, warnings: Array<string>, };

export type SpaceRole = "viewer" | "editor" | "owner";

export type OrgRole = "member" | "admin";

export type Cap = "view" | "query" | "export" | "edit" | "publish" | "create_workbook" | "delete_workbook" | "purge" | "manage_members" | "manage_space" | "choose_compute";

export type AuthMode = "local" | "accounts";

export type Org = { id: string, name: string, created_at: number, };

export type Space = { id: string, org_id: string, name: string, created_at: number, 
/**
 * The least role that may publish: function versions and pins in the space's
 * workbooks, and entries of its library (7.11). `editor` or `owner`.
 */
publish_role: SpaceRole, 
/**
 * Whether Python functions may run for its workbooks (phase 8); organization admins
 * turn it off.
 */
python: boolean, };

export type PublicUser = { id: string, email: string, name: string, instance_admin: boolean, disabled: boolean, 
/**
 * `password`, `sso` or both.
 */
sign_in: Array<string>, groups: Array<string>, last_login?: number, };

export type OrgMember = { user: PublicUser, role: OrgRole, };

export type MemberKind = "user" | "group";

export type SpaceMember = { kind: MemberKind, 
/**
 * User id or group name.
 */
principal: string, role: SpaceRole, 
/**
 * For a user: their email and name.
 */
user?: PublicUser, };

export type Token = { id: string, name: string, space_id: string, role: SpaceRole, created_by: string, created_at: number, expires_at: number, revoked_at?: number, last_used?: number, };

export type AgentConnection = { id: string, name: string, space_id: string, 
/**
 * Workbooks it may read; absent: every workbook of the space.
 */
workbooks?: Array<string>, created_by: string, created_at: number, expires_at: number, revoked_at?: number, last_used?: number, };

export type AgentActivity = { at_ms: number, tool: string, workbook?: string, ok: boolean, 
/**
 * What it did: SQL, rows, result handle, or the error.
 */
detail: Record<string, unknown>, };

export type AgentProposal = { id: string, connection_id: string, workbook: string, 
/**
 * What it would do: `saved_query`.
 */
kind: string, name: string, 
/**
 * The SQL and parameters of a saved query.
 */
payload: Record<string, unknown>, 
/**
 * Workbook revision the agent read when it asked.
 */
revision: number, created_at: number, 
/**
 * pending, approved, rejected or failed.
 */
status: string, decided_by?: string, decided_at?: number, result?: Record<string, unknown>, };

export type Invite = { id: string, org_id: string, email: string, org_role: OrgRole, space_id?: string, space_role?: SpaceRole, created_by: string, created_at: number, expires_at: number, accepted_at?: number, revoked_at?: number, };

export type AuditEvent = { seq: number, at: number, 
/**
 * `user:<id>`, `token:<id>`, `local` or `system`.
 */
actor: string, action: string, org_id?: string, space_id?: string, workbook?: string, 
/**
 * `ok` or `denied`.
 */
outcome: string, detail: Record<string, unknown>, };

export type QuotaLimits = { storage_bytes?: number, workbooks?: number, jobs?: number, open_workbooks?: number, compute_memory_bytes?: number, };

export type QuotaKind = "storage" | "workbooks" | "jobs" | "open_workbooks";

export type SpaceUsage = { space: Space, 
/**
 * Bytes of its workbooks, the trash included.
 */
storage_bytes: number, trash_bytes: number, workbooks: number, open_workbooks: number, 
/**
 * Memory the space's open workbooks are guaranteed (their profiles).
 */
compute_memory_bytes: number, jobs: number, 
/**
 * The limits that apply.
 */
limits: QuotaLimits, 
/**
 * The space's own overrides of the server defaults.
 */
overrides: QuotaLimits, 
/**
 * Compute profiles granted to it besides `standard`.
 */
profiles: Array<string>, };

export type LibraryKind = "query" | "view" | "function";

export type LibraryVersion = { version: number, 
/**
 * The saved query (`name`, `kind`, `sql`, `params`), view (`name`, `spec`, `widths`) or
 * function definition as published.
 */
payload: Record<string, unknown>, source_workbook: string, published_by: string, published_at: number, };

export type LibraryEntry = { id: string, space_id: string, kind: LibraryKind, name: string, created_by: string, created_at: number, retired_at?: number, latest: LibraryVersion, };

export type HistoryEntry = { revision: number, mutation_id?: string, op: string, actor?: string, at_ms: number, 
/**
 * Short description of the change, at most [`SUMMARY_BYTES`] bytes.
 */
summary: string, 
/**
 * Present for table changes that can be taken back (see `undo`).
 */
undo?: UndoState, };

export type UndoState = { table: string, kind: UndoKind, 
/**
 * The revision this entry took back (undo and redo entries).
 */
undoes?: number, 
/**
 * The revision that took this entry back, if one did; it cannot be undone again (undo
 * that one instead).
 */
undone_by?: number, };

export type UndoKind = "change" | "undo" | "redo";

export type UndoOutcome = { table: string, 
/**
 * The revision taken back.
 */
undone: number, 
/**
 * `undo` or `redo`.
 */
kind: UndoKind, 
/**
 * Rows deleted, restored or changed.
 */
rows: number, };

export type Summary = { tables: number, sheets: number, rows: number, };

export type WorkbookMeta = { id: string, name: string, created_at: number, 
/**
 * The space that owns the workbook; its roles decide who may use it.
 */
space: string, starred: boolean, opened_at: number, updated_at: number, summary: Summary, 
/**
 * Set while the workbook is in the trash.
 */
deleted_at?: number, 
/**
 * Id of the [`crate::runtime::RuntimeSpec`] the workbook was last opened with.
 */
runtime_spec?: string, 
/**
 * The compute profile it runs with (7.15); `None` is `standard`.
 */
profile?: string, };

export type WorkbookListing = { size_bytes: number, 
/**
 * The asking principal's role in the workbook's space (workbook lists).
 */
role?: SpaceRole, 
/**
 * Its compute right now (sleeping when closed; normal, not a failure).
 */
compute: ComputeState, id: string, name: string, created_at: number, 
/**
 * The space that owns the workbook; its roles decide who may use it.
 */
space: string, starred: boolean, opened_at: number, updated_at: number, summary: Summary, 
/**
 * Set while the workbook is in the trash.
 */
deleted_at?: number, 
/**
 * Id of the [`crate::runtime::RuntimeSpec`] the workbook was last opened with.
 */
runtime_spec?: string, 
/**
 * The compute profile it runs with (7.15); `None` is `standard`.
 */
profile?: string, };

export type WorkbookPatch = { name?: string, 
/**
 * Move the workbook into this space.
 */
space?: string, starred?: boolean, };

export type Event = { "event": "cells_changed", table: string, origin: number, revision: number, } | { "event": "table_changed", table: string, origin: number, revision: number, } | { "event": "tables_changed", origin: number, revision: number, } | { "event": "sheet_changed", sheet: number | null, origin: number, revision: number, } | { "event": "views_changed", table: string, origin: number, revision: number, } | { "event": "queries_changed", origin: number, revision: number, } | { "event": "workbook_deleted", origin: number, } | { "event": "workbook_closed", reason: string, } | { "event": "resync", revision?: number, } | { "event": "committed", origin: number, revision: number, } | { "event": "access_revoked", reason: string, } | { "event": "role_changed", role: SpaceRole, } | { "event": "progress", request: number, percentage: number, };

export type Op = { "op": "analyze", spec: AnalysisSpec, } | { "op": "analyses" } | { "op": "save_analysis", saved: string | null, name: string, spec: AnalysisSpec, chart: boolean, chart_kind?: ChartKind, } | { "op": "delete_analysis", analysis: string, } | { "op": "dashboards" } | { "op": "dashboard", dashboard: string, } | { "op": "save_dashboard", saved: string | null, name: string, tiles: Array<Tile>, slicers: Array<Slicer>, } | { "op": "delete_dashboard", dashboard: string, } | { "op": "validations", table: string, } | { "op": "set_validations", table: string, rules: Array<Validation>, } | { "op": "preview_transform", table: string, transform: Transform, } | { "op": "apply_transform", table: string, transform: Transform, name: string, } | { "op": "inspect_formula", table: string, column: string, } | { "op": "xlsx_compatibility" } | { "op": "drill", spec: AnalysisSpec, row: CellValue[], } | { "op": "materialize_analysis", name: string, spec: AnalysisSpec, } | { "op": "replace_preview", spec: ReplaceSpec, } | { "op": "replace_apply", spec: ReplaceSpec, revision: number, } | { "op": "replace_undo", revision: number, } | { "op": "presentation", table: string, } | { "op": "save_presentation", table: string, presentation: Presentation, } | { "op": "functions" } | { "op": "function_usage", name: string, } | { "op": "test_function", definition: FunctionDefinition, } | { "op": "publish_function", definition: FunctionDefinition, } | { "op": "pin_functions", pins: { [key in string]: number }, } | { "op": "refresh_functions" } | { "op": "tables" } | { "op": "open_view", spec: ViewSpec, } | { "op": "update_view", view_id: number, spec: ViewSpec, } | { "op": "view_info", view_id: number, } | { "op": "close_view", view_id: number, } | { "op": "fetch", view_id: number, row_start: number, row_count: number, column_start?: number, column_count?: number, } | { "op": "edit", view_id: number, edits: Array<CellEdit>, generation?: number, } | { "op": "insert_rows", view_id: number, at: number, count: number, generation?: number, } | { "op": "delete_rows", view_id: number, start: number, count: number, generation?: number, } | { "op": "undo", revision?: number, redo?: boolean, } | { "op": "stats", view_id: number, column: string, start: number, end: number, } | { "op": "find_in_view", view_id: number, text: string, match_case?: boolean, from?: number, backwards?: boolean, count?: boolean, } | { "op": "column_values", spec: ViewSpec, column: string, search?: string, limit?: number, } | { "op": "add_column", table: string, name: string, sql_type: string, } | { "op": "add_formula_column", table: string, name: string, formula: string, language?: FormulaLanguage, } | { "op": "update_formula", table: string, name: string, formula: string, } | { "op": "drop_column", table: string, name: string, } | { "op": "rename_column", table: string, name: string, new_name: string, } | { "op": "query", sql: string, params?: Array<QueryParam>, limit?: number, } | { "op": "create_table_from_query", name: string, sql: string, params?: Array<QueryParam>, saved?: string, version?: number, } | { "op": "create_demo_table", rows?: number, } | { "op": "drop_table", table: string, } | { "op": "rename_table", table: string, new_name: string, } | { "op": "sheets" } | { "op": "sheet_create", name?: string, } | { "op": "sheet_rename", sheet: number, name: string, layout?: number, } | { "op": "sheet_delete", sheet: number, layout?: number, } | { "op": "sheet_fetch", sheet: number, row_start: number, row_count: number, column_count: number, } | { "op": "sheet_set", sheet: number, inputs: Array<SheetInput>, layout?: number, } | { "op": "sheet_undo" } | { "op": "sheet_redo" } | { "op": "sheet_insert_rows", sheet: number, at: number, count: number, layout?: number, } | { "op": "sheet_delete_rows", sheet: number, at: number, count: number, layout?: number, } | { "op": "sheet_insert_columns", sheet: number, at: number, count: number, layout?: number, } | { "op": "sheet_delete_columns", sheet: number, at: number, count: number, layout?: number, } | { "op": "sheet_names" } | { "op": "sheet_define_name", name: string, scope?: number, refers_to: string, } | { "op": "sheet_update_name", name: string, scope?: number, new_name: string, new_scope?: number, refers_to: string, } | { "op": "sheet_delete_name", name: string, scope?: number, } | { "op": "sheet_cell_detail", sheet: number, row: number, column: number, } | { "op": "sheet_region", sheet: number, row: number, column: number, } | { "op": "sheet_check_formula", sheet: number, row: number, column: number, formula: string, } | { "op": "sheet_fill", sheet: number, area: CellArea, direction: FillDirection, layout?: number, } | { "op": "sheet_drag_fill", sheet: number, source: CellArea, target: CellArea, layout?: number, } | { "op": "sheet_style", sheet: number, area: CellArea, style: StylePatch, layout?: number, } | { "op": "sheet_find", sheet: number, search: SheetSearch, from?: number, backwards?: boolean, count?: boolean, } | { "op": "sheet_replace", sheet: number, search: SheetSearch, replacement: string, } | { "op": "sheet_find_all", search: SheetSearch, } | { "op": "sheet_replace_all", search: SheetSearch, replacement: string, } | { "op": "comments", sheet: number, } | { "op": "add_comment", sheet: number, row: number, column: number, text: string, parent?: string, } | { "op": "edit_comment", sheet: number, comment: string, text: string, } | { "op": "resolve_comment", sheet: number, comment: string, resolved: boolean, } | { "op": "delete_comment", sheet: number, comment: string, } | { "op": "sheet_border", sheet: number, area: CellArea, kind: string, weight: string, layout?: number, } | { "op": "sheet_sort", sheet: number, area: CellArea, by: Array<SortBy>, header?: boolean, layout?: number, } | { "op": "sheet_charts", sheet: number, } | { "op": "set_sheet_charts", sheet: number, charts: Array<SheetChart>, } | { "op": "sheet_rules", sheet: number, } | { "op": "set_sheet_rules", sheet: number, rules: SheetRules, } | { "op": "sheet_reorder", sheet: number, to: number, } | { "op": "sheet_hide", sheet: number, } | { "op": "hidden_sheets" } | { "op": "sheet_unhide", name: string, } | { "op": "sheet_duplicate", sheet: number, } | { "op": "sheet_color", sheet: number, color: string, } | { "op": "sheet_freeze", sheet: number, columns: number, rows?: number, } | { "op": "sheet_layout", sheet: number, } | { "op": "sheet_resize", sheet: number, axis: Axis, start: number, count: number, size: number, } | { "op": "sheet_set_hidden", sheet: number, axis: Axis, start: number, count: number, hidden: boolean, } | { "op": "sheet_merge", sheet: number, area: CellArea, mode: MergeMode, } | { "op": "sheet_column_texts", sheet: number, column: number, } | { "op": "sheet_range_values", sheet: number, area: CellArea, header: boolean, column: number, } | { "op": "sheet_numbers", sheet: number, area: CellArea, } | { "op": "sheet_filter", sheet: number, area: CellArea, header: boolean, column: number, keep?: Array<string>, } | { "op": "sheet_clear_format", sheet: number, area: CellArea, layout?: number, } | { "op": "sheet_paste", from_sheet: number, source: CellArea, sheet: number, row: number, column: number, mode?: PasteMode, transpose?: boolean, 
/**
 * Layouts (see `SheetInfo::layout`) of the target sheet and of `from_sheet`.
 */
layout?: number, source_layout?: number, } | { "op": "sheet_move", sheet: number, source: CellArea, row: number, column: number, layout?: number, } | { "op": "create_sheet_from_query", name: string, sql: string, params?: Array<QueryParam>, allow_truncation?: boolean, } | { "op": "table_to_sheet", view_id: number, name: string, } | { "op": "saved_views", table?: string, } | { "op": "save_view", saved?: string, name: string, spec: ViewSpec, widths?: { [key in string]: number }, } | { "op": "rename_saved_view", saved: string, name: string, } | { "op": "delete_saved_view", saved: string, } | { "op": "table_from_view", view_id: number, name: string, } | { "op": "sheet_to_table", sheet: number, name: string, layout?: number, } | { "op": "history", after?: number, limit?: number, } | { "op": "schema" } | { "op": "export_query", sql: string, params?: Array<QueryParam>, format?: ExportFormat, } | { "op": "sql_change", sql: string, params?: Array<QueryParam>, } | { "op": "saved_queries" } | { "op": "query_versions", saved: string, } | { "op": "save_query", saved?: string, name: string, kind?: QueryKind, sql: string, params?: Array<QueryParam>, } | { "op": "rename_query", saved: string, name: string, } | { "op": "delete_query", saved: string, } | { "op": "materialized" } | { "op": "refresh_materialized", table: string, } | { "op": "cancel", request: number, } | { "op": "recalculate", table: string, } | { "op": "cancel_recalculation", table: string, };

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
 * Workbook storage format the runtime reads and writes (`alephgrid_core::storage`).
 */
storage_format: number, 
/**
 * Versions of the components that compute results.
 */
components: { [key in string]: string }, capabilities: Array<Capability>, };

export type IsolationClass = "shared_process" | "process" | "container" | "virtual_machine";

export type ProviderKind = "local";

export type ComputeProfile = { id: string, 
/**
 * Fixed memory reservation; `None` (standard) is an equal share of what fixed profiles
 * leave, at least the policy's minimum.
 */
memory_bytes?: number, 
/**
 * Fixed worker threads (with `memory_bytes`).
 */
threads?: number, 
/**
 * Never closed to make room for another workbook; choosing it needs an organization admin.
 */
dedicated: boolean, };

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
idle_timeout_ms: number, providers: Array<ProviderKind>, 
/**
 * Profiles allocations may use: `standard` first, then the configured ones.
 */
profiles: Array<ComputeProfile>, 
/**
 * How long a start waits for host capacity before failing with `capacity`.
 */
queue_timeout_ms: number, 
/**
 * Limits of Python workers, when this server runs Python functions (phase 8).
 */
python?: Limits, };

export type ProfileChoice = { profile: ComputeProfile, 
/**
 * Granted to the workbook's space (`standard` always is).
 */
granted: boolean, 
/**
 * The caller may switch the workbook to it.
 */
allowed: boolean, 
/**
 * Why not, if not.
 */
reason?: string, };

export type SpaceProfiles = { 
/**
 * Profile ids granted to the space besides `standard`, which always is.
 */
profiles: Array<string>, };

export type AllocationState = "requested" | "starting" | "running" | "ready" | "draining" | "stopped" | "failed";

export type ComputeState = "sleeping" | "queued" | "starting" | "ready" | "stopping";

export type FailureKind = "incompatible" | "capacity" | "quota" | "owned_elsewhere" | "startup_failed" | "cancelled" | "lost";

export type RuntimeFailure = { kind: FailureKind, message: string, };

export type Reservation = { memory_bytes: number, threads: number, temp_disk_bytes: number, };

export type Transition = { at_ms: number, from: AllocationState, to: AllocationState, operation: string, };

export type RuntimeAllocation = { id: string, workbook: string, spec: string, policy: string, 
/**
 * The profile it runs with.
 */
profile: string, provider: ProviderKind, state: AllocationState, 
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

export type RuntimeStatus = { workbook: string, spec: RuntimeSpec, spec_id: string, policy: ComputePolicy, allocation?: RuntimeAllocation, ownership?: OwnershipAssignment, sessions: number, 
/**
 * The profile the workbook asks for (`workbook.json`).
 */
profile: string, 
/**
 * Why the live allocation runs with another profile, if it does.
 */
note?: string, 
/**
 * Place in the capacity queue (1 is next) while waiting for the host to have room.
 */
queue_position?: number, 
/**
 * Last meaningful activity of the live allocation (ms since the epoch).
 */
last_activity_ms?: number, 
/**
 * Bytes of the workbook on disk: kept while its compute is stopped.
 */
storage_bytes: number, };

export type RuntimeAction = "start" | "stop" | "cancel" | "restart";

export type RuntimeRequest = { action: RuntimeAction, 
/**
 * Repeating an operation id that already moved this workbook's allocation returns the
 * current status and does nothing.
 */
operation_id?: string, 
/**
 * `start`, `restart`: `false` replies once the start is under way (or over) instead of
 * when it is ready; follow it with `GET`.
 */
wait?: boolean, 
/**
 * `restart`: run with this profile from now on (7.15).
 */
profile?: string, };

export type HostBudget = { memory_bytes: number, threads: number, temp_disk_bytes: number, };

export type AllocationUsage = { workbook: string, reservation: Reservation, 
/**
 * Settings read back from the workbook's DuckDB instance.
 */
duckdb_memory_limit: string, duckdb_threads: string, 
/**
 * Jobs waiting or running.
 */
jobs: number, 
/**
 * Bytes of the workbook's database file and log.
 */
database_bytes: number, };

export type ResourceUsage = { budget: HostBudget, allocations: Array<AllocationUsage>, reserved: Reservation, upload_bytes: number, upload_budget: number, 
/**
 * Largest a workbook's database may grow, if limited (SEC.11).
 */
workbook_size_limit: number | null, 
/**
 * Free bytes on the data directory's disk, when the platform says.
 */
disk_free_bytes: number | null, 
/**
 * The disk reserve (3.T03): its size (0 when off) and whether it is held. Not held while
 * it is on means the disk filled up and the reserve was given back.
 */
disk_reserve_bytes: number, disk_reserve_held: boolean, };

export type BackupEntry = { id: string, name: string, 
/**
 * Revision the copy holds.
 */
revision: number, storage_format: number, bytes: number, sha256: string, };

export type BackupManifest = { format: number, created_at_ms: number, server_version: string, 
/**
 * SHA-256 of `control.duckdb` (format 2).
 */
control_sha256?: string, workbooks: Array<BackupEntry>, };

/** Result type of each operation. */
export interface Results {
  analyze: AnalysisResult;
  analyses: SavedAnalysis[];
  save_analysis: SavedAnalysis;
  delete_analysis: null;
  dashboards: Dashboard[];
  dashboard: DashboardResult;
  save_dashboard: Dashboard;
  delete_dashboard: null;
  validations: Validation[];
  set_validations: ValidationReport;
  preview_transform: TransformPreview;
  apply_transform: TableInfo;
  inspect_formula: FormulaInspection;
  xlsx_compatibility: string[];
  drill: ViewSpec;
  materialize_analysis: TableInfo;
  replace_preview: ReplacePreview;
  replace_apply: ReplaceOutcome;
  replace_undo: ReplaceOutcome;
  presentation: Presentation;
  save_presentation: Presentation;
  functions: WorkbookFunction[];
  function_usage: FunctionUsage;
  test_function: FunctionTestResult[];
  publish_function: WorkbookFunction;
  pin_functions: WorkbookFunction[];
  refresh_functions: number;
  tables: TableInfo[];
  open_view: ViewInfo;
  update_view: ViewInfo;
  view_info: ViewInfo;
  close_view: null;
  fetch: RowBlock;
  edit: EditOutcome;
  insert_rows: null;
  delete_rows: null;
  undo: UndoOutcome | null;
  stats: Stats;
  find_in_view: FindResult;
  column_values: ColumnValues;
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
  sheet_names: NamedRange[];
  sheet_define_name: NamedRange[];
  sheet_update_name: NamedRange[];
  sheet_delete_name: NamedRange[];
  sheet_cell_detail: CellDetail;
  sheet_region: CellArea;
  sheet_check_formula: FormulaProblem | null;
  sheet_fill: SheetInfo;
  sheet_drag_fill: SheetInfo;
  sheet_style: SheetInfo;
  sheet_find: FindResult;
  sheet_replace: number;
  sheet_find_all: SheetMatches[];
  sheet_replace_all: number;
  comments: CellComment[];
  add_comment: CellComment;
  edit_comment: null;
  resolve_comment: null;
  delete_comment: null;
  sheet_border: SheetInfo;
  sheet_sort: SheetInfo;
  sheet_charts: SheetChart[];
  set_sheet_charts: null;
  sheet_rules: SheetRules;
  set_sheet_rules: null;
  sheet_reorder: null;
  sheet_hide: null;
  hidden_sheets: string[];
  sheet_unhide: null;
  sheet_duplicate: SheetInfo;
  sheet_color: SheetInfo;
  sheet_freeze: SheetInfo;
  sheet_layout: SheetLayout;
  sheet_resize: null;
  sheet_set_hidden: null;
  sheet_merge: null;
  sheet_column_texts: string[];
  sheet_range_values: ValueCount[];
  sheet_numbers: (number | null)[][];
  sheet_filter: number;
  sheet_clear_format: SheetInfo;
  sheet_paste: CellArea;
  sheet_move: CellArea;
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
  recalculate: boolean;
  cancel_recalculation: boolean;
}

export type OpName = Op["op"];

/** Parameters of an operation (its request without `op`). */
export type Params<O extends OpName> = Omit<Extract<Op, { op: O }>, "op">;
