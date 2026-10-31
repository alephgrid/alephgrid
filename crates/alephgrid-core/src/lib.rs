//! Alephgrid engine: a spreadsheet-shaped API over DuckDB.
//!
//! * Tables are stored columnar in a single DuckDB file per workbook, so opening a
//!   workbook is instant (nothing is loaded into memory) and data never leaves your disk.
//! * Clients read through [`Workbook::open_view`] + [`Workbook::fetch`], asking only for
//!   the rows on screen.
//! * Formula columns are lazily evaluated DuckDB expressions.
//! * Free-form Excel-style sheets run on IronCalc and live in the same file.

pub mod analysis;
pub use analysis::*;
pub mod catalog;
pub mod faults;
mod formula;
pub mod functions;
pub use functions::{FunctionDefinition, FunctionKind, FunctionTest, FunctionTestResult, FunctionVersion, WorkbookFunction};
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
pub use sheets::{Align, CellArea, CellDetail, CellError, FillDirection, FormulaProblem, NamedRange, PasteMode, SheetBlock, SheetInfo, SheetInput};
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
