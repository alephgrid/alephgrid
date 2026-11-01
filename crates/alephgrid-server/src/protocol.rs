//! TypeScript bindings for everything the server sends or accepts. `web/src/protocol.ts`
//! is generated from these Rust types; `tests/protocol.rs` fails when it is out of date
//! (regenerate with `UPDATE_BINDINGS=1 cargo test -p alephgrid-server --test protocol`).

use alephgrid_core::{
    FunctionDefinition, FunctionKind, FunctionTest, FunctionTestResult, FunctionVersion, WorkbookFunction,
    Align, CellEdit, CellErrorMark, Column, ColumnType, FormulaLanguage, EditOutcome, ExportFormat, FilePreview, Filter, FilterOp, ImportProblem, Imported,
    HistoryEntry, Kind, MaterializedSource, MaterializedTable, ParamType, PreviewColumn, QueryKind, QueryParam, QueryPreview, QueryResult,
    QueryVersion, RowBlock, SavedQuery, SavedView, SheetBlock, CellArea, CellDetail, CellError, FillDirection, FormulaProblem, NamedRange, PasteMode, SheetCopy, SheetInfo, SheetInput, SortKey, SourceRevision, SqlChangeOutcome,
    Stats, TableInfo, TableSchema, UploadPreview, ViewInfo, ViewSpec, ColumnValues, ValueCount, FindHit, FindResult, CellStyle, StyledCell, StylePatch, SheetSearch, CellComment, SortBy, SheetColorRule, SheetRules, SheetValidation, SheetChart, SheetMatches, Axis, LineSize, MergeMode, SheetLayout,
};
use alephgrid_core::analysis::*;
use alephgrid_core::tools::{FormulaInspection, Rule, Transform, TransformPreview, Validation, ValidationReport};
use alephgrid_core::reports::{Calculated, ChartKind, Dashboard, DashboardResult, Measure, Slicer, Tile, TileResult};
use ts_rs::{Config, TS};

use crate::backup::{BackupEntry, BackupManifest};
use crate::http::{ImportRequest, JobStatus, ProfileChoice, RuntimeAction, RuntimeRequest, UploadCreated};
use crate::runtime::budget::{AllocationUsage, HostBudget, ResourceUsage};
use crate::runtime::controller::RuntimeStatus;
use crate::runtime::{
    AllocationState, Capability, ComputePolicy, ComputeState, ComputeProfile, FailureKind, IsolationClass, OwnershipAssignment, ProviderKind, Reservation,
    RuntimeAllocation, RuntimeFailure, RuntimeSpec, Transition,
};
use crate::auth::store::{AgentActivity, AgentConnection, AuditEvent, Invite, MemberKind, Org, OrgMember, PublicUser, Space, SpaceMember, Token};
use crate::auth::{AuthMode, Cap, OrgRole, SpaceRole};
use crate::auth::routes::SpaceProfiles;
use crate::library::{LibraryEntry, LibraryKind, LibraryVersion};
use crate::quota::{QuotaKind, QuotaLimits, SpaceUsage};
use crate::state::{Summary, WorkbookListing, WorkbookMeta, WorkbookPatch};
use crate::ws::{Event, Op, QueryExport};

const HEADER: &str = "\
// Generated from the Rust types in crates/alephgrid-core and crates/alephgrid-server.
// Do not edit: run `UPDATE_BINDINGS=1 cargo test -p alephgrid-server --test protocol`.

/** A cell as sent by the server: integers beyond ±(2^53 - 1) and decimals arrive as exact strings. */
export type CellValue = string | number | boolean | null;

/** One rendered sheet cell: [display, align, input?]; input is present when it differs from display. */
export type SheetCell = [string, Align, string?];

/** A WebSocket request frame. `mutation_id` makes a retried change apply once; `timeout_ms` caps the wait. */
export type Request = Op & { id: number; mutation_id?: string; timeout_ms?: number };

/** Machine-readable reason of a failed request. */
export type ErrorCode = \"overloaded\" | \"deadline_exceeded\" | \"cancelled\" | \"too_large\" | \"capacity\" | \"unavailable\"
  | \"not_allowed\" | \"sql_change\" | \"bad_parameter\" | \"schema_changed\"
  | \"unauthenticated\" | \"forbidden\" | \"not_found\" | \"bad_request\" | \"quota\" | \"conflict\";

/** A WebSocket reply frame. Changes carry the revision they committed as (`replayed` for a retry already applied).
 *  A `conflict` with `moved` addressed rows, columns or cells that moved or are gone: refresh, nothing to overwrite. */
export type Reply =
  | { id: number; ok: true; result: unknown; revision?: number; replayed?: boolean }
  | { id: number | null; ok: false; error: string; code?: ErrorCode; moved?: boolean };

/** Pushed after connecting. */
export type Hello = { event: \"hello\"; session: number; workbook: WorkbookMeta; revision: number; role: SpaceRole };
";

macro_rules! decls {
    ($cfg:expr, $out:expr, $($t:ty),* $(,)?) => {
        $(
            $out.push_str("\nexport ");
            $out.push_str(<$t as TS>::decl($cfg).trim());
            $out.push('\n');
        )*
    };
}

/// The result type of every operation, by op name (TypeScript type expressions).
/// `tests/api.rs` checks each against a real server reply.
pub const RESULTS: &[(&str, &str)] = &[
    ("analyze", "AnalysisResult"), ("analyses", "SavedAnalysis[]"), ("save_analysis", "SavedAnalysis"),
    ("delete_analysis", "null"), ("dashboards", "Dashboard[]"), ("dashboard", "DashboardResult"), ("save_dashboard", "Dashboard"),
    ("delete_dashboard", "null"), ("validations", "Validation[]"), ("set_validations", "ValidationReport"),
    ("preview_transform", "TransformPreview"), ("apply_transform", "TableInfo"), ("inspect_formula", "FormulaInspection"), ("xlsx_compatibility", "string[]"), ("drill", "ViewSpec"), ("materialize_analysis", "TableInfo"), ("replace_preview", "ReplacePreview"),
    ("replace_apply", "ReplaceOutcome"), ("replace_undo", "ReplaceOutcome"), ("presentation", "Presentation"), ("save_presentation", "Presentation"),
    ("functions", "WorkbookFunction[]"),
    ("test_function", "FunctionTestResult[]"),
    ("publish_function", "WorkbookFunction"),
    ("pin_functions", "WorkbookFunction[]"),
    ("refresh_functions", "number"),
    ("tables", "TableInfo[]"),
    ("open_view", "ViewInfo"),
    ("update_view", "ViewInfo"),
    ("view_info", "ViewInfo"),
    ("close_view", "null"),
    ("fetch", "RowBlock"),
    ("edit", "EditOutcome"),
    ("insert_rows", "null"),
    ("delete_rows", "null"),
    ("undo", "UndoOutcome | null"),
    ("stats", "Stats"),
    ("find_in_view", "FindResult"),
    ("column_values", "ColumnValues"),
    ("add_column", "null"),
    ("add_formula_column", "null"),
    ("update_formula", "null"),
    ("drop_column", "null"),
    ("rename_column", "null"),
    ("query", "QueryResult"),
    ("create_table_from_query", "TableInfo"),
    ("create_demo_table", "TableInfo"),
    ("drop_table", "null"),
    ("rename_table", "string"),
    ("sheets", "SheetInfo[]"),
    ("sheet_create", "SheetInfo"),
    ("sheet_rename", "null"),
    ("sheet_delete", "null"),
    ("sheet_fetch", "SheetBlock"),
    ("sheet_set", "SheetInfo"),
    ("sheet_undo", "boolean"),
    ("sheet_redo", "boolean"),
    ("sheet_insert_rows", "null"),
    ("sheet_delete_rows", "null"),
    ("sheet_insert_columns", "null"),
    ("sheet_delete_columns", "null"),
    ("sheet_names", "NamedRange[]"),
    ("sheet_define_name", "NamedRange[]"),
    ("sheet_update_name", "NamedRange[]"),
    ("sheet_delete_name", "NamedRange[]"),
    ("sheet_cell_detail", "CellDetail"),
    ("sheet_check_formula", "FormulaProblem | null"),
    ("sheet_fill", "SheetInfo"),
    ("sheet_style", "SheetInfo"),
    ("sheet_find", "FindResult"),
    ("sheet_replace", "number"),
    ("sheet_find_all", "SheetMatches[]"),
    ("sheet_replace_all", "number"),
    ("comments", "CellComment[]"),
    ("add_comment", "CellComment"),
    ("edit_comment", "null"),
    ("resolve_comment", "null"),
    ("delete_comment", "null"),
    ("sheet_border", "SheetInfo"),
    ("sheet_sort", "SheetInfo"),
    ("sheet_charts", "SheetChart[]"),
    ("set_sheet_charts", "null"),
    ("sheet_rules", "SheetRules"),
    ("set_sheet_rules", "null"),
    ("sheet_reorder", "null"),
    ("sheet_hide", "null"),
    ("hidden_sheets", "string[]"),
    ("sheet_unhide", "null"),
    ("sheet_duplicate", "SheetInfo"),
    ("sheet_color", "SheetInfo"),
    ("sheet_freeze", "SheetInfo"),
    ("sheet_layout", "SheetLayout"),
    ("sheet_resize", "null"),
    ("sheet_set_hidden", "null"),
    ("sheet_merge", "null"),
    ("sheet_column_texts", "string[]"),
    ("sheet_range_values", "ValueCount[]"),
    ("sheet_filter", "number"),
    ("sheet_clear_format", "SheetInfo"),
    ("sheet_paste", "CellArea"),
    ("sheet_move", "CellArea"),
    ("create_sheet_from_query", "SheetCopy"),
    ("table_to_sheet", "SheetCopy"),
    ("saved_views", "SavedView[]"),
    ("save_view", "SavedView"),
    ("rename_saved_view", "SavedView"),
    ("delete_saved_view", "null"),
    ("table_from_view", "TableInfo"),
    ("sheet_to_table", "TableInfo"),
    ("history", "HistoryEntry[]"),
    ("schema", "TableSchema[]"),
    ("export_query", "QueryExport"),
    ("sql_change", "SqlChangeOutcome"),
    ("saved_queries", "SavedQuery[]"),
    ("query_versions", "QueryVersion[]"),
    ("save_query", "SavedQuery"),
    ("rename_query", "SavedQuery"),
    ("delete_query", "null"),
    ("materialized", "MaterializedTable[]"),
    ("refresh_materialized", "MaterializedTable"),
    ("cancel", "boolean"),
    ("recalculate", "boolean"),
    ("cancel_recalculation", "boolean"),
];

/// Op names in the order `Op` declares them, read from its generated declaration.
pub fn op_names() -> Vec<String> {
    let decl = <Op as TS>::decl(&Config::new());
    decl.split("\"op\": \"").skip(1).filter_map(|rest| rest.split('"').next().map(String::from)).collect()
}

pub fn bindings() -> String {
    let cfg = Config::new().with_large_int("number");
    let mut out = HEADER.to_string();
    decls!(
        &cfg, out,
        Rule, Validation, ValidationReport, Transform, TransformPreview, FormulaInspection, Aggregate, Pivot, Measure, Calculated, ChartKind, Slicer, Tile, Dashboard, TileResult, DashboardResult, JoinKind, AnalysisSpec, AnalysisResult, SavedAnalysis, ReplaceSpec, ReplacePreview, ReplaceOutcome, DisplayKind, DisplayFormat, Condition, ColorRule, Presentation,
        alephgrid_core::external::ResultType, alephgrid_core::external::RuntimeIdentity, crate::python::Limits,
        FunctionDefinition, FunctionKind, FunctionTest, FunctionTestResult, FunctionVersion, WorkbookFunction,
        Kind, FormulaLanguage, Column, CellErrorMark, TableInfo, SortKey, FilterOp, Filter, ViewSpec, SavedView, ViewInfo, RowBlock, CellEdit, EditOutcome,
        Stats, ColumnValues, ValueCount, FindHit, FindResult, CellStyle, StyledCell, StylePatch, SheetSearch, CellComment, SortBy, SheetColorRule, SheetRules, SheetValidation, SheetChart, SheetMatches, Axis, LineSize, MergeMode, SheetLayout, QueryPreview, ParamType, QueryParam, SourceRevision, QueryResult, TableSchema, QueryKind, SavedQuery, QueryVersion,
        MaterializedSource, MaterializedTable, SqlChangeOutcome, QueryExport, ExportFormat, Align, SheetInfo, SheetBlock, SheetInput, SheetCopy, NamedRange, CellError, CellDetail, FormulaProblem, CellArea, FillDirection, PasteMode, Imported,
        SpaceRole, OrgRole, Cap, AuthMode, Org, Space, PublicUser, OrgMember, MemberKind, SpaceMember, Token, AgentConnection, AgentActivity, Invite, AuditEvent, QuotaLimits, QuotaKind, SpaceUsage, LibraryKind, LibraryVersion, LibraryEntry,
        HistoryEntry, alephgrid_core::UndoState, alephgrid_core::UndoKind, alephgrid_core::UndoOutcome, Summary, WorkbookMeta, WorkbookListing, WorkbookPatch, Event, Op,
        ColumnType, PreviewColumn, FilePreview, UploadPreview, ImportProblem, UploadCreated, ImportRequest, JobStatus,
        Capability, RuntimeSpec, IsolationClass, ProviderKind, ComputeProfile, ComputePolicy, ProfileChoice, SpaceProfiles, AllocationState, ComputeState, FailureKind, RuntimeFailure,
        Reservation, Transition, RuntimeAllocation, OwnershipAssignment, RuntimeStatus, RuntimeAction, RuntimeRequest,
        HostBudget, AllocationUsage, ResourceUsage, BackupEntry, BackupManifest,
    );
    out.push_str("\n/** Result type of each operation. */\nexport interface Results {\n");
    for (op, ty) in RESULTS {
        out.push_str(&format!("  {op}: {ty};\n"));
    }
    out.push_str("}\n\nexport type OpName = Op[\"op\"];\n\n/** Parameters of an operation (its request without `op`). */\nexport type Params<O extends OpName> = Omit<Extract<Op, { op: O }>, \"op\">;\n");
    out
}
