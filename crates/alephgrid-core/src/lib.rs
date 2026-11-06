//! Alephgrid engine: a spreadsheet-shaped API over DuckDB.
//!
//! * Tables are stored columnar in a single DuckDB file per workbook, so opening a
//!   workbook is instant (nothing is loaded into memory) and data never leaves your disk.
//! * Clients read through [`Workbook::open_view`] + [`Workbook::fetch`], asking only for
//!   the rows on screen.
//! * Formula columns are lazily evaluated DuckDB expressions.
//! * Free-form Excel-style sheets run on IronCalc and live in the same file.

pub mod comments;
pub mod sheetrules;
pub mod sheetcharts;
pub mod analysis;
pub use analysis::*;
pub mod catalog;
pub mod faults;
mod formula;
pub mod external;
pub mod functions;
pub use functions::{FunctionDefinition, FunctionDependent, FunctionKind, FunctionTest, FunctionTestResult, FunctionUsage, FunctionUseCell, FunctionUseColumn, FunctionVersion, StoredResults, TestValue, TestValueKind, TestedPython, TypedTestValue, WorkbookFunction};
pub mod import;
pub mod limits;
mod links;
pub mod model;
mod queries;
mod reads;
pub mod results;
pub mod reports;
pub mod tools;
pub mod undo;
pub mod sheets;
pub mod sql;
mod sqlchange;
mod sqllex;
pub mod sqlpolicy;
pub mod storage;
pub mod table_formula;
pub mod wire;
mod workbook;

pub use model::*;
pub use comments::CellComment;
pub use sheetrules::{SheetColorRule, SheetRules, SheetValidation};
pub use sheetcharts::SheetChart;
pub use sheets::{SheetMatches, Axis, LineSize, MergeMode, SheetLayout, SortBy, SheetSearch, CellStyle, StyledCell, StylePatch, Align, CellArea, CellDetail, CellError, FillDirection, FormulaProblem, NamedRange, PastedCell, PasteMode, SheetBlock, SheetInfo, SheetInput};
pub use import::{ColumnType, FilePreview, ImportProblem, PreviewColumn};
pub use queries::SchemaChanged;
pub use results::{ResultsActivity, RESULTS_MIN_ROWS};
pub use sqlpolicy::{BadParameter, IsChange, NotAllowed, ParamType, QueryParam};
pub use storage::{HistoryEntry, OpenReport, TooNew, WrongWorkbook};
pub use undo::{UndoKind, UndoOutcome, UndoState};
pub use workbook::{Committed, Imported, Mutation, Reader, SheetCopy, UploadPreview, Workbook};

/// DuckDB handles the server uses for progress and cancellation.
pub mod duckdb_handles {
    pub use duckdb::progress::{ProgressHandle, QueryProgress};
    pub use duckdb::InterruptHandle;
}

#[cfg(test)]
mod tests;

/// The DuckDB library version this build links (for receipts and diagnostics).
pub fn duckdb_version() -> String {
    static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        duckdb::Connection::open_in_memory()
            .and_then(|c| c.query_row("SELECT version()", [], |r| r.get::<_, String>(0)))
            .unwrap_or_else(|_| "unknown".into())
    })
    .clone()
}
