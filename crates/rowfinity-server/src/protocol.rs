//! TypeScript bindings for everything the server sends or accepts. `web/src/protocol.ts`
//! is generated from these Rust types; `tests/protocol.rs` fails when it is out of date
//! (regenerate with `UPDATE_BINDINGS=1 cargo test -p rowfinity-server --test protocol`).

use rowfinity_engine::{
    Align, CellEdit, Column, ColumnType, EditOutcome, ExportFormat, FilePreview, Filter, FilterOp, ImportProblem, Imported,
    Kind, PreviewColumn, QueryPreview, RowBlock, SheetBlock, SheetCopy, SheetInfo, SheetInput, SortKey, Stats, TableInfo,
    UploadPreview, ViewInfo, ViewSpec,
};
use ts_rs::{Config, TS};

use crate::http::{ImportRequest, JobStatus, UploadCreated};
use crate::state::{Summary, WorkbookListing, WorkbookMeta, WorkbookPatch, Workspace};
use crate::ws::{Event, Op};

const HEADER: &str = "\
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
export type Hello = { event: \"hello\"; session: number; workbook: WorkbookMeta };
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
    ("tables", "TableInfo[]"),
    ("open_view", "ViewInfo"),
    ("update_view", "ViewInfo"),
    ("view_info", "ViewInfo"),
    ("close_view", "null"),
    ("fetch", "RowBlock"),
    ("edit", "EditOutcome"),
    ("insert_rows", "null"),
    ("delete_rows", "null"),
    ("stats", "Stats"),
    ("add_column", "null"),
    ("add_formula_column", "null"),
    ("update_formula", "null"),
    ("drop_column", "null"),
    ("rename_column", "null"),
    ("query", "QueryPreview"),
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
    ("create_sheet_from_query", "SheetCopy"),
    ("table_to_sheet", "SheetCopy"),
    ("sheet_to_table", "TableInfo"),
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
        Kind, Column, TableInfo, SortKey, FilterOp, Filter, ViewSpec, ViewInfo, RowBlock, CellEdit, EditOutcome,
        Stats, QueryPreview, ExportFormat, Align, SheetInfo, SheetBlock, SheetInput, SheetCopy, Imported,
        Summary, WorkbookMeta, WorkbookListing, WorkbookPatch, Workspace, Event, Op,
        ColumnType, PreviewColumn, FilePreview, UploadPreview, ImportProblem, UploadCreated, ImportRequest, JobStatus,
    );
    out.push_str("\n/** Result type of each operation. */\nexport interface Results {\n");
    for (op, ty) in RESULTS {
        out.push_str(&format!("  {op}: {ty};\n"));
    }
    out.push_str("}\n\nexport type OpName = Op[\"op\"];\n\n/** Parameters of an operation (its request without `op`). */\nexport type Params<O extends OpName> = Omit<Extract<Op, { op: O }>, \"op\">;\n");
    out
}
