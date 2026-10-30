//! Types shared between the engine and its clients (serialized over the wire).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// How the grid should treat a column's values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Int,
    Number,
    Bool,
    Date,
    Datetime,
    Time,
    Text,
}

impl Kind {
    pub fn from_sql_type(sql_type: &str) -> Kind {
        let t = sql_type.to_ascii_uppercase();
        match t.as_str() {
            "BOOLEAN" => Kind::Bool,
            "TINYINT" | "SMALLINT" | "INTEGER" | "BIGINT" | "UTINYINT" | "USMALLINT" | "UINTEGER"
            | "UBIGINT" | "HUGEINT" | "UHUGEINT" | "VARINT" | "BIGNUM" => Kind::Int,
            "FLOAT" | "DOUBLE" | "REAL" => Kind::Number,
            "DATE" => Kind::Date,
            "TIME" | "TIME WITH TIME ZONE" => Kind::Time,
            _ if t.starts_with("DECIMAL") => Kind::Number,
            _ if t.starts_with("TIMESTAMP") => Kind::Datetime,
            _ => Kind::Text,
        }
    }

    pub fn is_numeric(self) -> bool {
        matches!(self, Kind::Int | Kind::Number)
    }

}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct Column {
    pub name: String,
    pub sql_type: String,
    pub kind: Kind,
    /// Present for computed (formula) columns.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub formula: Option<String>,
    /// `sheet` for a formula written in spreadsheet syntax; absent for DuckDB SQL formulas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub formula_language: Option<FormulaLanguage>,
}

/// The language of a formula column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "lowercase")]
pub enum FormulaLanguage {
    #[default]
    Sql,
    Sheet,
}

/// An error value in a fetched block: `row` and `column` index `rows` (column relative to
/// `column_start`).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct CellErrorMark {
    pub row: u32,
    pub column: u32,
    pub code: String,
}

impl Column {
    pub fn is_computed(&self) -> bool {
        self.formula.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct TableInfo {
    pub name: String,
    pub row_count: u64,
    pub column_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SortKey {
    pub column: String,
    #[serde(default)]
    pub desc: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    Eq,
    Neq,
    Gt,
    Gte,
    Lt,
    Lte,
    Contains,
    NotContains,
    StartsWith,
    IsEmpty,
    NotEmpty,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Filter {
    pub column: String,
    pub op: FilterOp,
    #[serde(default)]
    #[ts(type = "CellValue")]
    pub value: Value,
}

/// What a client wants to look at: a table, optionally sorted and filtered.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
pub struct ViewSpec {
    pub table: String,
    #[serde(default)]
    pub sort: Vec<SortKey>,
    #[serde(default)]
    pub filters: Vec<Filter>,
    /// Columns left out of the view (and of its exports); the data is untouched.
    #[serde(default)]
    pub hidden: Vec<String>,
}

/// A named, saved view of a table. Live: opening it re-applies the sort and filters to
/// the table's current data.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct SavedView {
    pub id: String,
    pub name: String,
    pub spec: ViewSpec,
    /// Column widths in pixels, by column name.
    #[ts(type = "Record<string, number>")]
    pub widths: std::collections::BTreeMap<String, f64>,
    pub updated_at: u64,
}

impl ViewSpec {
    pub fn is_identity(&self) -> bool {
        self.sort.is_empty() && self.filters.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ViewInfo {
    pub view_id: u64,
    pub spec: ViewSpec,
    pub row_count: u64,
    pub columns: Vec<Column>,
    /// True when rows can be appended by typing below the last row.
    pub can_append: bool,
    /// Why the view's formula values are not current, when it is ordered or filtered by
    /// formula columns whose stored results are being recalculated or failed to be (F.09).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stale: Option<String>,
}

/// A block of rows, row-major, in view order.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct RowBlock {
    pub row_start: u64,
    /// Values of the view's columns `column_start..column_start + rows[i].len()`.
    #[ts(type = "CellValue[][]")]
    pub rows: Vec<Vec<Value>>,
    /// Stable record id of each row (see [`CellEdit::record`]).
    pub ids: Vec<i64>,
    #[serde(default)]
    pub column_start: u32,
    /// Fewer rows than asked for, because the reply reached its size limit; fetch the
    /// rest from `row_start + rows.len()`.
    #[serde(default)]
    pub truncated: bool,
    /// Cells of spreadsheet-formula columns that hold an error (their value is `null`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<CellErrorMark>>")]
    pub errors: Vec<CellErrorMark>,
    /// As [`ViewInfo::stale`]: the formula values are the last committed results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stale: Option<String>,
}

#[derive(Debug, Clone, Deserialize, TS)]
pub struct CellEdit {
    /// View position. Ignored when `record` is given, except that rows at or after the
    /// end append new rows.
    pub row: u64,
    pub column: String,
    /// `None` or an empty string clears the cell.
    pub value: Option<String>,
    /// Record id from a fetch. The edit then lands on that record even if rows were
    /// inserted, deleted or re-sorted since the fetch; it fails if the record is gone.
    #[serde(default)]
    #[ts(optional, as = "Option<i64>")]
    pub record: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct EditOutcome {
    pub table: String,
    pub cells_changed: usize,
    pub rows_appended: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
pub struct Stats {
    pub rows: u64,
    pub count: u64,
    /// Exact: a number, or a string for integers beyond 2^53 and for decimals.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number | string")]
    pub sum: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub avg: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "CellValue")]
    pub min: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "CellValue")]
    pub max: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct QueryPreview {
    pub columns: Vec<Column>,
    #[ts(type = "CellValue[][]")]
    pub rows: Vec<Vec<Value>>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Csv,
    Parquet,
    Json,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Csv => "csv",
            ExportFormat::Parquet => "parquet",
            ExportFormat::Json => "jsonl",
        }
    }
}

/// A table's data revision as read by a query: the revision that last changed its rows,
/// columns or formulas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SourceRevision {
    pub table: String,
    pub data_rev: u64,
}

/// The first rows of a user query (a preview, never the whole result unless `truncated` is
/// false), with how long it took and what it read.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct QueryResult {
    pub columns: Vec<Column>,
    #[ts(type = "CellValue[][]")]
    pub rows: Vec<Vec<Value>>,
    /// More rows exist than were returned (the row limit or the reply size was reached).
    pub truncated: bool,
    /// Most rows this preview could hold.
    pub row_limit: u64,
    pub elapsed_ms: u64,
    /// Workbook revision of the snapshot the query read.
    pub revision: u64,
    /// User tables the query read, with their data revisions in that snapshot.
    pub sources: Vec<SourceRevision>,
}

/// A user table and its columns, for the schema browser and autocomplete.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct TableSchema {
    pub name: String,
    pub row_count: u64,
    pub columns: Vec<Column>,
}

/// A saved SQL query: plain (`query`, run on demand) or `live` (run on current data every
/// time it is opened; its result goes stale when a source table changes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum QueryKind {
    Query,
    Live,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct SavedQuery {
    pub id: String,
    pub name: String,
    pub kind: QueryKind,
    /// Latest version number (versions start at 1 and never change).
    pub version: u64,
    pub sql: String,
    pub params: Vec<crate::sqlpolicy::QueryParam>,
    pub created_at: u64,
    pub updated_at: u64,
}

/// One immutable version of a saved query.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct QueryVersion {
    pub version: u64,
    pub sql: String,
    pub params: Vec<crate::sqlpolicy::QueryParam>,
    pub created_at: u64,
    /// Workbook revision that saved it.
    pub revision: u64,
}

/// A source table of a materialized result: its data revision when the result was computed
/// and now (absent when the table no longer exists).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct MaterializedSource {
    pub table: String,
    pub data_rev: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub current_rev: Option<u64>,
}

/// A table created from a query, and how it was computed.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct MaterializedTable {
    pub table: String,
    /// The saved query and version it came from, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub query_version: Option<u64>,
    pub sql: String,
    pub params: Vec<crate::sqlpolicy::QueryParam>,
    pub sources: Vec<MaterializedSource>,
    /// Workbook revision at which the rows were computed.
    pub revision: u64,
    pub refreshed_at: u64,
    /// A source changed (or disappeared) since the rows were computed.
    pub stale: bool,
    /// The table itself was changed by hand since; a refresh replaces those changes.
    pub edited: bool,
}

/// What `sql_change` did.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct SqlChangeOutcome {
    pub table: String,
    /// `insert`, `update` or `delete`.
    pub kind: String,
    pub rows: u64,
}
