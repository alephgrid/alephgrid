//! File import: preview before import, per-column type overrides and errors that say
//! what to change. Readers are DuckDB's own (CSV/TSV, Parquet, JSON); `.xlsx` goes to
//! sheets instead (see `sheets.rs`).

use std::path::Path;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::model::Kind;
use crate::sql::{ident, lit};

/// Rows shown in an import preview.
pub const PREVIEW_ROWS: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Csv,
    Parquet,
    Json,
}

impl Format {
    pub fn from_path(path: &Path) -> Format {
        let lower = path.to_string_lossy().to_ascii_lowercase();
        let stripped = lower.trim_end_matches(".gz").trim_end_matches(".zst");
        if stripped.ends_with(".parquet") {
            Format::Parquet
        } else if stripped.ends_with(".json") || stripped.ends_with(".jsonl") || stripped.ends_with(".ndjson") {
            Format::Json
        } else {
            Format::Csv
        }
    }

    fn label(self) -> &'static str {
        match self {
            Format::Csv => "CSV",
            Format::Parquet => "Parquet",
            Format::Json => "JSON",
        }
    }
}

/// A user's choice of type for one column.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ColumnType {
    pub name: String,
    pub sql_type: String,
}

/// One column in an import preview.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct PreviewColumn {
    pub name: String,
    /// The type the column will be imported as (detected, or the override).
    pub sql_type: String,
    pub kind: Kind,
    /// What the reader detected, before overrides.
    pub detected: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct FilePreview {
    /// "CSV", "Parquet" or "JSON".
    pub format: String,
    pub suggested_name: String,
    pub columns: Vec<PreviewColumn>,
    /// The first rows, converted as they will be imported.
    #[ts(type = "CellValue[][]")]
    pub rows: Vec<Vec<Value>>,
    /// Types a user can choose from.
    pub type_choices: Vec<String>,
}

/// An import failure with enough structure for the UI to offer a fix.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ImportProblem {
    pub message: String,
    #[ts(optional)]
    pub line: Option<u64>,
    #[ts(optional)]
    pub column: Option<String>,
    #[ts(optional)]
    pub value: Option<String>,
    /// A type that would accept the failing value (always Text for conversion errors).
    #[ts(optional)]
    pub suggested_type: Option<String>,
}

impl std::fmt::Display for ImportProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ImportProblem {}

pub const TYPE_CHOICES: &[&str] =
    &["VARCHAR", "BIGINT", "DOUBLE", "DECIMAL(18,2)", "DECIMAL(38,10)", "BOOLEAN", "DATE", "TIMESTAMP", "TIMESTAMPTZ", "TIME"];

/// Types an override may use: the choices above plus any `DECIMAL(p,s)` and common aliases.
pub fn valid_override_type(t: &str) -> bool {
    let u = t.trim().to_ascii_uppercase();
    if TYPE_CHOICES.contains(&u.as_str()) || ["INTEGER", "HUGEINT", "SMALLINT", "TINYINT", "FLOAT", "TEXT"].contains(&u.as_str()) {
        return true;
    }
    if let Some(args) = u.strip_prefix("DECIMAL(").and_then(|a| a.strip_suffix(')')) {
        let parts: Vec<&str> = args.split(',').map(str::trim).collect();
        if let [p, s] = parts.as_slice() {
            if let (Ok(p), Ok(s)) = (p.parse::<u32>(), s.parse::<u32>()) {
                return (1..=38).contains(&p) && s <= p;
            }
        }
    }
    false
}

/// `SELECT … FROM <reader>` for a file, with overrides applied the right way for each
/// format: CSV types are set in the reader (so `007` read as text stays `007`); other
/// formats are cast after reading.
pub(crate) fn reader_select(path: &Path, overrides: &[ColumnType]) -> Result<String> {
    for o in overrides {
        if !valid_override_type(&o.sql_type) {
            bail!("{:?} is not a type you can import as", o.sql_type);
        }
    }
    let file = lit(&path.to_string_lossy());
    Ok(match Format::from_path(path) {
        Format::Csv => {
            let types = if overrides.is_empty() {
                String::new()
            } else {
                let pairs: Vec<String> = overrides.iter().map(|o| format!("{}: {}", lit(&o.name), lit(&o.sql_type))).collect();
                format!(", types = {{{}}}", pairs.join(", "))
            };
            format!("SELECT * FROM read_csv({file}, auto_detect = true, sample_size = 100000{types})")
        }
        format => {
            let reader = if format == Format::Parquet { format!("read_parquet({file})") } else { format!("read_json_auto({file})") };
            if overrides.is_empty() {
                format!("SELECT * FROM {reader}")
            } else {
                let casts: Vec<String> =
                    overrides.iter().map(|o| format!("CAST({} AS {}) AS {}", ident(&o.name), o.sql_type, ident(&o.name))).collect();
                format!("SELECT * REPLACE ({}) FROM {reader}", casts.join(", "))
            }
        }
    })
}

pub(crate) fn suggested_name(path: &Path) -> String {
    let stem = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "table".into());
    stem.split('.').next().unwrap_or("table").to_string()
}

pub(crate) fn format_label(path: &Path) -> &'static str {
    Format::from_path(path).label()
}

/// Turn a DuckDB import error into a message that says what to change.
pub fn explain_error(raw: &str) -> ImportProblem {
    let line = raw.split("CSV Error on Line: ").nth(1).and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next()).and_then(|n| n.parse().ok());
    let conversion = raw
        .split("Error when converting column \"")
        .nth(1)
        .and_then(|rest| {
            let (column, rest) = rest.split_once("\". Could not convert string \"")?;
            let (value, rest) = rest.split_once("\" to '")?;
            let ty = rest.split('\'').next()?;
            Some((column.to_string(), value.to_string(), ty.to_string()))
        });
    match conversion {
        Some((column, value, ty)) => ImportProblem {
            message: format!(
                "{}\"{value}\" in column \"{column}\" is not a valid {ty}. Import \"{column}\" as VARCHAR (text), or fix the value in the file.",
                line.map(|l| format!("Line {l}: ")).unwrap_or_default()
            ),
            line,
            column: Some(column),
            value: Some(value),
            suggested_type: Some("VARCHAR".into()),
        },
        None => {
            let first = raw.lines().next().unwrap_or(raw).trim();
            let message = if first.starts_with("INTERRUPT") {
                "Cancelled.".to_string()
            } else if first.contains("No files found") || first.contains("No such file") {
                "The uploaded file could not be found on the server; upload it again.".to_string()
            } else if raw.contains("Invalid Input Error: No magic bytes found") || raw.contains("is not a Parquet file") {
                "This file is not a valid Parquet file.".to_string()
            } else if raw.contains("Malformed JSON") || raw.contains("JSON") && raw.contains("Invalid Input Error") {
                format!("The JSON could not be read: {first}")
            } else {
                first.to_string()
            };
            ImportProblem { message, line, column: None, value: None, suggested_type: None }
        }
    }
}

/// Wrap a DuckDB error from reading `path` as an [`ImportProblem`].
pub(crate) fn import_error(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!(explain_error(&e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explains_conversion_errors() {
        let raw = "Conversion Error: CSV Error on Line: 30002\nOriginal Line: oops,x\nError when converting column \"n\". Could not convert string \"oops\" to 'BIGINT'\nColumn n is being converted as type BIGINT";
        let p = explain_error(raw);
        assert_eq!(p.line, Some(30002));
        assert_eq!(p.column.as_deref(), Some("n"));
        assert_eq!(p.value.as_deref(), Some("oops"));
        assert_eq!(p.suggested_type.as_deref(), Some("VARCHAR"));
        assert_eq!(p.message, "Line 30002: \"oops\" in column \"n\" is not a valid BIGINT. Import \"n\" as VARCHAR (text), or fix the value in the file.");
        assert_eq!(explain_error("INTERRUPT Error: Interrupted!").message, "Cancelled.");
        let other = explain_error("IO Error: something odd\nmore detail");
        assert_eq!((other.message.as_str(), other.column), ("IO Error: something odd", None));
    }

    #[test]
    fn override_types_are_allow_listed() {
        for ok in ["VARCHAR", "bigint", "DECIMAL(10,2)", "decimal(38, 10)", "TIMESTAMPTZ"] {
            assert!(valid_override_type(ok), "{ok}");
        }
        for bad in ["VARCHAR); DROP TABLE x; --", "DECIMAL(39,2)", "DECIMAL(5,6)", "", "MAP(INT,INT)", "STRUCT(a INT)"] {
            assert!(!valid_override_type(bad), "{bad}");
        }
    }
}
