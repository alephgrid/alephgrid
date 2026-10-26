//! Rowfinity engine: a spreadsheet-shaped API over DuckDB.
//!
//! * Tables are stored columnar in a single DuckDB file per workbook, so opening a
//!   workbook is instant (nothing is loaded into memory) and data never leaves your disk.
//! * Clients read through [`Workbook::open_view`] + [`Workbook::fetch`], asking only for
//!   the rows on screen.
//! * Formula columns are lazily evaluated DuckDB expressions.
//! * Free-form Excel-style sheets run on IronCalc and live in the same file.

pub mod faults;
mod formula;
pub mod import;
pub mod model;
pub mod sheets;
pub mod sql;
pub mod wire;
mod workbook;

pub use model::*;
pub use sheets::{Align, SheetBlock, SheetInfo, SheetInput};
pub use import::{ColumnType, FilePreview, ImportProblem, PreviewColumn};
pub use workbook::{Imported, UploadPreview, Workbook};

/// DuckDB handles the server uses for progress and cancellation.
pub mod duckdb_handles {
    pub use duckdb::progress::{ProgressHandle, QueryProgress};
    pub use duckdb::InterruptHandle;
}

#[cfg(test)]
mod tests;
