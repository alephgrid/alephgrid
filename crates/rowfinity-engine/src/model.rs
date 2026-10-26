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

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
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

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct Filter {
    pub column: String,
    pub op: FilterOp,
    #[serde(default)]
    #[ts(type = "CellValue")]
    pub value: Value,
}

/// What a client wants to look at: a table, optionally sorted and filtered.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
pub struct ViewSpec {
    pub table: String,
    #[serde(default)]
    pub sort: Vec<SortKey>,
    #[serde(default)]
    pub filters: Vec<Filter>,
}

impl ViewSpec {
    pub fn is_identity(&self) -> bool {
        self.sort.is_empty() && self.filters.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, TS)]
pub struct ViewInfo {
    pub view_id: u64,
    pub spec: ViewSpec,
    pub row_count: u64,
    pub columns: Vec<Column>,
    /// True when rows can be appended by typing below the last row.
    pub can_append: bool,
}

/// A block of rows, row-major, in view order.
#[derive(Debug, Clone, Serialize, TS)]
pub struct RowBlock {
    pub row_start: u64,
    #[ts(type = "CellValue[][]")]
    pub rows: Vec<Vec<Value>>,
}

#[derive(Debug, Clone, Deserialize, TS)]
pub struct CellEdit {
    pub row: u64,
    pub column: String,
    /// `None` or an empty string clears the cell.
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
pub struct EditOutcome {
    pub table: String,
    pub cells_changed: usize,
    pub rows_appended: u64,
}

#[derive(Debug, Clone, Default, Serialize, TS)]
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

#[derive(Debug, Clone, Serialize, TS)]
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
