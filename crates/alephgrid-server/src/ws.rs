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
use crate::state::{Job, Shared, Work};

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
    /// Refresh everything shown: a change whose effects are broad (analyses, functions),
    /// with its `revision`; or, without one when nothing is known, after events were
    /// dropped because this client was not reading fast enough (sent to that session only,
    /// with the latest revision announced so far).
    Resync {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        revision: Option<u64>,
    },
    /// A committed change with nothing to refresh (an undo with nothing to undo). Every
    /// revision is announced by at least one event, in revision order, so a client that
    /// sees a revision skip knows it missed something.
    Committed { origin: u64, revision: u64 },
    /// This connection lost its access (signed out, session expired, role removed, token
    /// revoked or the workbook moved to a space it cannot see); the server closes it.
    AccessRevoked { reason: String },
    /// This connection's role in the workbook's space changed.
    RoleChanged { role: SpaceRole },
    /// A long request's progress (sent to the session that made it).
    Progress { request: u64, percentage: f64 },
}

impl Event {
    /// The revision a change event announces.
    pub fn revision(&self) -> Option<u64> {
        match self {
            Event::CellsChanged { revision, .. }
            | Event::TableChanged { revision, .. }
            | Event::TablesChanged { revision, .. }
            | Event::SheetChanged { revision, .. }
            | Event::ViewsChanged { revision, .. }
            | Event::QueriesChanged { revision, .. }
            | Event::Committed { revision, .. } => Some(*revision),
            Event::Resync { revision } => *revision,
            _ => None,
        }
    }

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
    /// Compute volatile Python functions again and retry calls that ran out of time,
    /// memory or a worker (docs/PYTHON_FUNCTIONS.md).
    RefreshFunctions,
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
    /// `generation` (see `ViewInfo::generation`) guards edits that address rows by position
    /// (appends); edits by record are guarded by their `base` values.
    Edit { view_id: u64, edits: Vec<CellEdit>, #[serde(default)] #[ts(optional, as = "Option<i64>")] generation: Option<i64> },
    InsertRows { view_id: u64, at: u64, count: u64, #[serde(default)] #[ts(optional, as = "Option<i64>")] generation: Option<i64> },
    DeleteRows { view_id: u64, start: u64, count: u64, #[serde(default)] #[ts(optional, as = "Option<i64>")] generation: Option<i64> },
    /// Take back a table change: `revision`, or the caller's own latest change (`redo`:
    /// their latest undo). `null` when there is nothing to take back; a `conflict` when
    /// what it would take back changed since (see the engine's `undo.rs`).
    Undo {
        #[serde(default)] #[ts(optional, as = "Option<u64>")] revision: Option<u64>,
        #[serde(default)] #[ts(optional, as = "Option<bool>")] redo: bool,
    },
    Stats { view_id: u64, column: String, start: u64, end: u64 },
    /// Find text in a view's visible cells (U.17).
    FindInView { view_id: u64, text: String, #[serde(default)] #[ts(optional, as = "Option<bool>")] match_case: bool, #[serde(default)] #[ts(optional, as = "Option<u64>")] from: u64, #[serde(default)] #[ts(optional, as = "Option<bool>")] backwards: bool, #[serde(default)] #[ts(optional, as = "Option<bool>")] count: bool },
    /// Distinct values of a column with counts, for a header filter (U.16).
    ColumnValues { spec: ViewSpec, column: String, #[serde(default)] #[ts(optional, as = "Option<String>")] search: String, #[serde(default = "default_value_limit")] #[ts(optional, as = "Option<usize>")] limit: usize },
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
    // Sheets (IronCalc). Rows/columns are 0-based. `layout` (see `SheetInfo::layout`) guards
    // changes that address cells by position; `SheetInput::base` guards each cell's value.
    Sheets,
    SheetCreate { #[serde(default)] #[ts(optional, as = "Option<String>")] name: String },
    SheetRename { sheet: u32, name: String, #[serde(default)] #[ts(optional, as = "Option<u64>")] layout: Option<u64> },
    SheetDelete { sheet: u32, #[serde(default)] #[ts(optional, as = "Option<u64>")] layout: Option<u64> },
    SheetFetch { sheet: u32, row_start: u32, row_count: u32, column_count: u32 },
    SheetSet { sheet: u32, inputs: Vec<SheetInput>, #[serde(default)] #[ts(optional, as = "Option<u64>")] layout: Option<u64> },
    SheetUndo,
    SheetRedo,
    SheetInsertRows { sheet: u32, at: u32, count: u32, #[serde(default)] #[ts(optional, as = "Option<u64>")] layout: Option<u64> },
    SheetDeleteRows { sheet: u32, at: u32, count: u32, #[serde(default)] #[ts(optional, as = "Option<u64>")] layout: Option<u64> },
    SheetInsertColumns { sheet: u32, at: u32, count: u32, #[serde(default)] #[ts(optional, as = "Option<u64>")] layout: Option<u64> },
    SheetDeleteColumns { sheet: u32, at: u32, count: u32, #[serde(default)] #[ts(optional, as = "Option<u64>")] layout: Option<u64> },
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
    /// The block of filled cells around a cell (Ctrl+A).
    SheetRegion { sheet: u32, row: u32, column: u32 },
    /// Where a formula typed into a cell fails to parse; `null` when it parses.
    SheetCheckFormula { sheet: u32, row: u32, column: u32, formula: String },
    /// Copy the first row (down) or column (right) of `area` over the rest of it.
    SheetFill { sheet: u32, area: alephgrid_core::CellArea, direction: alephgrid_core::FillDirection, #[serde(default)] #[ts(optional, as = "Option<u64>")] layout: Option<u64> },
    /// Drag the fill handle from `source` to `target` (series, counting text or the pattern).
    SheetDragFill { sheet: u32, source: alephgrid_core::CellArea, target: alephgrid_core::CellArea, #[serde(default)] #[ts(optional, as = "Option<u64>")] layout: Option<u64> },
    /// Format an area (S.01, S.02): only the properties given change; one undo step.
    SheetStyle { sheet: u32, area: alephgrid_core::CellArea, style: alephgrid_core::StylePatch, #[serde(default)] #[ts(optional, as = "Option<u64>")] layout: Option<u64> },
    /// Find text in a sheet (S.04), as `find_in_view` does for tables.
    SheetFind { sheet: u32, search: alephgrid_core::SheetSearch, #[serde(default)] #[ts(optional, as = "Option<u64>")] from: u64, #[serde(default)] #[ts(optional, as = "Option<bool>")] backwards: bool, #[serde(default)] #[ts(optional, as = "Option<bool>")] count: bool },
    /// Replace in every matching cell of a sheet, as one undo step; replies how many changed.
    SheetReplace { sheet: u32, search: alephgrid_core::SheetSearch, replacement: String },
    /// The visible sheets with matches and their counts (S.04, all sheets).
    SheetFindAll { search: alephgrid_core::SheetSearch },
    /// Replace in every visible sheet as one undo step (S.04).
    SheetReplaceAll { search: alephgrid_core::SheetSearch, replacement: String },
    /// The comments of a sheet (S.11).
    Comments { sheet: u32 },
    /// Comment on a cell, or reply to a thread (S.11).
    AddComment { sheet: u32, row: u32, column: u32, text: String, #[serde(default)] #[ts(optional, as = "Option<String>")] parent: String },
    EditComment { sheet: u32, comment: String, text: String },
    ResolveComment { sheet: u32, comment: String, resolved: bool },
    DeleteComment { sheet: u32, comment: String },
    /// Borders on an area (S.01): kind all/outer/inner/top/bottom/left/right/none, weight thin/medium/thick.
    SheetBorder { sheet: u32, area: alephgrid_core::CellArea, kind: String, weight: String, #[serde(default)] #[ts(optional, as = "Option<u64>")] layout: Option<u64> },
    /// Sort the rows of a range by columns of it, keeping a header row (S.06).
    SheetSort { sheet: u32, area: alephgrid_core::CellArea, by: Vec<alephgrid_core::SortBy>, #[serde(default)] #[ts(optional, as = "Option<bool>")] header: bool, #[serde(default)] #[ts(optional, as = "Option<u64>")] layout: Option<u64> },
    /// The charts placed on a sheet (S.09).
    SheetCharts { sheet: u32 },
    /// Replace the charts placed on a sheet (S.09).
    SetSheetCharts { sheet: u32, charts: Vec<alephgrid_core::SheetChart> },
    /// A sheet's conditional colours and validation rules (S.10).
    SheetRules { sheet: u32 },
    SetSheetRules { sheet: u32, rules: alephgrid_core::SheetRules },
    /// Move a sheet's tab to where visible sheet `to` is (S.07).
    SheetReorder { sheet: u32, to: u32 },
    /// Hide a sheet's tab, list hidden sheets, show one again (S.07).
    SheetHide { sheet: u32 },
    HiddenSheets,
    SheetUnhide { name: String },
    /// Copy a sheet into a new one after it (S.07).
    SheetDuplicate { sheet: u32 },
    /// Colour a sheet's tab, `#RRGGBB` or empty (S.07).
    SheetColor { sheet: u32, color: String },
    /// Freeze the first columns of a sheet (S.05; 0 unfreezes), and the first `rows` rows
    /// when given (frozen rows stay otherwise).
    SheetFreeze { sheet: u32, columns: u32, #[serde(default)] #[ts(optional, as = "Option<u32>")] rows: Option<u32> },
    /// A sheet's frozen panes and the rows and columns that differ from the default (S.05).
    SheetLayout { sheet: u32 },
    /// Set the width or height (grid pixels) of `count` columns or rows from `start` (S.05).
    SheetResize { sheet: u32, axis: alephgrid_core::Axis, start: u32, count: u32, size: u32 },
    /// Hide or show `count` columns or rows from `start` (S.05).
    SheetSetHidden { sheet: u32, axis: alephgrid_core::Axis, start: u32, count: u32, hidden: bool },
    /// Merge an area (or unmerge the merges in it) (S.05).
    SheetMerge { sheet: u32, area: alephgrid_core::CellArea, mode: alephgrid_core::MergeMode },
    /// The longest shown texts of a column, for auto-fit (S.05).
    SheetColumnTexts { sheet: u32, column: u32 },
    /// The distinct shown values of a range's column below its header row (S.06).
    SheetRangeValues { sheet: u32, area: alephgrid_core::CellArea, header: bool, column: u32 },
    /// Show the data rows of a range whose `column` shows one of `keep`, hide the others;
    /// no `keep` shows them all again (S.06). Returns the number of rows hidden.
    SheetFilter { sheet: u32, area: alephgrid_core::CellArea, header: bool, column: u32, #[serde(default)] #[ts(optional, as = "Option<Vec<String>>")] keep: Option<Vec<String>> },
    /// Clear every format of an area; values stay.
    SheetClearFormat { sheet: u32, area: alephgrid_core::CellArea, #[serde(default)] #[ts(optional, as = "Option<u64>")] layout: Option<u64> },
    /// Paste `source` of `from_sheet` at (`row`, `column`) of `sheet`, translating formulas.
    SheetPaste {
        from_sheet: u32,
        source: alephgrid_core::CellArea,
        sheet: u32,
        row: u32,
        column: u32,
        #[serde(default = "default_paste_mode")] #[ts(optional, as = "Option<alephgrid_core::PasteMode>")] mode: alephgrid_core::PasteMode,
        #[serde(default)] #[ts(optional, as = "Option<bool>")] transpose: bool,
        /// Layouts (see `SheetInfo::layout`) of the target sheet and of `from_sheet`.
        #[serde(default)] #[ts(optional, as = "Option<u64>")] layout: Option<u64>,
        #[serde(default)] #[ts(optional, as = "Option<u64>")] source_layout: Option<u64>,
    },
    /// Cut and paste within one sheet; references to the moved cells follow them.
    SheetMove { sheet: u32, source: alephgrid_core::CellArea, row: u32, column: u32, #[serde(default)] #[ts(optional, as = "Option<u64>")] layout: Option<u64> },
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
    SheetToTable { sheet: u32, name: String, #[serde(default)] #[ts(optional, as = "Option<u64>")] layout: Option<u64> },
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

fn default_value_limit() -> usize {
    500
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
        | UpdateView { .. } | ViewInfo { .. } | CloseView { .. } | Fetch { .. } | Stats { .. } | ColumnValues { .. } | FindInView { .. } | SheetFind { .. } | SheetFindAll { .. } | Comments { .. } | HiddenSheets | SheetRules { .. } | SheetCharts { .. } | SheetLayout { .. } | SheetColumnTexts { .. } | SheetRangeValues { .. } | Sheets | SheetFetch { .. } | SheetNames
        | SheetCellDetail { .. } | SheetRegion { .. } | SheetCheckFormula { .. } | SavedViews { .. } | History { .. } | Schema | SavedQueries
        | QueryVersions { .. } | Materialized | Cancel { .. } => VIEW,
        TestFunction { definition } if definition.kind == alephgrid_core::FunctionKind::Sql => QUERY,
        // Running Python is publishing-level: it executes the code (in a worker).
        TestFunction { definition } if definition.kind == alephgrid_core::FunctionKind::Python => &[Cap::Publish],
        TestFunction { .. } => VIEW,
        Query { .. } => QUERY,
        ExportQuery { .. } => &[Cap::Query, Cap::Export],
        PublishFunction { .. } | PinFunctions { .. } => &[Cap::Publish],
        RefreshFunctions => &[Cap::Edit],
        CreateTableFromQuery { .. } | CreateSheetFromQuery { .. } | SqlChange { .. } | RefreshMaterialized { .. } => EDIT_QUERY,
        SaveAnalysis { .. } | DeleteAnalysis { .. } | SaveDashboard { .. } | DeleteDashboard { .. } | SetValidations { .. }
        | ApplyTransform { .. } | MaterializeAnalysis { .. } | ReplaceApply { .. } | ReplaceUndo { .. } | SavePresentation { .. }
        | Edit { .. } | InsertRows { .. } | DeleteRows { .. } | Undo { .. } | AddColumn { .. } | AddFormulaColumn { .. } | UpdateFormula { .. }
        | DropColumn { .. } | RenameColumn { .. } | CreateDemoTable { .. } | DropTable { .. } | RenameTable { .. } | SheetCreate { .. }
        | SheetRename { .. } | SheetDelete { .. } | SheetSet { .. } | SheetUndo | SheetRedo | SheetInsertRows { .. }
        | SheetDeleteRows { .. } | SheetInsertColumns { .. } | SheetDeleteColumns { .. } | SheetDefineName { .. }
        | SheetUpdateName { .. } | SheetDeleteName { .. } | SheetFill { .. } | SheetDragFill { .. } | SheetStyle { .. } | SheetFreeze { .. } | SheetResize { .. } | SheetSetHidden { .. } | SheetMerge { .. } | SheetFilter { .. } | AddComment { .. } | EditComment { .. } | ResolveComment { .. } | DeleteComment { .. } | SheetDuplicate { .. } | SheetHide { .. } | SheetReorder { .. } | SetSheetRules { .. } | SetSheetCharts { .. } | SheetUnhide { .. } | SheetSort { .. } | SheetBorder { .. } | SheetColor { .. } | SheetReplace { .. } | SheetReplaceAll { .. } | SheetClearFormat { .. } | SheetPaste { .. } | SheetMove { .. }
        | TableToSheet { .. } | SaveView { .. } | RenameSavedView { .. } | DeleteSavedView { .. } | TableFromView { .. }
        | SheetToTable { .. } | SaveQuery { .. } | RenameQuery { .. } | DeleteQuery { .. } | Recalculate { .. }
        | CancelRecalculation { .. } => EDIT,
    }
}

/// Where a request runs (see `sched.rs`): `(is_write, class)`; `None` is answered at once.
fn classify(op: &Op) -> Option<(bool, Class)> {
    use Op::*;
    match op {
        Analyses | Dashboards | Dashboard { .. } | Drill { .. } | XlsxCompatibility | Validations { .. } | PreviewTransform { .. } | InspectFormula { .. } | Presentation { .. } | Functions | Tables | ViewInfo { .. } | Fetch { .. } | Stats { .. } | Comments { .. } | HiddenSheets | SheetRules { .. } | SheetCharts { .. } | SheetLayout { .. } | SheetColumnTexts { .. } | SheetRangeValues { .. } | Sheets | SheetFetch { .. } | SheetNames | SheetCellDetail { .. } | SheetRegion { .. } | SheetCheckFormula { .. } | SavedViews { .. } | History { .. }
        | Schema | SavedQueries | QueryVersions { .. } | Materialized => Some((false, Class::Interactive)),
        Analyze { .. } | ColumnValues { .. } | FindInView { .. } | SheetFind { .. } | SheetFindAll { .. } | ReplacePreview { .. } | TestFunction { .. } | OpenView { .. } | UpdateView { .. } | Query { .. } | ExportQuery { .. } => Some((false, Class::Bulk)),
        CloseView { .. } | Cancel { .. } | CancelRecalculation { .. } | Recalculate { .. } => None,
        MaterializeAnalysis { .. } | ApplyTransform { .. } | ReplaceApply { .. } | ReplaceUndo { .. } | PublishFunction { .. } | PinFunctions { .. } | RefreshFunctions | CreateTableFromQuery { .. } | CreateDemoTable { .. } | CreateSheetFromQuery { .. } | TableToSheet { .. }
        | TableFromView { .. } | SheetToTable { .. } | SqlChange { .. } | RefreshMaterialized { .. } => Some((true, Class::Bulk)),
        _ => Some((true, Class::Interactive)),
    }
}

/// The open view an op names, if any. Views belong to the connection that opened them: one
/// session may not read, re-point or close another's view, nor write through it.
fn view_named(op: &Op) -> Option<u64> {
    match op {
        Op::UpdateView { view_id, .. } | Op::ViewInfo { view_id } | Op::CloseView { view_id } | Op::Fetch { view_id, .. }
        | Op::Stats { view_id, .. } | Op::FindInView { view_id, .. } => Some(*view_id),
        _ => view_of(op),
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
        Op::Undo { revision: Some(r), redo } => format!("{} revision {r}", if *redo { "redo" } else { "undo" }),
        Op::Undo { revision: None, redo } => (if *redo { "redo" } else { "undo" }).to_string(),
        Op::SheetSet { sheet, inputs, .. } => format!("sheet {sheet}: {} cell(s)", inputs.len()),
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
    /// Python tests of a definition being published, run before the writer was taken (8.07).
    tested: Option<alephgrid_core::TestedPython>,
    /// The workbook's database file and the size it may grow to (SEC.11).
    database: std::path::PathBuf,
    max_workbook_bytes: Option<u64>,
}

/// Demo tables a workbook may hold, and rows one may have, when made over the protocol
/// (SEC.11); `--demo-rows` on the command line is the operator's and not capped.
const MAX_DEMO_TABLES: usize = 3;
const MAX_PROTOCOL_DEMO_ROWS: u64 = 10_000_000;

/// Whether an operation only takes data away (allowed when the workbook is at its size limit).
fn only_removes(op: &Op) -> bool {
    use Op::*;
    matches!(
        op,
        DropTable { .. } | DropColumn { .. } | DeleteRows { .. } | SheetDelete { .. } | SheetDeleteRows { .. } | SheetDeleteColumns { .. }
            | DeleteSavedView { .. } | DeleteQuery { .. } | DeleteAnalysis { .. } | DeleteDashboard { .. } | Undo { .. } | SheetUndo
    )
}

/// Refuse an operation that may add data once the database is at its limit (SEC.11).
fn check_size(op: &Op, ctx: &Ctx) -> Result<()> {
    let Some(limit) = ctx.max_workbook_bytes else { return Ok(()) };
    if only_removes(op) {
        return Ok(());
    }
    let size = crate::state::database_bytes(ctx.database.parent().expect("the database lives in its workbook's directory"));
    if size >= limit {
        bail!(alephgrid_core::limits::TooLarge(format!(
            "this workbook holds {} MiB, its limit is {} MiB; delete tables or rows first, or ask an admin to raise the limit",
            size >> 20,
            limit >> 20
        )));
    }
    Ok(())
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
        Op::Comments { sheet } => json!(r.comments(sheet)?),
        Op::HiddenSheets => json!(r.sheets().hidden_sheets()),
        Op::SheetRules { sheet } => json!(r.sheet_rules(sheet)?),
        Op::SheetCharts { sheet } => json!(r.sheet_charts(sheet)?),
        Op::SheetLayout { sheet } => json!(r.sheets().layout(sheet)?),
        Op::SheetColumnTexts { sheet, column } => json!(r.sheets().column_texts(sheet, column)?),
        Op::SheetRangeValues { sheet, area, header, column } => json!(r.sheets().range_values(sheet, &area, header, column)?),
        Op::SheetFind { sheet, search, from, backwards, count } => json!(r.sheets().find(sheet, &search, from, backwards, count)?),
        Op::SheetFindAll { search } => json!(r.sheets().find_all_sheets(&search)?),
        Op::FindInView { view_id, text, match_case, from, backwards, count } => json!(r.find_in_view(view_id, &text, match_case, from, backwards, count)?),
        Op::ColumnValues { spec, column, search, limit } => json!(r.column_values(&spec, &column, &search, limit)?),
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
        Op::SheetRegion { sheet, row, column } => json!(r.sheets().current_region(sheet, row, column)?),
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
    check_size(&op, ctx)?;
    Ok(match op {
        Op::SaveAnalysis { saved, name, spec, chart, chart_kind } => (json!(wb.save_analysis_as(saved.as_deref(), &name, spec, chart, chart_kind)?),Some(Event::Resync { revision: None })),
        Op::SetValidations { table, rules } => (json!(wb.set_validations(&table, rules)?), Some(Event::TableChanged { table, origin, revision: 0 })),
        Op::ApplyTransform { table, transform, name } => (json!(wb.apply_transform(&table, &transform, &name)?), Some(Event::TablesChanged { origin, revision: 0 })),
        Op::SaveDashboard { saved, name, tiles, slicers } => (json!(wb.save_dashboard(saved.as_deref(), &name, tiles, slicers)?), Some(Event::Resync { revision: None })),
        Op::DeleteDashboard { dashboard } => { wb.delete_dashboard(&dashboard)?; (Value::Null, Some(Event::Resync { revision: None })) },
        Op::DeleteAnalysis { analysis } => { wb.delete_analysis(&analysis)?; (Value::Null,Some(Event::Resync { revision: None })) },
        Op::MaterializeAnalysis { name, spec } => (json!(wb.materialize_analysis(&name,&spec)?),tables()),
        Op::ReplaceApply { spec, revision } => {let out=wb.replace_apply(&spec,revision)?;let event=changed(out.table.clone());(json!(out),event)},
        Op::ReplaceUndo { revision } => {let out=wb.replace_undo(revision)?;let event=changed(out.table.clone());(json!(out),event)},
        Op::SavePresentation { table, presentation } => (json!(wb.save_presentation(&table,presentation)?),Some(Event::Resync { revision: None })),
        Op::PublishFunction { definition } => {
            if definition.kind == alephgrid_core::FunctionKind::Sql { require_sql(ctx)?; }
            (json!(wb.publish_function_tested(definition, ctx.tested.clone())?), Some(Event::Resync { revision: None }))
        }
        Op::RefreshFunctions => (json!(wb.refresh_functions()?), Some(Event::Resync { revision: None })),
        Op::PinFunctions { pins } => {
            if wb.functions()?.iter().any(|f| pins.contains_key(&f.name) && f.versions.iter().any(|v| v.definition.kind == alephgrid_core::FunctionKind::Sql)) { require_sql(ctx)?; }
            (json!(wb.pin_functions(&pins)?), Some(Event::Resync { revision: None }))
        }
        Op::Edit { view_id, edits, generation } => {
            if let Some(g) = generation.filter(|_| edits.iter().any(|e| e.record.is_none())) {
                wb.expect_generation(view_id, g)?;
            }
            let out = wb.edit(view_id, &edits)?;
            let event = if out.rows_appended > 0 {
                Event::TableChanged { table: out.table.clone(), origin, revision: 0 }
            } else {
                Event::CellsChanged { table: out.table.clone(), origin, revision: 0 }
            };
            (json!(out), Some(event))
        }
        Op::InsertRows { view_id, at, count, generation } => {
            if let Some(g) = generation {
                wb.expect_generation(view_id, g)?;
            }
            (Value::Null, changed(wb.insert_rows(view_id, at, count)?))
        }
        Op::DeleteRows { view_id, start, count, generation } => {
            if let Some(g) = generation {
                wb.expect_generation(view_id, g)?;
            }
            (Value::Null, changed(wb.delete_rows(view_id, start, count)?))
        }
        Op::Undo { revision, redo } => {
            let out = wb.undo_change(revision, redo)?;
            let event = out.as_ref().map(|o| Event::TableChanged { table: o.table.clone(), origin, revision: 0 });
            (json!(out), event)
        }
        Op::AddColumn { table, name, sql_type } => {
            wb.add_column(&table, &name, &sql_type)?;
            (Value::Null, changed(table))
        }
        Op::AddFormulaColumn { table, name, formula, language } => {
            // A SQL formula is a DuckDB expression: SQL, so gated with it (SEC.10).
            if language == alephgrid_core::FormulaLanguage::Sql {
                require_sql(ctx)?;
            }
            wb.add_formula_column_as(&table, &name, &formula, language)?;
            (Value::Null, changed(table))
        }
        Op::UpdateFormula { table, name, formula } => {
            if wb.formula_language(&table, &name)? == alephgrid_core::FormulaLanguage::Sql {
                require_sql(ctx)?;
            }
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
        Op::CreateDemoTable { rows } => {
            let demos = wb.list_tables()?.iter().filter(|t| t.name.starts_with("flights")).count();
            if demos >= MAX_DEMO_TABLES {
                bail!(alephgrid_core::limits::TooLarge(format!("a workbook holds at most {MAX_DEMO_TABLES} demo tables; drop one first")));
            }
            (json!(wb.create_demo_table(rows.min(MAX_PROTOCOL_DEMO_ROWS))?), tables())
        }
        Op::DropTable { table } => {
            wb.drop_table(&table)?;
            (Value::Null, tables())
        }
        Op::RenameTable { table, new_name } => (json!(wb.rename_table(&table, &new_name)?), tables()),
        Op::SheetCreate { name } => {
            let name = if name.trim().is_empty() { "Sheet".to_string() } else { name };
            (json!(wb.with_sheets(|s| s.create(&name))?), tables())
        }
        Op::SheetRename { sheet, name, layout } => {
            expect_layout(wb, sheet, layout)?;
            wb.with_sheets(|s| s.rename(sheet, &name))?;
            (Value::Null, tables())
        }
        Op::SheetDelete { sheet, layout } => {
            expect_layout(wb, sheet, layout)?;
            wb.delete_sheet(sheet)?;
            (Value::Null, tables())
        }
        Op::SheetSet { sheet, inputs, layout } => {
            expect_layout(wb, sheet, layout)?;
            wb.check_sheet_inputs(sheet, &inputs)?;
            wb.set_sheet_cells(sheet, &inputs)?;
            (json!(wb.sheets().info(sheet)?), sheet_changed(sheet))
        }
        Op::SheetUndo | Op::SheetRedo => {
            let before: Vec<String> = wb.sheets().list().into_iter().map(|s| s.name).collect();
            let undo = matches!(op, Op::SheetUndo);
            let step = { let s = wb.sheets(); if undo { s.undo_step() } else { s.redo_step() } };
            let changed = wb.with_sheets(|s| if undo { s.undo() } else { s.redo() })?;
            // Comments moved by the step move back (or again) with it (S.11).
            let note = step.filter(|_| changed).and_then(|id| wb.sheets().step_note(id).cloned());
            if let Some(note) = note {
                wb.restore_comment_step(&note, undo)?;
            }
            let after: Vec<String> = wb.sheets().list().into_iter().map(|s| s.name).collect();
            // Undoing a sheet creation, rename or deletion changes the tab list too.
            let event = if before != after {
                Event::TablesChanged { origin, revision: 0 }
            } else {
                Event::SheetChanged { sheet: None, origin, revision: 0 }
            };
            (json!(changed), changed.then_some(event))
        }
        Op::SheetInsertRows { sheet, at, count, layout } => {
            expect_layout(wb, sheet, layout)?;
            let before = wb.comments(sheet)?;
            wb.with_sheets(|s| s.insert_rows(sheet, at, count))?;
            wb.shift_comments(sheet, alephgrid_core::comments::Axis::Rows, at, count as i64)?;
            wb.note_comment_step(sheet, before)?;
            (Value::Null, sheet_changed(sheet))
        }
        Op::SheetDeleteRows { sheet, at, count, layout } => {
            expect_layout(wb, sheet, layout)?;
            let before = wb.comments(sheet)?;
            wb.with_sheets(|s| s.delete_rows(sheet, at, count))?;
            wb.shift_comments(sheet, alephgrid_core::comments::Axis::Rows, at, -(count as i64))?;
            wb.note_comment_step(sheet, before)?;
            (Value::Null, sheet_changed(sheet))
        }
        Op::SheetInsertColumns { sheet, at, count, layout } => {
            expect_layout(wb, sheet, layout)?;
            let before = wb.comments(sheet)?;
            wb.with_sheets(|s| s.insert_columns(sheet, at, count))?;
            wb.shift_comments(sheet, alephgrid_core::comments::Axis::Columns, at, count as i64)?;
            wb.note_comment_step(sheet, before)?;
            (Value::Null, sheet_changed(sheet))
        }
        Op::SheetDeleteColumns { sheet, at, count, layout } => {
            expect_layout(wb, sheet, layout)?;
            let before = wb.comments(sheet)?;
            wb.with_sheets(|s| s.delete_columns(sheet, at, count))?;
            wb.shift_comments(sheet, alephgrid_core::comments::Axis::Columns, at, -(count as i64))?;
            wb.note_comment_step(sheet, before)?;
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
        Op::AddComment { sheet, row, column, text, parent } => (json!(wb.add_comment(sheet, row, column, &text, Some(parent.as_str()).filter(|p| !p.is_empty()))?), sheet_changed(sheet)),
        Op::EditComment { sheet, comment, text } => { wb.edit_comment(&comment, &text)?; (Value::Null, sheet_changed(sheet)) }
        Op::ResolveComment { sheet, comment, resolved } => { wb.resolve_comment(&comment, resolved)?; (Value::Null, sheet_changed(sheet)) }
        Op::DeleteComment { sheet, comment } => { wb.delete_comment(&comment)?; (Value::Null, sheet_changed(sheet)) }
        Op::SheetBorder { sheet, area, kind, weight, layout } => {
            expect_layout(wb, sheet, layout)?;
            wb.with_sheets(|s| s.set_border(sheet, area, &kind, &weight))?;
            (json!(wb.sheets().info(sheet)?), sheet_changed(sheet))
        }
        Op::SheetSort { sheet, area, by, header, layout } => {
            expect_layout(wb, sheet, layout)?;
            wb.with_sheets(|s| s.sort_range(sheet, area, &by, header))?;
            (json!(wb.sheets().info(sheet)?), sheet_changed(sheet))
        }
        Op::SheetResize { sheet, axis, start, count, size } => {
            wb.with_sheets(|s| s.resize(sheet, axis, start, count, size))?;
            (Value::Null, sheet_changed(sheet))
        }
        Op::SheetSetHidden { sheet, axis, start, count, hidden } => {
            wb.with_sheets(|s| s.set_hidden(sheet, axis, start, count, hidden))?;
            (Value::Null, sheet_changed(sheet))
        }
        Op::SheetMerge { sheet, area, mode } => {
            wb.with_sheets(|s| s.merge(sheet, &area, mode))?;
            (Value::Null, sheet_changed(sheet))
        }
        Op::SheetFilter { sheet, area, header, column, keep } => {
            let n = wb.with_sheets(|s| s.filter_range(sheet, &area, header, column, keep.as_deref()))?;
            (json!(n), sheet_changed(sheet))
        }
        Op::SetSheetCharts { sheet, charts } => {
            wb.set_sheet_charts(sheet, charts)?;
            (Value::Null, sheet_changed(sheet))
        }
        Op::SetSheetRules { sheet, rules } => {
            wb.set_sheet_rules(sheet, rules)?;
            (Value::Null, sheet_changed(sheet))
        }
        Op::SheetReorder { sheet, to } => {
            wb.with_sheets(|s| s.move_tab(sheet, to))?;
            (Value::Null, Some(Event::Resync { revision: None }))
        }
        Op::SheetHide { sheet } => {
            wb.with_sheets(|s| s.hide_sheet(sheet))?;
            (Value::Null, Some(Event::Resync { revision: None }))
        }
        Op::SheetUnhide { name } => {
            wb.with_sheets(|s| s.unhide_sheet(&name))?;
            (Value::Null, Some(Event::Resync { revision: None }))
        }
        Op::SheetDuplicate { sheet } => {
            let info = wb.with_sheets(|s| s.duplicate(sheet))?;
            (json!(info), Some(Event::Resync { revision: None }))
        }
        Op::SheetColor { sheet, color } => {
            wb.with_sheets(|s| s.set_tab_color(sheet, &color))?;
            (json!(wb.sheets().info(sheet)?), sheet_changed(sheet))
        }
        Op::SheetFreeze { sheet, columns, rows } => {
            wb.with_sheets(|s| match rows {
                Some(rows) => s.freeze(sheet, rows, columns),
                None => s.freeze_columns(sheet, columns),
            })?;
            (json!(wb.sheets().info(sheet)?), sheet_changed(sheet))
        }
        Op::SheetReplaceAll { search, replacement } => {
            let n = wb.with_sheets(|s| s.replace_all_sheets(&search, &replacement))?;
            (json!(n), Some(Event::SheetChanged { sheet: None, origin, revision: 0 }))
        }
        Op::SheetReplace { sheet, search, replacement } => {
            let n = wb.with_sheets(|s| s.replace_all(sheet, &search, &replacement))?;
            (json!(n), sheet_changed(sheet))
        }
        Op::SheetStyle { sheet, area, style, layout } => {
            expect_layout(wb, sheet, layout)?;
            wb.with_sheets(|s| s.set_style(sheet, area, &style))?;
            (json!(wb.sheets().info(sheet)?), sheet_changed(sheet))
        }
        Op::SheetClearFormat { sheet, area, layout } => {
            expect_layout(wb, sheet, layout)?;
            wb.with_sheets(|s| s.clear_format(sheet, area))?;
            (json!(wb.sheets().info(sheet)?), sheet_changed(sheet))
        }
        Op::SheetFill { sheet, area, direction, layout } => {
            expect_layout(wb, sheet, layout)?;
            wb.with_sheets(|s| s.fill(sheet, area, direction))?;
            (json!(wb.sheets().info(sheet)?), sheet_changed(sheet))
        }
        Op::SheetDragFill { sheet, source, target, layout } => {
            expect_layout(wb, sheet, layout)?;
            wb.with_sheets(|s| s.drag_fill(sheet, source, target))?;
            (json!(wb.sheets().info(sheet)?), sheet_changed(sheet))
        }
        Op::SheetPaste { from_sheet, source, sheet, row, column, mode, transpose, layout, source_layout } => {
            expect_layout(wb, sheet, layout)?;
            expect_layout(wb, from_sheet, source_layout)?;
            let area = wb.with_sheets(|s| s.paste(from_sheet, source, sheet, (row, column), mode, transpose))?;
            (json!(area), sheet_changed(sheet))
        }
        Op::SheetMove { sheet, source, row, column, layout } => {
            expect_layout(wb, sheet, layout)?;
            let before = wb.comments(sheet)?;
            let area = wb.with_sheets(|s| s.move_cells(sheet, source, (row, column)))?;
            wb.move_comments(sheet, source, (row, column))?;
            wb.note_comment_step(sheet, before)?;
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
        Op::SheetToTable { sheet, name, layout } => {
            expect_layout(wb, sheet, layout)?;
            (json!(wb.sheet_to_table(sheet, &name)?), tables())
        }
        other => bail!("{} is not a change", op_name(&other)),
    })
}

/// Refuse a sheet change computed against another layout of `sheet` (see `SheetInfo::layout`).
fn expect_layout(wb: &Workbook, sheet: u32, layout: Option<u64>) -> Result<()> {
    match layout {
        Some(l) => wb.sheets().expect_layout(sheet, l),
        None => Ok(()),
    }
}

/// Broadcast what a committed change did: its own event (or [`Event::Committed`]), then
/// tables and sheet cells that changed through formulas reading it. Returns whether the
/// change restructured something (for the home-screen summary).
fn announce(
    events: &tokio::sync::broadcast::Sender<Event>,
    c: &alephgrid_core::Committed,
    event: Option<Event>,
    derived: Vec<(String, bool)>,
    sheets: bool,
    origin: u64,
) -> bool {
    let Some(ev) = event else {
        let _ = events.send(Event::Committed { origin, revision: c.revision });
        return false;
    };
    let ev = with_revision(ev, c.revision);
    let structural = ev.changes_structure();
    let primary = match &ev {
        Event::CellsChanged { table, .. } | Event::TableChanged { table, .. } => Some(table.clone()),
        _ => None,
    };
    let resync = matches!(ev, Event::Resync { .. });
    let _ = events.send(ev);
    // Origin 0: no session has applied these changes itself.
    for (table, restructured) in derived.into_iter().filter(|(t, r)| !resync && (*r || primary.as_ref() != Some(t))) {
        let ev = if restructured || structural {
            Event::TableChanged { table, origin: 0, revision: c.revision }
        } else {
            Event::CellsChanged { table, origin: 0, revision: c.revision }
        };
        let _ = events.send(ev);
    }
    // Sheet cells that read the changed tables have new values.
    if sheets && !resync {
        let _ = events.send(Event::SheetChanged { sheet: None, origin: 0, revision: c.revision });
    }
    structural
}

fn with_revision(ev: Event, rev: u64) -> Event {
    match ev {
        Event::CellsChanged { table, origin, .. } => Event::CellsChanged { table, origin, revision: rev },
        Event::TableChanged { table, origin, .. } => Event::TableChanged { table, origin, revision: rev },
        Event::TablesChanged { origin, .. } => Event::TablesChanged { origin, revision: rev },
        Event::SheetChanged { sheet, origin, .. } => Event::SheetChanged { sheet, origin, revision: rev },
        Event::ViewsChanged { table, origin, .. } => Event::ViewsChanged { table, origin, revision: rev },
        Event::QueriesChanged { origin, .. } => Event::QueriesChanged { origin, revision: rev },
        Event::Resync { .. } => Event::Resync { revision: Some(rev) },
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
        if c.downcast_ref::<alephgrid_core::Conflict>().is_some() {
            return Some("conflict");
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
                crate::runtime::FailureKind::Quota => "quota",
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
    /// The latest revision this session was told about (or would have been, had its events
    /// not been dropped).
    latest: std::sync::atomic::AtomicU64,
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
            latest: std::sync::atomic::AtomicU64::new(0),
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
        // Once events are dropped, later ones are too until the resync goes out: the
        // client refreshes everything then.
        if !reply && (now + len > self.pause || self.lagged.load(Ordering::SeqCst)) {
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
            let latest = self.latest.load(Ordering::SeqCst);
            self.push(serde_json::to_string(&Event::Resync { revision: Some(latest) }).unwrap(), true);
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

/// Serve one connection. `lease` keeps the allocation from closing while the session sets
/// up; after that, only its operations count as activity (7.16).
pub async fn serve_socket(socket: WebSocket, state: Shared, lease: crate::state::Lease, principal: Principal, role: SpaceRole, space: crate::auth::Space) {
    let handle = lease.handle();
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
    outbox.latest.fetch_max(revision, Ordering::SeqCst);
    let hello = json!({ "event": "hello", "session": session, "workbook": handle.meta.lock().unwrap().clone(), "revision": revision, "role": role });
    outbox.push(hello.to_string(), true);
    drop(lease);

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
                        if let Some(r) = ev.revision() {
                            outbox.latest.fetch_max(r, Ordering::SeqCst);
                        }
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
    // Set when the session ends (the workbook closed, or the client left): work withdrawn
    // then was not cancelled by the client and must be sent again, so it answers
    // `unavailable`, never `cancelled`.
    let ending = Arc::new(AtomicBool::new(false));
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
        // Each operation is meaningful activity and keeps the allocation open while it runs.
        let op_lease = crate::state::Lease::new(handle.clone());
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
        if let Some(v) = view_named(&envelope.op).filter(|v| !views.lock().unwrap().contains(v)) {
            outbox.push(error_reply(id, &anyhow::anyhow!("unknown view {v}"), &state.config.data_dir).to_string(), true);
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
            tested: None,
            database: handle.dir.join("data.duckdb"),
            max_workbook_bytes: state.config.max_workbook_bytes,
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
        let ending = ending.clone();
        let withdrawn = move |e: anyhow::Error| -> anyhow::Error {
            let cancelled = e.chain().any(|c| matches!(c.downcast_ref::<JobError>(), Some(JobError::Cancelled)));
            if cancelled && ending.load(Ordering::SeqCst) {
                anyhow::anyhow!("this workbook was closed on the server before this ran; reconnect and send it again")
            } else {
                e
            }
        };
        jobs.spawn(async move {
            let _slot = slot;
            let _lease = op_lease;
            let mut ctx = ctx;
            // A Python definition's tests run now, in a worker, not under the writer.
            if let Op::PublishFunction { definition } = &op {
                if let (alephgrid_core::FunctionKind::Python, Some(rt)) = (&definition.kind, handle.functions.runtime_impl()) {
                    let d = definition.clone();
                    let tested = tokio::task::spawn_blocking(move || alephgrid_core::functions::test_python(&d, rt.as_ref(), &AtomicBool::new(false))).await;
                    ctx.tested = tested.ok().and_then(|t| t.ok());
                }
            }
            let reply = if is_write {
                let name = op_name(&op);
                let audit_op = name.clone();
                let summary = summary(&op);
                let audit_summary = summary.clone();
                let view = view_of(&op);
                let (events, origin) = (handle.events.clone(), session);
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
                    // Announced here, on the writer, so events go out in revision order.
                    let structural = if committed.replayed { None } else { Some(announce(&events, &committed, event, derived, sheets, origin)) };
                    Ok((committed, structural, queued))
                }));
                match handle.run_queued(ticket, job, Some(cancel_rx), progress, work).await {
                    Ok((c, structural, queued)) => {
                        if !c.replayed {
                            audit(&audit_state, &principal, &space, &handle.id, (&audit_op, audit_summary), &c);
                            if class == Class::Bulk {
                                audit_state.quota.forget(&space.id);
                            }
                        }
                        handle.recalculate_later(queued, c.revision, recalc_timeout);
                        // New sheet cells or columns may call Python functions not computed yet.
                        handle.compute_functions_later();
                        if let Some(structural) = structural {
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
                    Err(e) => {
                        // A write that ran out of disk leaves its blocks in the file: give
                        // the disk reserve back so the server can still record and restart.
                        audit_state.controller.disk_reserve.note(&e);
                        error_reply(id, &withdrawn(e), &data_dir)
                    }
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
                        error_reply(id, &withdrawn(anyhow::Error::new(JobError::Cancelled)), &data_dir)
                    }
                    Ok(v) => {
                        if exporting {
                            let detail = json!({ "kind": "query", "rows": v["rows"], "bytes": v["bytes"], "revision": v["revision"] });
                            audit_state.auth.record(&principal, "data_exported", &space, &handle.id, detail);
                        }
                        json!({ "id": id, "ok": true, "result": v })
                    }
                    Err(e) => {
                        audit_state.controller.disk_reserve.note(&e);
                        error_reply(id, &withdrawn(e), &data_dir)
                    }
                }
            };
            cancels.lock().unwrap().remove(&id);
            outbox.push(reply.to_string(), true);
        });
    }

    // Queued work of this session is withdrawn and running work interrupted. A change
    // that already committed stays committed (a retry with its mutation id is not applied
    // twice).
    ending.store(true, Ordering::SeqCst);
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
    // What the change addressed moved: there is nothing to overwrite, only to refresh.
    if e.chain().any(|c| c.downcast_ref::<alephgrid_core::Conflict>().is_some_and(|c| c.moved)) {
        r["moved"] = json!(true);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(revision: u64) -> String {
        serde_json::to_string(&Event::CellsChanged { table: "t".into(), origin: 1, revision }).unwrap()
    }

    /// Once a slow client's events are dropped, later ones are dropped too until it has
    /// caught up; then one resync names the latest revision it was not told about.
    #[test]
    fn dropped_events_end_in_one_resync_with_the_latest_revision() {
        let frame = event(1).len();
        let outbox = Outbox::new(frame * 3, frame * 100);
        for r in 1..=3 {
            outbox.latest.fetch_max(r, Ordering::SeqCst);
            outbox.push(event(r), false);
        }
        // Over the pause threshold: 4 is dropped, and 5 too although there would be room.
        outbox.latest.fetch_max(4, Ordering::SeqCst);
        outbox.push(event(4), false);
        assert!(outbox.lagged.load(Ordering::SeqCst));
        let first = outbox.pop().unwrap();
        outbox.sent(first.len());
        outbox.latest.fetch_max(5, Ordering::SeqCst);
        outbox.push(event(5), false);
        // Replies are never dropped.
        outbox.push("{\"id\":9,\"ok\":true}".into(), true);
        let mut out = vec![first];
        while let Some(f) = outbox.pop() {
            outbox.sent(f.len());
            out.push(f);
        }
        let revisions: Vec<Value> = out.iter().map(|f| serde_json::from_str::<Value>(f).unwrap()).collect();
        assert_eq!(
            revisions,
            vec![
                json!({ "event": "cells_changed", "table": "t", "origin": 1, "revision": 1 }),
                json!({ "event": "cells_changed", "table": "t", "origin": 1, "revision": 2 }),
                json!({ "event": "cells_changed", "table": "t", "origin": 1, "revision": 3 }),
                json!({ "id": 9, "ok": true }),
                json!({ "event": "resync", "revision": 5 }),
            ]
        );
        assert!(!outbox.lagged.load(Ordering::SeqCst));
    }
}
