//! WebSocket protocol. See docs/PROTOCOL.md.
//!
//! Client -> server: `{"id": 7, "op": "fetch", "view_id": 1, "row_start": 0, "row_count": 200}`
//! Server -> client: `{"id": 7, "ok": true, "result": {...}}` or `{"id": 7, "ok": false, "error": "...", "code": "..."}`
//! Server pushes:    `{"event": "cells_changed", "table": "flights", "origin": 3, "revision": 12}`
//!
//! Every request becomes a scheduled job (`sched.rs`): reads on reader connections,
//! changes one at a time on the writer, each change one revision (`Workbook::mutate`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use alephgrid_core::{CellEdit, ExportFormat, Mutation, QueryKind, QueryParam, Reader, SheetInput, ViewSpec, Workbook};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{watch, Notify};
use ts_rs::TS;

use crate::auth::{Cap, Principal, SpaceRole};
use crate::sched::{Class, JobError};
use crate::state::{Job, Shared, Work, WorkbookHandle};

#[derive(Debug, Clone, Serialize, TS)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Cell values changed; refetch visible rows of this table.
    CellsChanged { table: String, origin: u64, revision: u64 },
    /// Rows were inserted/deleted or columns changed; views on this table were rebuilt.
    TableChanged { table: String, origin: u64, revision: u64 },
    /// Tables or sheets were added, removed or renamed.
    TablesChanged { origin: u64, revision: u64 },
    /// Sheet cells changed. `sheet` is None when every sheet may have changed (undo/redo).
    SheetChanged { sheet: Option<u32>, origin: u64, revision: u64 },
    /// Saved views of this table were added, changed or removed.
    ViewsChanged { table: String, origin: u64, revision: u64 },
    /// Saved queries were added, changed or removed.
    QueriesChanged { origin: u64, revision: u64 },
    /// The workbook was moved to the trash; the server closes the connection.
    WorkbookDeleted { origin: u64 },
    /// The server closed the workbook (`reason`: "idle", "stopped" or "shutdown") and ends
    /// the connection; reconnecting reopens it. Unacknowledged requests may not have run.
    WorkbookClosed { reason: String },
    /// Events were dropped because this client was not reading fast enough: refresh
    /// everything shown (sent to that session only).
    Resync {},
    /// This connection lost its access (signed out, session expired, role removed, token
    /// revoked or the workbook moved to a space it cannot see); the server closes it.
    AccessRevoked { reason: String },
    /// This connection's role in the workbook's space changed.
    RoleChanged { role: SpaceRole },
    /// A long request's progress (sent to the session that made it).
    Progress { request: u64, percentage: f64 },
}

impl Event {
    /// Events after which the home-screen summary (counts, rows) may be stale.
    pub fn changes_structure(&self) -> bool {
        matches!(self, Event::TableChanged { .. } | Event::TablesChanged { .. })
    }
}

#[derive(Debug, Deserialize)]
struct Envelope {
    id: u64,
    /// Client-chosen id of a change; a retry with the same id is not applied twice.
    #[serde(default)]
    mutation_id: Option<String>,
    /// How long the client will wait (capped by the server's limit for the request kind).
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(flatten)]
    op: Op,
}

/// Every request a client can send (`{"id": n, "op": "...", ...params}`).
#[derive(Debug, Deserialize, TS)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Analyze { spec: alephgrid_core::AnalysisSpec },
    Analyses,
    SaveAnalysis {
        saved: Option<String>,
        name: String,
        spec: alephgrid_core::AnalysisSpec,
        chart: bool,
        #[serde(default)]
        #[ts(optional, as = "Option<alephgrid_core::reports::ChartKind>")]
        chart_kind: alephgrid_core::reports::ChartKind,
    },
    DeleteAnalysis { analysis: String },
    /// Dashboards (F.11): tiles of saved analyses on a 12×24 grid, with shared slicers.
    Dashboards,
    Dashboard { dashboard: String },
    SaveDashboard { saved: Option<String>, name: String, tiles: Vec<alephgrid_core::reports::Tile>, slicers: Vec<alephgrid_core::reports::Slicer> },
    DeleteDashboard { dashboard: String },
    /// Data tools (F.12): validation rules, transform previews and derived tables, formula
    /// inspection. Only `set_validations` and `apply_transform` change anything (and run on
    /// the writer); the others read.
    Validations { table: String },
    SetValidations { table: String, rules: Vec<alephgrid_core::tools::Validation> },
    PreviewTransform { table: String, transform: alephgrid_core::tools::Transform },
    ApplyTransform { table: String, transform: alephgrid_core::tools::Transform, name: String },
    InspectFormula { table: String, column: String },
    /// What an XLSX download leaves out or replaces by values (F.13), one note each.
    XlsxCompatibility,
    /// The view spec of the rows behind one result row of a summary or report.
    Drill { spec: alephgrid_core::AnalysisSpec, #[ts(type = "CellValue[]")] row: Vec<Value> },
    MaterializeAnalysis { name: String, spec: alephgrid_core::AnalysisSpec },
    ReplacePreview { spec: alephgrid_core::ReplaceSpec },
    ReplaceApply { spec: alephgrid_core::ReplaceSpec, revision: u64 },
    ReplaceUndo { revision: u64 },
    Presentation { table: String },
    SavePresentation { table: String, presentation: alephgrid_core::Presentation },
    Functions,
    TestFunction { definition: alephgrid_core::FunctionDefinition },
    PublishFunction { definition: alephgrid_core::FunctionDefinition },
    PinFunctions { pins: std::collections::BTreeMap<String,u32> },
    Tables,
    OpenView { spec: ViewSpec },
    UpdateView { view_id: u64, spec: ViewSpec },
    ViewInfo { view_id: u64 },
    CloseView { view_id: u64 },
    /// `column_start`/`column_count` select visible columns; the reply says where they start.
    Fetch {
        view_id: u64,
        row_start: u64,
        row_count: u64,
        #[serde(default)] #[ts(optional, as = "Option<u32>")] column_start: u32,
        #[serde(default)] #[ts(optional, as = "Option<u32>")] column_count: Option<u32>,
    },
    Edit { view_id: u64, edits: Vec<CellEdit> },
    InsertRows { view_id: u64, at: u64, count: u64 },
    DeleteRows { view_id: u64, start: u64, count: u64 },
    Stats { view_id: u64, column: String, start: u64, end: u64 },
    AddColumn { table: String, name: String, sql_type: String },
    /// `language` is `sql` (a DuckDB expression, the default) or `sheet` (spreadsheet syntax).
    AddFormulaColumn { table: String, name: String, formula: String, #[serde(default)] #[ts(optional, as = "Option<alephgrid_core::FormulaLanguage>")] language: alephgrid_core::FormulaLanguage },
    UpdateFormula { table: String, name: String, formula: String },
    DropColumn { table: String, name: String },
    RenameColumn { table: String, name: String, new_name: String },
    /// Preview a query: its first `limit` rows (at most 5,000), timing, revision and sources.
    Query {
        sql: String,
        #[serde(default)] #[ts(optional, as = "Option<Vec<QueryParam>>")] params: Vec<QueryParam>,
        #[serde(default = "default_limit")] #[ts(optional, as = "Option<usize>")] limit: usize,
    },
    /// Make a table of a query's complete result; `saved`/`version` record the saved query it came from.
    CreateTableFromQuery {
        name: String,
        sql: String,
        #[serde(default)] #[ts(optional, as = "Option<Vec<QueryParam>>")] params: Vec<QueryParam>,
        #[serde(default)] #[ts(optional, as = "Option<String>")] saved: String,
        #[serde(default)] #[ts(optional, as = "Option<u64>")] version: u64,
    },
    CreateDemoTable { #[serde(default = "default_demo_rows")] #[ts(optional, as = "Option<u64>")] rows: u64 },
    DropTable { table: String },
    RenameTable { table: String, new_name: String },
    // Sheets (IronCalc). Rows/columns are 0-based.
    Sheets,
    SheetCreate { #[serde(default)] #[ts(optional, as = "Option<String>")] name: String },
    SheetRename { sheet: u32, name: String },
    SheetDelete { sheet: u32 },
    SheetFetch { sheet: u32, row_start: u32, row_count: u32, column_count: u32 },
    SheetSet { sheet: u32, inputs: Vec<SheetInput> },
    SheetUndo,
    SheetRedo,
    SheetInsertRows { sheet: u32, at: u32, count: u32 },
    SheetDeleteRows { sheet: u32, at: u32, count: u32 },
    SheetInsertColumns { sheet: u32, at: u32, count: u32 },
    SheetDeleteColumns { sheet: u32, at: u32, count: u32 },
    /// Named cells and ranges (workbook functions are listed by `functions`).
    SheetNames,
    /// `scope` is a sheet index for a sheet-level name; omitted for the whole workbook.
    SheetDefineName { name: String, #[serde(default)] #[ts(optional, as = "Option<u32>")] scope: Option<u32>, refers_to: String },
    /// Rename, rescope or repoint a name; formulas using it follow a rename.
    SheetUpdateName {
        name: String,
        #[serde(default)] #[ts(optional, as = "Option<u32>")] scope: Option<u32>,
        new_name: String,
        #[serde(default)] #[ts(optional, as = "Option<u32>")] new_scope: Option<u32>,
        refers_to: String,
    },
    SheetDeleteName { name: String, #[serde(default)] #[ts(optional, as = "Option<u32>")] scope: Option<u32> },
    /// A cell's input and display, and for an error the cell it comes from.
    SheetCellDetail { sheet: u32, row: u32, column: u32 },
    /// Where a formula typed into a cell fails to parse; `null` when it parses.
    SheetCheckFormula { sheet: u32, row: u32, column: u32, formula: String },
    /// Copy the first row (down) or column (right) of `area` over the rest of it.
    SheetFill { sheet: u32, area: alephgrid_core::CellArea, direction: alephgrid_core::FillDirection },
    /// Paste `source` of `from_sheet` at (`row`, `column`) of `sheet`, translating formulas.
    SheetPaste {
        from_sheet: u32,
        source: alephgrid_core::CellArea,
        sheet: u32,
        row: u32,
        column: u32,
        #[serde(default = "default_paste_mode")] #[ts(optional, as = "Option<alephgrid_core::PasteMode>")] mode: alephgrid_core::PasteMode,
        #[serde(default)] #[ts(optional, as = "Option<bool>")] transpose: bool,
    },
    /// Cut and paste within one sheet; references to the moved cells follow them.
    SheetMove { sheet: u32, source: alephgrid_core::CellArea, row: u32, column: u32 },
    /// Copy a query's first rows into a new sheet. Fails with `too_large` when the result has
    /// more rows than a sheet copy holds, unless `allow_truncation`.
    CreateSheetFromQuery {
        name: String,
        sql: String,
        #[serde(default)] #[ts(optional, as = "Option<Vec<QueryParam>>")] params: Vec<QueryParam>,
        #[serde(default)] #[ts(optional, as = "Option<bool>")] allow_truncation: bool,
    },
    /// Copy the first rows of a table view (its sort and filters) into a new sheet.
    TableToSheet { view_id: u64, name: String },
    /// Saved (live) views of a table, or of every table.
    SavedViews { #[serde(default)] #[ts(optional, as = "Option<String>")] table: String },
    /// Create a saved view, or update the one with id `saved`. (Op parameters cannot be
    /// called `id`: that is the request frame's own id.)
    SaveView {
        #[serde(default)] #[ts(optional, as = "Option<String>")] saved: String,
        name: String,
        spec: ViewSpec,
        #[serde(default)] #[ts(optional, as = "Option<std::collections::BTreeMap<String, f64>>")] widths: std::collections::BTreeMap<String, f64>,
    },
    RenameSavedView { saved: String, name: String },
    DeleteSavedView { saved: String },
    /// Copy a view's current rows into a new table (a snapshot).
    TableFromView { view_id: u64, name: String },
    SheetToTable { sheet: u32, name: String },
    /// Committed changes after revision `after`, oldest first.
    History { #[serde(default)] #[ts(optional, as = "Option<u64>")] after: u64, #[serde(default = "default_history")] #[ts(optional, as = "Option<usize>")] limit: usize },
    /// Every table with its columns (schema browser, autocomplete).
    Schema,
    /// Write a query's complete result to a file; download it once from
    /// `/api/workbooks/{id}/downloads/{download}`.
    ExportQuery {
        sql: String,
        #[serde(default)] #[ts(optional, as = "Option<Vec<QueryParam>>")] params: Vec<QueryParam>,
        #[serde(default = "default_export_format")] #[ts(optional, as = "Option<ExportFormat>")] format: ExportFormat,
    },
    /// Apply an INSERT, UPDATE or DELETE as one change (record ids, revision, history).
    SqlChange { sql: String, #[serde(default)] #[ts(optional, as = "Option<Vec<QueryParam>>")] params: Vec<QueryParam> },
    SavedQueries,
    QueryVersions { saved: String },
    /// Create a saved query, or update the one with id `saved` (a new version when the SQL
    /// or parameters changed).
    SaveQuery {
        #[serde(default)] #[ts(optional, as = "Option<String>")] saved: String,
        name: String,
        #[serde(default = "default_query_kind")] #[ts(optional, as = "Option<QueryKind>")] kind: QueryKind,
        sql: String,
        #[serde(default)] #[ts(optional, as = "Option<Vec<QueryParam>>")] params: Vec<QueryParam>,
    },
    RenameQuery { saved: String, name: String },
    DeleteQuery { saved: String },
    /// Tables made from queries, with how they were computed and whether they are stale.
    Materialized,
    /// Recompute a table made from a query (all or nothing).
    RefreshMaterialized { table: String },
    /// Cancel this connection's request `request`: removed if it is waiting, interrupted if
    /// it runs. A change that already committed stays committed.
    Cancel { request: u64 },
    /// Recalculate a table's stored formula results in the background (F.09); for retrying
    /// after a failure or cancellation. Replies at once; `table_changed` follows when done.
    Recalculate { table: String },
    /// Cancel the background recalculation of a table's stored results; the last committed
    /// results stay, flagged. False when none runs.
    CancelRecalculation { table: String },
}

/// Default for the most rows copied from a table or query into a sheet (sheets keep every
/// cell in memory). Set with `--sheet-copy-rows`.
pub const SHEET_COPY_ROWS: usize = 100_000;
/// Most demo rows a client may ask for over the protocol.
pub const MAX_DEMO_ROWS: u64 = 100_000_000;

fn default_limit() -> usize {
    500
}

fn default_demo_rows() -> u64 {
    1_000_000
}

fn default_history() -> usize {
    100
}

fn default_paste_mode() -> alephgrid_core::PasteMode {
    alephgrid_core::PasteMode::All
}

fn default_export_format() -> ExportFormat {
    ExportFormat::Csv
}

fn default_query_kind() -> QueryKind {
    QueryKind::Query
}

/// What the connection's principal must be allowed to do in the workbook's space for `op`
/// (7.04). Every operation is listed, so a new one cannot be added without deciding.
pub fn required_caps(op: &Op) -> &'static [Cap] {
    use Op::*;
    const VIEW: &[Cap] = &[Cap::View];
    const EDIT: &[Cap] = &[Cap::Edit];
    const QUERY: &[Cap] = &[Cap::Query];
    const EDIT_QUERY: &[Cap] = &[Cap::Edit, Cap::Query];
    match op {
        Analyze { .. } | Analyses | Dashboards | Dashboard { .. } | Validations { .. } | PreviewTransform { .. } | InspectFormula { .. }
        | XlsxCompatibility | Drill { .. } | ReplacePreview { .. } | Presentation { .. } | Functions | Tables | OpenView { .. }
        | UpdateView { .. } | ViewInfo { .. } | CloseView { .. } | Fetch { .. } | Stats { .. } | Sheets | SheetFetch { .. } | SheetNames
        | SheetCellDetail { .. } | SheetCheckFormula { .. } | SavedViews { .. } | History { .. } | Schema | SavedQueries
        | QueryVersions { .. } | Materialized | Cancel { .. } => VIEW,
        TestFunction { definition } if definition.kind == alephgrid_core::FunctionKind::Sql => QUERY,
        TestFunction { .. } => VIEW,
        Query { .. } => QUERY,
        ExportQuery { .. } => &[Cap::Query, Cap::Export],
        PublishFunction { .. } | PinFunctions { .. } => &[Cap::Publish],
        CreateTableFromQuery { .. } | CreateSheetFromQuery { .. } | SqlChange { .. } | RefreshMaterialized { .. } => EDIT_QUERY,
        SaveAnalysis { .. } | DeleteAnalysis { .. } | SaveDashboard { .. } | DeleteDashboard { .. } | SetValidations { .. }
        | ApplyTransform { .. } | MaterializeAnalysis { .. } | ReplaceApply { .. } | ReplaceUndo { .. } | SavePresentation { .. }
        | Edit { .. } | InsertRows { .. } | DeleteRows { .. } | AddColumn { .. } | AddFormulaColumn { .. } | UpdateFormula { .. }
        | DropColumn { .. } | RenameColumn { .. } | CreateDemoTable { .. } | DropTable { .. } | RenameTable { .. } | SheetCreate { .. }
        | SheetRename { .. } | SheetDelete { .. } | SheetSet { .. } | SheetUndo | SheetRedo | SheetInsertRows { .. }
        | SheetDeleteRows { .. } | SheetInsertColumns { .. } | SheetDeleteColumns { .. } | SheetDefineName { .. }
        | SheetUpdateName { .. } | SheetDeleteName { .. } | SheetFill { .. } | SheetPaste { .. } | SheetMove { .. }
        | TableToSheet { .. } | SaveView { .. } | RenameSavedView { .. } | DeleteSavedView { .. } | TableFromView { .. }
        | SheetToTable { .. } | SaveQuery { .. } | RenameQuery { .. } | DeleteQuery { .. } | Recalculate { .. }
        | CancelRecalculation { .. } => EDIT,
    }
}

/// Where a request runs (see `sched.rs`): `(is_write, class)`; `None` is answered at once.
fn classify(op: &Op) -> Option<(bool, Class)> {
    use Op::*;
    match op {
        Analyses | Dashboards | Dashboard { .. } | Drill { .. } | XlsxCompatibility | Validations { .. } | PreviewTransform { .. } | InspectFormula { .. } | Presentation { .. } | Functions | Tables | ViewInfo { .. } | Fetch { .. } | Stats { .. } | Sheets | SheetFetch { .. } | SheetNames | SheetCellDetail { .. } | SheetCheckFormula { .. } | SavedViews { .. } | History { .. }
        | Schema | SavedQueries | QueryVersions { .. } | Materialized => Some((false, Class::Interactive)),
        Analyze { .. } | ReplacePreview { .. } | TestFunction { .. } | OpenView { .. } | UpdateView { .. } | Query { .. } | ExportQuery { .. } => Some((false, Class::Bulk)),
        CloseView { .. } | Cancel { .. } | CancelRecalculation { .. } | Recalculate { .. } => None,
        MaterializeAnalysis { .. } | ReplaceApply { .. } | ReplaceUndo { .. } | PublishFunction { .. } | PinFunctions { .. } | CreateTableFromQuery { .. } | CreateDemoTable { .. } | CreateSheetFromQuery { .. } | TableToSheet { .. }
        | TableFromView { .. } | SheetToTable { .. } | SqlChange { .. } | RefreshMaterialized { .. } => Some((true, Class::Bulk)),
        _ => Some((true, Class::Interactive)),
    }
}

/// The view a change reads positions from, if any.
fn view_of(op: &Op) -> Option<u64> {
    match op {
        Op::Edit { view_id, .. } | Op::InsertRows { view_id, .. } | Op::DeleteRows { view_id, .. }
        | Op::TableToSheet { view_id, .. } | Op::TableFromView { view_id, .. } => Some(*view_id),
        _ => None,
    }
}

/// `SheetSet` → `sheet_set`.
fn op_name(op: &Op) -> String {
    let decl = format!("{op:?}");
    let name: String = decl.chars().take_while(|c| c.is_alphanumeric()).collect();
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// A short description for the history, without formatting large payloads.
fn summary(op: &Op) -> String {
    match op {
        Op::Edit { edits, .. } => format!("{} cell(s)", edits.len()),
        Op::SheetSet { sheet, inputs } => format!("sheet {sheet}: {} cell(s)", inputs.len()),
        Op::CreateTableFromQuery { name, .. } | Op::CreateSheetFromQuery { name, .. } => format!("{} {name:?}", op_name(op)),
        Op::SqlChange { sql, .. } => format!("sql: {}", sql.chars().take(200).collect::<String>()),
        Op::SaveQuery { name, .. } => format!("save query {name:?}"),
        other => format!("{other:?}").chars().take(200).collect(),
    }
}

struct Ctx {
    session: u64,
    allow_sql: bool,
    sheet_copy_rows: usize,
    views: Arc<Mutex<HashSet<u64>>>,
    workbook: String,
    /// Where `export_query` writes: this workbook's export scratch directory.
    export_dir: std::path::PathBuf,
    downloads: Arc<crate::state::Downloads>,
}

/// A query result written to a file, ready to download once.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct QueryExport {
    /// Download it from `/api/workbooks/{id}/downloads/{download}` (once, within an hour).
    pub download: String,
    pub file_name: String,
    pub rows: u64,
    pub bytes: u64,
    /// Workbook revision of the snapshot that was exported.
    pub revision: u64,
}

fn require_sql(ctx: &Ctx) -> Result<()> {
    if !ctx.allow_sql {
        bail!("SQL is disabled on this server");
    }
    Ok(())
}

fn read(r: &Reader, op: Op, ctx: &Ctx) -> Result<Value> {
    Ok(match op {
        Op::Tables => json!(r.list_tables()?),
        Op::OpenView { spec } => {
            let info = r.open_view(spec)?;
            ctx.views.lock().unwrap().insert(info.view_id);
            json!(info)
        }
        Op::UpdateView { view_id, spec } => json!(r.update_view(view_id, spec)?),
        Op::ViewInfo { view_id } => json!(r.view_info(view_id)?),
        Op::Fetch { view_id, row_start, row_count, column_start, column_count } => {
            json!(r.fetch(view_id, row_start, row_count, column_start, column_count)?)
        }
        Op::Stats { view_id, column, start, end } => json!(r.stats(view_id, &column, start, end)?),
        Op::Query { sql, params, limit } => {
            require_sql(ctx)?;
            json!(r.run_query(&sql, &params, limit)?)
        }
        Op::ExportQuery { sql, params, format } => {
            require_sql(ctx)?;
            let dir = ctx.export_dir.join(crate::state::new_id());
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(format!("result.{}", format.extension()));
            match r.export_query(&sql, &params, &path, format) {
                Ok((rows, revision)) => {
                    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    let file_name = format!("query.{}", format.extension());
                    let download = ctx.downloads.register(&ctx.workbook, dir, path, &file_name);
                    json!(QueryExport { download, file_name, rows, bytes, revision })
                }
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&dir);
                    return Err(e);
                }
            }
        }
        Op::Analyze { spec } => json!(r.analyze(&spec)?),
        Op::Analyses => json!(r.analyses()?),
        Op::Dashboards => json!(r.dashboards()?),
        Op::Dashboard { dashboard } => json!(r.dashboard_results(&dashboard)?),
        Op::Drill { spec, row } => json!(r.drill(&spec, &row)?),
        Op::XlsxCompatibility => json!(r.xlsx_compatibility()?),
        Op::Validations { table } => json!(r.validations(&table)?),
        Op::PreviewTransform { table, transform } => json!(r.preview_transform(&table, &transform)?),
        Op::InspectFormula { table, column } => json!(r.inspect_formula(&table, &column)?),
        Op::ReplacePreview { spec } => json!(r.replace_preview(&spec)?),
        Op::Presentation { table } => json!(r.presentation(&table)?),
        Op::TestFunction { definition } => {
            if definition.kind == alephgrid_core::FunctionKind::Sql { require_sql(ctx)?; }
            json!(r.test_function(&definition)?)
        }
        Op::Functions => json!(r.functions()?),
        Op::Schema => json!(r.schema()?),
        Op::SavedQueries => json!(r.saved_queries()?),
        Op::QueryVersions { saved } => json!(r.query_versions(&saved)?),
        Op::Materialized => json!(r.materialized()?),
        Op::Sheets => json!(r.sheets().list()),
        Op::SheetFetch { sheet, row_start, row_count, column_count } => json!(r.sheets().fetch(sheet, row_start, row_count, column_count)?),
        Op::SheetNames => json!(r.sheets().names()),
        Op::SheetCellDetail { sheet, row, column } => json!(r.sheets().cell_detail(sheet, row, column)?),
        Op::SheetCheckFormula { sheet, row, column, formula } => json!(r.check_sheet_formula(sheet, row, column, &formula)?),
        Op::SavedViews { table } => json!(r.saved_views(Some(table.as_str()).filter(|t| !t.is_empty()))?),
        Op::History { after, limit } => json!(r.history(after, limit.min(1000))?),
        other => bail!("{} is not a read", op_name(&other)),
    })
}

/// A change, inside the caller's [`Workbook::mutate`] transaction. Returns the reply and
/// the event to broadcast (its revision is filled in after commit).
fn write(wb: &mut Workbook, op: Op, ctx: &Ctx) -> Result<(Value, Option<Event>)> {
    let origin = ctx.session;
    let changed = |table: String| Some(Event::TableChanged { table, origin, revision: 0 });
    let tables = || Some(Event::TablesChanged { origin, revision: 0 });
    let sheet_changed = |sheet: u32| Some(Event::SheetChanged { sheet: Some(sheet), origin, revision: 0 });
    Ok(match op {
        Op::SaveAnalysis { saved, name, spec, chart, chart_kind } => (json!(wb.save_analysis_as(saved.as_deref(), &name, spec, chart, chart_kind)?),Some(Event::Resync {})),
        Op::SetValidations { table, rules } => (json!(wb.set_validations(&table, rules)?), Some(Event::TableChanged { table, origin, revision: 0 })),
        Op::ApplyTransform { table, transform, name } => (json!(wb.apply_transform(&table, &transform, &name)?), Some(Event::TablesChanged { origin, revision: 0 })),
        Op::SaveDashboard { saved, name, tiles, slicers } => (json!(wb.save_dashboard(saved.as_deref(), &name, tiles, slicers)?), Some(Event::Resync {})),
        Op::DeleteDashboard { dashboard } => { wb.delete_dashboard(&dashboard)?; (Value::Null, Some(Event::Resync {})) },
        Op::DeleteAnalysis { analysis } => { wb.delete_analysis(&analysis)?; (Value::Null,Some(Event::Resync {})) },
        Op::MaterializeAnalysis { name, spec } => (json!(wb.materialize_analysis(&name,&spec)?),tables()),
        Op::ReplaceApply { spec, revision } => {let out=wb.replace_apply(&spec,revision)?;let event=changed(out.table.clone());(json!(out),event)},
        Op::ReplaceUndo { revision } => {let out=wb.replace_undo(revision)?;let event=changed(out.table.clone());(json!(out),event)},
        Op::SavePresentation { table, presentation } => (json!(wb.save_presentation(&table,presentation)?),Some(Event::Resync {})),
        Op::PublishFunction { definition } => {
            if definition.kind == alephgrid_core::FunctionKind::Sql { require_sql(ctx)?; }
            (json!(wb.publish_function(definition)?), Some(Event::Resync {}))
        }
        Op::PinFunctions { pins } => {
            if wb.functions()?.iter().any(|f| pins.contains_key(&f.name) && f.versions.iter().any(|v| v.definition.kind == alephgrid_core::FunctionKind::Sql)) { require_sql(ctx)?; }
            (json!(wb.pin_functions(&pins)?), Some(Event::Resync {}))
        }
        Op::Edit { view_id, edits } => {
            let out = wb.edit(view_id, &edits)?;
            let event = if out.rows_appended > 0 {
                Event::TableChanged { table: out.table.clone(), origin, revision: 0 }
            } else {
                Event::CellsChanged { table: out.table.clone(), origin, revision: 0 }
            };
            (json!(out), Some(event))
        }
        Op::InsertRows { view_id, at, count } => (Value::Null, changed(wb.insert_rows(view_id, at, count)?)),
        Op::DeleteRows { view_id, start, count } => (Value::Null, changed(wb.delete_rows(view_id, start, count)?)),
        Op::AddColumn { table, name, sql_type } => {
            wb.add_column(&table, &name, &sql_type)?;
            (Value::Null, changed(table))
        }
        Op::AddFormulaColumn { table, name, formula, language } => {
            wb.add_formula_column_as(&table, &name, &formula, language)?;
            (Value::Null, changed(table))
        }
        Op::UpdateFormula { table, name, formula } => {
            wb.update_formula(&table, &name, &formula)?;
            (Value::Null, changed(table))
        }
        Op::DropColumn { table, name } => {
            wb.drop_column(&table, &name)?;
            (Value::Null, changed(table))
        }
        Op::RenameColumn { table, name, new_name } => {
            wb.rename_column(&table, &name, &new_name)?;
            (Value::Null, changed(table))
        }
        Op::CreateTableFromQuery { name, sql, params, saved, version } => {
            require_sql(ctx)?;
            let origin = Some(saved.as_str()).filter(|s| !s.is_empty()).map(|s| (s, version));
            (json!(wb.create_table_from_query_with(&name, &sql, &params, origin)?), tables())
        }
        Op::CreateDemoTable { rows } => (json!(wb.create_demo_table(rows.min(MAX_DEMO_ROWS))?), tables()),
        Op::DropTable { table } => {
            wb.drop_table(&table)?;
            (Value::Null, tables())
        }
        Op::RenameTable { table, new_name } => (json!(wb.rename_table(&table, &new_name)?), tables()),
        Op::SheetCreate { name } => {
            let name = if name.trim().is_empty() { "Sheet".to_string() } else { name };
            (json!(wb.with_sheets(|s| s.create(&name))?), tables())
        }
        Op::SheetRename { sheet, name } => {
            wb.with_sheets(|s| s.rename(sheet, &name))?;
            (Value::Null, tables())
        }
        Op::SheetDelete { sheet } => {
            wb.delete_sheet(sheet)?;
            (Value::Null, tables())
        }
        Op::SheetSet { sheet, inputs } => {
            wb.set_sheet_cells(sheet, &inputs)?;
            (json!(wb.sheets().info(sheet)?), sheet_changed(sheet))
        }
        Op::SheetUndo | Op::SheetRedo => {
            let before: Vec<String> = wb.sheets().list().into_iter().map(|s| s.name).collect();
            let undo = matches!(op, Op::SheetUndo);
            let changed = wb.with_sheets(|s| if undo { s.undo() } else { s.redo() })?;
            let after: Vec<String> = wb.sheets().list().into_iter().map(|s| s.name).collect();
            // Undoing a sheet creation, rename or deletion changes the tab list too.
            let event = if before != after {
                Event::TablesChanged { origin, revision: 0 }
            } else {
                Event::SheetChanged { sheet: None, origin, revision: 0 }
            };
            (json!(changed), changed.then_some(event))
        }
        Op::SheetInsertRows { sheet, at, count } => {
            wb.with_sheets(|s| s.insert_rows(sheet, at, count))?;
            (Value::Null, sheet_changed(sheet))
        }
        Op::SheetDeleteRows { sheet, at, count } => {
            wb.with_sheets(|s| s.delete_rows(sheet, at, count))?;
            (Value::Null, sheet_changed(sheet))
        }
        Op::SheetInsertColumns { sheet, at, count } => {
            wb.with_sheets(|s| s.insert_columns(sheet, at, count))?;
            (Value::Null, sheet_changed(sheet))
        }
        Op::SheetDeleteColumns { sheet, at, count } => {
            wb.with_sheets(|s| s.delete_columns(sheet, at, count))?;
            (Value::Null, sheet_changed(sheet))
        }
        Op::SheetDefineName { name, scope, refers_to } => {
            wb.with_sheets(|s| s.define_name(&name, scope, &refers_to))?;
            (json!(wb.sheets().names()), Some(Event::SheetChanged { sheet: None, origin, revision: 0 }))
        }
        Op::SheetUpdateName { name, scope, new_name, new_scope, refers_to } => {
            wb.with_sheets(|s| s.update_name(&name, scope, &new_name, new_scope, &refers_to))?;
            (json!(wb.sheets().names()), Some(Event::SheetChanged { sheet: None, origin, revision: 0 }))
        }
        Op::SheetDeleteName { name, scope } => {
            wb.with_sheets(|s| s.delete_name(&name, scope))?;
            (json!(wb.sheets().names()), Some(Event::SheetChanged { sheet: None, origin, revision: 0 }))
        }
        Op::SheetFill { sheet, area, direction } => {
            wb.with_sheets(|s| s.fill(sheet, area, direction))?;
            (json!(wb.sheets().info(sheet)?), sheet_changed(sheet))
        }
        Op::SheetPaste { from_sheet, source, sheet, row, column, mode, transpose } => {
            let area = wb.with_sheets(|s| s.paste(from_sheet, source, sheet, (row, column), mode, transpose))?;
            (json!(area), sheet_changed(sheet))
        }
        Op::SheetMove { sheet, source, row, column } => {
            let area = wb.with_sheets(|s| s.move_cells(sheet, source, (row, column)))?;
            (json!(area), sheet_changed(sheet))
        }
        Op::CreateSheetFromQuery { name, sql, params, allow_truncation } => {
            require_sql(ctx)?;
            (json!(wb.create_sheet_from_query_with(&name, &sql, &params, ctx.sheet_copy_rows, allow_truncation)?), tables())
        }
        Op::SqlChange { sql, params } => {
            require_sql(ctx)?;
            let out = wb.sql_change(&sql, &params)?;
            let table = out.table.clone();
            (json!(out), changed(table))
        }
        Op::SaveQuery { saved, name, kind, sql, params } => {
            require_sql(ctx)?;
            let id = Some(saved.as_str()).filter(|i| !i.is_empty());
            (json!(wb.save_query(id, &name, kind, &sql, &params)?), Some(Event::QueriesChanged { origin, revision: 0 }))
        }
        Op::RenameQuery { saved, name } => (json!(wb.rename_query(&saved, &name)?), Some(Event::QueriesChanged { origin, revision: 0 })),
        Op::DeleteQuery { saved } => {
            wb.delete_query(&saved)?;
            (Value::Null, Some(Event::QueriesChanged { origin, revision: 0 }))
        }
        Op::RefreshMaterialized { table } => {
            require_sql(ctx)?;
            (json!(wb.refresh_materialized(&table)?), changed(table))
        }
        Op::SaveView { saved, name, spec, widths } => {
            let id = Some(saved.as_str()).filter(|i| !i.is_empty());
            let table = spec.table.clone();
            (json!(wb.save_view(id, &name, spec, widths)?), Some(Event::ViewsChanged { table, origin, revision: 0 }))
        }
        Op::RenameSavedView { saved, name } => {
            let v = wb.rename_saved_view(&saved, &name)?;
            let table = v.spec.table.clone();
            (json!(v), Some(Event::ViewsChanged { table, origin, revision: 0 }))
        }
        Op::DeleteSavedView { saved } => {
            let table = wb.saved_views(None)?.into_iter().find(|v| v.id == saved).map(|v| v.spec.table).unwrap_or_default();
            wb.delete_saved_view(&saved)?;
            (Value::Null, Some(Event::ViewsChanged { table, origin, revision: 0 }))
        }
        Op::TableFromView { view_id, name } => (json!(wb.table_from_view(view_id, &name)?), tables()),
        Op::TableToSheet { view_id, name } => (json!(wb.sheet_from_view(view_id, &name, ctx.sheet_copy_rows)?), tables()),
        Op::SheetToTable { sheet, name } => (json!(wb.sheet_to_table(sheet, &name)?), tables()),
        other => bail!("{} is not a change", op_name(&other)),
    })
}

fn with_revision(ev: Event, rev: u64) -> Event {
    match ev {
        Event::CellsChanged { table, origin, .. } => Event::CellsChanged { table, origin, revision: rev },
        Event::TableChanged { table, origin, .. } => Event::TableChanged { table, origin, revision: rev },
        Event::TablesChanged { origin, .. } => Event::TablesChanged { origin, revision: rev },
        Event::SheetChanged { sheet, origin, .. } => Event::SheetChanged { sheet, origin, revision: rev },
        Event::ViewsChanged { table, origin, .. } => Event::ViewsChanged { table, origin, revision: rev },
        Event::QueriesChanged { origin, .. } => Event::QueriesChanged { origin, revision: rev },
        other => other,
    }
}

/// Machine-readable code for an error reply, if the error has one.
pub fn error_code(e: &anyhow::Error) -> Option<&'static str> {
    for c in e.chain() {
        if let Some(j) = c.downcast_ref::<JobError>() {
            return Some(j.code());
        }
        if let Some(d) = c.downcast_ref::<crate::auth::Denied>() {
            return Some(d.code());
        }
        if c.downcast_ref::<crate::quota::QuotaExceeded>().is_some() {
            return Some("quota");
        }
        if c.downcast_ref::<alephgrid_core::limits::TooLarge>().is_some() {
            return Some("too_large");
        }
        if c.downcast_ref::<alephgrid_core::NotAllowed>().is_some() {
            return Some("not_allowed");
        }
        if c.downcast_ref::<alephgrid_core::IsChange>().is_some() {
            return Some("sql_change");
        }
        if c.downcast_ref::<alephgrid_core::BadParameter>().is_some() {
            return Some("bad_parameter");
        }
        if c.downcast_ref::<alephgrid_core::SchemaChanged>().is_some() {
            return Some("schema_changed");
        }
        if let Some(f) = c.downcast_ref::<crate::runtime::RuntimeFailure>() {
            return Some(match f.kind {
                crate::runtime::FailureKind::Capacity => "capacity",
                _ => "unavailable",
            });
        }
    }
    let text = format!("{e:#}");
    if text.contains("this workbook was closed") || text.contains("shutting down") {
        return Some("unavailable");
    }
    None
}

/// `text` with the data directory (as configured and as resolved) replaced by `<data>`.
pub fn redact(text: &str, data_dir: &std::path::Path) -> String {
    let mut out = text.to_string();
    let mut forms = vec![data_dir.to_string_lossy().to_string()];
    if let Ok(c) = data_dir.canonicalize() {
        forms.push(c.to_string_lossy().to_string());
    }
    forms.sort_by_key(|f| std::cmp::Reverse(f.len()));
    for f in forms.iter().filter(|f| f.len() > 1) {
        out = out.replace(f.as_str(), "<data>");
    }
    out
}

// ------------------------------------------------------------------ outbox

/// A session's unsent frames, bounded in bytes. Replies always queue; events are dropped
/// (and a `resync` sent later) once the queue is above the pause mark; past the maximum the
/// session is closed. The read loop stops reading requests while the queue is above the
/// pause mark, so a client that does not read its replies cannot make the server buffer
/// without limit.
pub struct Outbox {
    queue: Mutex<VecDeque<String>>,
    bytes: AtomicUsize,
    peak: AtomicUsize,
    lagged: AtomicBool,
    pub too_slow: AtomicBool,
    pause: usize,
    max: usize,
    ready: Notify,
    drained: Notify,
}

impl Outbox {
    fn new(pause: usize, max: usize) -> Outbox {
        Outbox {
            queue: Mutex::new(VecDeque::new()),
            bytes: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            lagged: AtomicBool::new(false),
            too_slow: AtomicBool::new(false),
            pause,
            max,
            ready: Notify::new(),
            drained: Notify::new(),
        }
    }

    fn push(&self, text: String, reply: bool) {
        let len = text.len();
        let now = self.bytes.load(Ordering::SeqCst);
        if !reply && now + len > self.pause {
            self.lagged.store(true, Ordering::SeqCst);
            return;
        }
        let total = self.bytes.fetch_add(len, Ordering::SeqCst) + len;
        self.peak.fetch_max(total, Ordering::SeqCst);
        if total > self.max {
            self.too_slow.store(true, Ordering::SeqCst);
            self.drained.notify_waiters();
        }
        self.queue.lock().unwrap().push_back(text);
        self.ready.notify_one();
    }

    fn pop(&self) -> Option<String> {
        self.queue.lock().unwrap().pop_front()
    }

    fn sent(&self, len: usize) {
        let left = self.bytes.fetch_sub(len, Ordering::SeqCst) - len;
        if left <= self.pause {
            self.drained.notify_waiters();
        }
        if left < self.pause / 2 && self.lagged.swap(false, Ordering::SeqCst) {
            self.push(serde_json::to_string(&Event::Resync {}).unwrap(), true);
        }
    }

    pub fn bytes(&self) -> usize {
        self.bytes.load(Ordering::SeqCst)
    }

    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
}

/// What `/api/admin/sessions` reports about one connection.
pub struct SessionInfo {
    pub workbook: String,
    pub outbox: Arc<Outbox>,
}

/// Recheck a connection's access: its credential is still valid and it still sees the
/// workbook's space. Returns the current principal and role.
fn recheck(state: &Shared, principal: &Principal, workbook: &str) -> Result<(Principal, SpaceRole, crate::auth::Space)> {
    let p = state.auth.still_valid(principal)?;
    let (space, role) = state.require_workbook(&p, workbook, Cap::View)?;
    Ok((p, role, space))
}

/// What a committed change, an export or a publication puts in the audit log (7.10).
fn audit(state: &Shared, p: &Principal, space: &crate::auth::Space, workbook: &str, (op, summary): (&str, String), c: &alephgrid_core::Committed) {
    let (revision, result) = (c.revision, &c.result);
    let detail = json!({ "op": op, "revision": revision, "summary": summary });
    state.auth.record(p, "workbook_changed", space, workbook, detail);
    match op {
        "publish_function" => {
            let versions = result["versions"].as_array().map(|v| v.len()).unwrap_or(0);
            state.auth.record(p, "function_published", space, workbook, json!({ "function": result["name"], "version": versions, "revision": revision }));
        }
        "pin_functions" => state.auth.record(p, "functions_pinned", space, workbook, json!({ "revision": revision })),
        _ => {}
    }
}

pub async fn serve_socket(socket: WebSocket, state: Shared, handle: Arc<WorkbookHandle>, principal: Principal, role: SpaceRole, space: crate::auth::Space) {
    let session = state.next_session_id();
    // Subscribe before anything else, so an event during setup (a deletion) is not missed.
    let mut events = handle.events.subscribe();
    handle.session_started();
    let _ = handle.touch(true);
    let outbox = Arc::new(Outbox::new(state.config.outbox_pause_bytes, state.config.outbox_max_bytes));
    state.sessions.lock().unwrap().insert(session, SessionInfo { workbook: handle.id.clone(), outbox: outbox.clone() });
    let revision = {
        let h = handle.clone();
        tokio::task::spawn_blocking(move || {
            // Keep home-screen counts accurate, including for workbooks made by older versions.
            let _ = h.refresh_summary();
            h.with_reader(|r| r.revision()).unwrap_or(0)
        })
        .await
        .unwrap_or(0)
    };
    let (mut sink, mut stream) = socket.split();
    let hello = json!({ "event": "hello", "session": session, "workbook": handle.meta.lock().unwrap().clone(), "revision": revision, "role": role });
    outbox.push(hello.to_string(), true);

    // Writer: sends queued frames in order; closes the socket when told to.
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let writer = {
        let outbox = outbox.clone();
        let mut stop = stop_rx.clone();
        tokio::spawn(async move {
            // Once the session ends, a client that does not read gets this long to take
            // what is queued; then it is abandoned.
            let mut give_up: Option<tokio::time::Instant> = None;
            loop {
                while let Some(text) = outbox.pop() {
                    let len = text.len();
                    // A stop arriving mid-send must not drop this frame (it may be the
                    // closing notice): keep sending it, now with a deadline.
                    let mut send = std::pin::pin!(sink.send(Message::Text(text.into())));
                    let sent = match give_up {
                        Some(t) => tokio::time::timeout_at(t, &mut send).await.ok(),
                        None => tokio::select! {
                            r = &mut send => Some(r),
                            _ = stop.changed() => {
                                let t = tokio::time::Instant::now() + Duration::from_secs(2);
                                give_up = Some(t);
                                tokio::time::timeout_at(t, &mut send).await.ok()
                            }
                        },
                    };
                    match sent {
                        Some(Ok(())) => outbox.sent(len),
                        _ => return,
                    }
                    if outbox.too_slow.load(Ordering::SeqCst) {
                        return;
                    }
                }
                if *stop.borrow() {
                    let deadline = give_up.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(2));
                    let _ = tokio::time::timeout_at(deadline, sink.send(Message::Close(None))).await;
                    return;
                }
                tokio::select! {
                    _ = outbox.ready.notified() => {}
                    _ = stop.changed() => {}
                }
            }
        })
    };

    // Events from other sessions and from the server.
    if handle.is_closing() {
        // Closed (deleted or stopped) before we subscribed: say so and end.
        let ev = if handle.is_deleted() { Event::WorkbookDeleted { origin: 0 } } else { Event::WorkbookClosed { reason: "stopped".into() } };
        let _ = handle.events.send(ev);
    }
    let forwarder = {
        let outbox = outbox.clone();
        let stop_tx = stop_tx.clone();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(ev) => {
                        let end = matches!(ev, Event::WorkbookDeleted { .. } | Event::WorkbookClosed { .. });
                        outbox.push(serde_json::to_string(&ev).unwrap(), end);
                        if end {
                            let _ = stop_tx.send(true);
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        outbox.lagged.store(true, Ordering::SeqCst);
                    }
                    Err(_) => break,
                }
            }
        })
    };

    // Access: rechecked whenever identities or roles change and when the credential expires.
    // Losing it ends the connection; a changed role applies to the next request.
    let access = Arc::new(Mutex::new((principal, role, space)));
    let guard = {
        let (outbox, stop_tx, access, state, workbook) = (outbox.clone(), stop_tx.clone(), access.clone(), state.clone(), handle.id.clone());
        let mut changes = state.auth.subscribe();
        let mut stop = stop_rx.clone();
        tokio::spawn(async move {
            loop {
                let expires = access.lock().unwrap().0.expires_at();
                let until = expires.map(|e| Duration::from_millis(e.saturating_sub(crate::auth::now_ms()))).unwrap_or(Duration::from_secs(3600));
                tokio::select! {
                    r = changes.changed() => if r.is_err() { break },
                    _ = tokio::time::sleep(until) => {}
                    _ = stop.changed() => break,
                }
                let (p, before, _) = access.lock().unwrap().clone();
                let (s, w) = (state.clone(), workbook.clone());
                let checked = tokio::task::spawn_blocking(move || recheck(&s, &p, &w)).await;
                match checked {
                    Ok(Ok((p, role, space))) => {
                        *access.lock().unwrap() = (p, role, space);
                        if role != before {
                            outbox.push(serde_json::to_string(&Event::RoleChanged { role }).unwrap(), true);
                        }
                    }
                    Ok(Err(e)) if e.chain().any(|c| c.downcast_ref::<crate::auth::Denied>().is_some()) => {
                        let reason = match e.chain().find_map(|c| c.downcast_ref::<crate::auth::Denied>()) {
                            Some(crate::auth::Denied::Unauthenticated) => "signed_out",
                            _ => "no_access",
                        };
                        outbox.push(serde_json::to_string(&Event::AccessRevoked { reason: reason.into() }).unwrap(), true);
                        let _ = stop_tx.send(true);
                        break;
                    }
                    // A failure to check (the control database) is not a revocation; try again.
                    _ => tokio::time::sleep(Duration::from_secs(1)).await,
                }
            }
        })
    };
    let views = Arc::new(Mutex::new(HashSet::new()));
    let cancels: Arc<Mutex<HashMap<u64, watch::Sender<bool>>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut jobs = tokio::task::JoinSet::new();
    loop {
        // Backpressure: do not read more requests while replies pile up unsent.
        while outbox.bytes() > outbox.pause && !outbox.too_slow.load(Ordering::SeqCst) && !*stop_rx.borrow() {
            tokio::select! {
                _ = outbox.drained.notified() => {}
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                _ = stop_rx.changed() => {}
            }
        }
        if outbox.too_slow.load(Ordering::SeqCst) {
            tracing::warn!(session, "closing a connection whose client is not reading its replies");
            break;
        }
        let msg = tokio::select! {
            msg = stream.next() => match msg {
                Some(Ok(msg)) => msg,
                _ => break,
            },
            _ = stop_rx.changed() => break,
            Some(_) = jobs.join_next(), if !jobs.is_empty() => continue,
        };
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };
        let envelope: Envelope = match serde_json::from_str(&text) {
            Ok(e) => e,
            Err(e) => {
                let id = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["id"].as_u64());
                outbox.push(json!({ "id": id, "ok": false, "error": format!("bad request: {e}") }).to_string(), true);
                continue;
            }
        };
        let id = envelope.id;
        // Authorization of this request: the role as last rechecked, and an expired
        // credential refuses everything (its revocation follows from the guard).
        let (expired, role, space_now) = {
            let a = access.lock().unwrap();
            (a.0.expires_at().is_some_and(|e| e <= crate::auth::now_ms()), a.1, a.2.clone())
        };
        if expired {
            let e = anyhow::Error::new(crate::auth::Denied::Unauthenticated);
            outbox.push(error_reply(id, &e, &state.config.data_dir).to_string(), true);
            continue;
        }
        if let Some(cap) = required_caps(&envelope.op).iter().find(|c| !space_now.allows(role, **c)) {
            let e = crate::auth::forbidden(cap.describe());
            outbox.push(error_reply(id, &e, &state.config.data_dir).to_string(), true);
            continue;
        }
        let Some((is_write, class)) = classify(&envelope.op) else {
            let result = match envelope.op {
                Op::CloseView { view_id } => {
                    views.lock().unwrap().remove(&view_id);
                    let h = handle.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        h.with_reader(|r| {
                            r.close_view(view_id);
                            Ok(())
                        })
                    })
                    .await;
                    Value::Null
                }
                Op::Cancel { request } => {
                    let found = cancels.lock().unwrap().get(&request).map(|c| c.send(true).is_ok()).unwrap_or(false);
                    json!(found)
                }
                Op::CancelRecalculation { table } => json!(handle.cancel_recalculation(&table)),
                Op::Recalculate { table } => {
                    let h = handle.clone();
                    let t = table.clone();
                    let ticket = tokio::task::spawn_blocking(move || h.with_reader(|r| Ok(r.shared().results_queued(&t)))).await;
                    match ticket {
                        Ok(Ok(k)) => {
                            handle.recalculate_later(vec![(table, k)], 0, state.config.job_timeout);
                            json!(true)
                        }
                        _ => json!(false),
                    }
                }
                _ => unreachable!("classified as immediate"),
            };
            outbox.push(json!({ "id": id, "ok": true, "result": result }).to_string(), true);
            continue;
        };
        // Bulk work holds one of the space's job slots while queued and running, and bulk
        // changes are refused once the space is at its storage limit (7.07).
        let slot = if class == Class::Bulk {
            let space_id = access.lock().unwrap().2.id.clone();
            let s = state.clone();
            let checked = tokio::task::spawn_blocking(move || {
                if is_write {
                    s.check_storage(&space_id, 0)?;
                }
                s.job_slot(&space_id)
            })
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
            match checked {
                Ok(slot) => Some(slot),
                Err(e) => {
                    outbox.push(error_reply(id, &e, &state.config.data_dir).to_string(), true);
                    continue;
                }
            }
        } else {
            None
        };
        let limit = if class == Class::Bulk { state.config.job_timeout } else { state.config.request_timeout };
        let timeout = envelope.timeout_ms.map(Duration::from_millis).unwrap_or(limit).min(limit);
        let job = Job { key: (session, id), class, deadline: Instant::now() + timeout };
        // Queue in arrival order: requests of one connection run in the order sent.
        let lane = if is_write { crate::sched::Lane::Write } else { crate::sched::Lane::Read };
        let ticket = match handle.enqueue(&job, lane) {
            Ok(t) => t,
            Err(e) => {
                outbox.push(error_reply(id, &e, &state.config.data_dir).to_string(), true);
                continue;
            }
        };
        let (cancel_tx, cancel_rx) = watch::channel(false);
        cancels.lock().unwrap().insert(id, cancel_tx);
        let ctx = Ctx {
            session,
            allow_sql: state.config.allow_sql,
            sheet_copy_rows: state.config.sheet_copy_rows,
            views: views.clone(),
            workbook: handle.id.clone(),
            export_dir: state.workbook_scratch(".exports", &handle.id),
            downloads: state.downloads.clone(),
        };
        let data_dir = state.config.data_dir.clone();
        let recalc_timeout = state.config.job_timeout;
        let (handle, outbox, cancels) = (handle.clone(), outbox.clone(), cancels.clone());
        let progress: Option<Box<dyn Fn(f64) + Send>> = (class == Class::Bulk).then(|| {
            let outbox = outbox.clone();
            Box::new(move |p: f64| outbox.push(serde_json::to_string(&Event::Progress { request: id, percentage: p }).unwrap(), false))
                as Box<dyn Fn(f64) + Send>
        });
        let (principal, space) = {
            let a = access.lock().unwrap();
            (a.0.clone(), a.2.clone())
        };
        let actor = principal.actor();
        let audit_state = state.clone();
        let op = envelope.op;
        let mutation_id = envelope.mutation_id;
        jobs.spawn(async move {
            let _slot = slot;
            let reply = if is_write {
                let name = op_name(&op);
                let audit_op = name.clone();
                let summary = summary(&op);
                let audit_summary = summary.clone();
                let view = view_of(&op);
                let work = Work::Write(Box::new(move |wb: &mut Workbook| {
                    let mut event = None;
                    let committed = wb.mutate(
                        Mutation { id: mutation_id.as_deref(), op: &name, actor: Some(&actor), summary },
                        view,
                        |wb| {
                            let (value, ev) = write(wb, op, &ctx)?;
                            event = ev;
                            Ok(value)
                        },
                    )?;
                    // Tables whose values changed through formulas that read what changed
                    // (another table's rows, sheet cells) get their own refresh event.
                    let derived: Vec<(String, bool)> = if committed.replayed || event.is_none() {
                        Vec::new()
                    } else {
                        wb.changed_tables(committed.revision).unwrap_or_else(|e| {
                            tracing::warn!("finding tables changed by revision {}: {e:#}", committed.revision);
                            Vec::new()
                        })
                    };
                    let sheets = !committed.replayed && wb.changed_table_cells();
                    // Stored formula results this change made stale, queued at once so views
                    // opened meanwhile show the last results, flagged, without rebuilding.
                    let queued: Vec<(String, u64)> = if committed.replayed {
                        Vec::new()
                    } else {
                        wb.stale_results().unwrap_or_default().into_iter().map(|t| { let k = wb.shared().results_queued(&t); (t, k) }).collect()
                    };
                    Ok((committed, event, derived, sheets, queued))
                }));
                match handle.run_queued(ticket, job, Some(cancel_rx), progress, work).await {
                    Ok((c, event, derived, sheets, queued)) => {
                        if !c.replayed {
                            audit(&audit_state, &principal, &space, &handle.id, (&audit_op, audit_summary), &c);
                            if class == Class::Bulk {
                                audit_state.quota.forget(&space.id);
                            }
                        }
                        handle.recalculate_later(queued, c.revision, recalc_timeout);
                        if let Some(ev) = event.filter(|_| !c.replayed) {
                            let ev = with_revision(ev, c.revision);
                            let structural = ev.changes_structure();
                            let primary = match &ev {
                                Event::CellsChanged { table, .. } | Event::TableChanged { table, .. } => Some(table.clone()),
                                _ => None,
                            };
                            let resync = matches!(ev, Event::Resync {});
                            let _ = handle.events.send(ev);
                            // Origin 0: no session has applied these changes itself.
                            for (table, restructured) in derived.into_iter().filter(|(t, r)| !resync && (*r || primary.as_ref() != Some(t))) {
                                let ev = if restructured || structural {
                                    Event::TableChanged { table, origin: 0, revision: c.revision }
                                } else {
                                    Event::CellsChanged { table, origin: 0, revision: c.revision }
                                };
                                let _ = handle.events.send(ev);
                            }
                            // Sheet cells that read the changed tables have new values.
                            if sheets && !resync {
                                let _ = handle.events.send(Event::SheetChanged { sheet: None, origin: 0, revision: c.revision });
                            }
                            let h = handle.clone();
                            tokio::task::spawn_blocking(move || {
                                let result = if structural { h.refresh_summary() } else { h.touch(false) };
                                if let Err(e) = result {
                                    tracing::warn!("updating workbook metadata: {e:#}");
                                }
                            });
                        }
                        let mut r = json!({ "id": id, "ok": true, "result": c.result, "revision": c.revision });
                        if c.replayed {
                            r["replayed"] = json!(true);
                        }
                        r
                    }
                    Err(e) => error_reply(id, &e, &data_dir),
                }
            } else {
                let downloads = ctx.downloads.clone();
                let exporting = matches!(op, Op::ExportQuery { .. });
                let cancelled = cancel_rx.clone();
                let work = Work::Read(Box::new(move |r: &Reader| read(r, op, &ctx)));
                let result = handle.run_queued(ticket, job, Some(cancel_rx), progress, work).await;
                // Leave the cancel table before deciding: a cancel that found the request
                // (and answered `true`) has already set the flag checked below.
                let was_cancelled = {
                    let mut c = cancels.lock().unwrap();
                    c.remove(&id);
                    *cancelled.borrow()
                };
                match result {
                    // A read cancelled while it finished anyway publishes nothing (an export's
                    // file is removed): the client has moved on.
                    Ok(v) if was_cancelled => {
                        if let Some(d) = v.get("download").and_then(Value::as_str) {
                            downloads.discard(d);
                        }
                        error_reply(id, &anyhow::Error::new(JobError::Cancelled), &data_dir)
                    }
                    Ok(v) => {
                        if exporting {
                            let detail = json!({ "kind": "query", "rows": v["rows"], "bytes": v["bytes"], "revision": v["revision"] });
                            audit_state.auth.record(&principal, "data_exported", &space, &handle.id, detail);
                        }
                        json!({ "id": id, "ok": true, "result": v })
                    }
                    Err(e) => error_reply(id, &e, &data_dir),
                }
            };
            cancels.lock().unwrap().remove(&id);
            outbox.push(reply.to_string(), true);
        });
    }

    // Queued work of this session is withdrawn and running work interrupted. A change
    // that already committed stays committed (a retry with its mutation id is not applied
    // twice).
    for (_, c) in cancels.lock().unwrap().drain() {
        let _ = c.send(true);
    }
    while jobs.join_next().await.is_some() {}
    forwarder.abort();
    guard.abort();
    let _ = stop_tx.send(true);
    if outbox.too_slow.load(Ordering::SeqCst) {
        writer.abort();
    }
    let _ = writer.await;
    state.sessions.lock().unwrap().remove(&session);
    handle.session_ended();
    let leftover: Vec<u64> = views.lock().unwrap().drain().collect();
    if !leftover.is_empty() && !handle.is_closing() {
        let h = handle.clone();
        let _ = tokio::task::spawn_blocking(move || {
            h.with_reader(|r| {
                for v in leftover {
                    r.close_view(v);
                }
                Ok(())
            })
        })
        .await;
    }
}

/// An error reply. Paths inside the server's data directory are not shown to clients.
fn error_reply(id: u64, e: &anyhow::Error, data_dir: &std::path::Path) -> Value {
    let mut r = json!({ "id": id, "ok": false, "error": redact(&format!("{e:#}"), data_dir) });
    if let Some(code) = error_code(e) {
        r["code"] = json!(code);
    }
    r
}
