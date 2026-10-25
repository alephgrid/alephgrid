//! Rowfinity engine: a spreadsheet-shaped API over DuckDB.
//!
//! * Tables are stored columnar in a single DuckDB file per workbook, so opening a
//!   workbook is instant (nothing is loaded into memory) and data never leaves your disk.
//! * Clients read through [`Workbook::open_view`] + [`Workbook::fetch`], asking only for
//!   the rows on screen.
//! * Formula columns are lazily evaluated DuckDB expressions.
//! * Free-form Excel-style sheets run on IronCalc and live in the same file.

pub mod model;
pub mod sheets;
pub mod sql;
mod workbook;

pub use model::*;
pub use sheets::{SheetBlock, SheetInfo, SheetInput};
pub use workbook::{Imported, Workbook};

#[cfg(test)]
mod tests;
