//! Spreadsheet-syntax formulas over DuckDB tables (phase F.05). See docs/TABLE_FORMULAS.md.

mod compile;
mod parse;

pub use compile::{column_type, compile, compile_full, compile_with_errors, qualify_cells, referenced_columns, references, rename_column, rename_table, rename_table_column, rewrite_cells, unqualify_cells, valid_table_name, CellValue, Compiled, Reference, Relation, Relations, Stage, Ty};
pub use parse::{column_letters, parse, sheet_name_text, CellRef, ErrorCode, Expr, ExprKind, FormulaError, Span};

#[cfg(test)]
mod tests;
