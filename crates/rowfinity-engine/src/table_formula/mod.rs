//! Spreadsheet-syntax formulas over DuckDB tables (phase F.05). See docs/TABLE_FORMULAS.md.

mod compile;
mod parse;

pub use compile::{column_type, compile, compile_with_errors, referenced_columns, rename_column, Compiled, Ty};
pub use parse::{parse, ErrorCode, FormulaError, Span};

#[cfg(test)]
mod tests;
