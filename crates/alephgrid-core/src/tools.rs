//! Data tools (F.12): validation rules with dropdown values, deduplication, split, trim and
//! type-conversion previews applied as derived result tables, and formula inspection.
//!
//! Validation is enforced on every path that writes table cells: edits (typing, pasting,
//! appending rows) and SQL changes, which are refused when they would add violations. Existing
//! values are not rewritten: setting a rule reports how many rows break it. The transforms never
//! change their source: they create a new table (one mutation, undone by deleting it), so the
//! original stays as it was.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::{Column, TableInfo};
use crate::reads::Db;
use crate::sql::{ident, lit};
use crate::{storage, Workbook};

pub const MAX_RULES: usize = 32;
pub const MAX_LIST_VALUES: usize = 500;
pub const PREVIEW_ROWS: usize = 50;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Rule {
    /// One of a list (a dropdown in the grid); blank allowed unless `Required` is also set.
    OneOf { values: Vec<String> },
    /// A number between `min` and `max` (either may be open).
    Between {
        #[ts(optional)]
        min: Option<f64>,
        #[ts(optional)]
        max: Option<f64>,
    },
    /// Not blank.
    Required,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Validation {
    pub column: String,
    pub rule: Rule,
}

/// Saving rules reports what they find in the data already there.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ValidationReport {
    pub rules: Vec<Validation>,
    /// Rows breaking each rule now, in rule order.
    pub violations: Vec<u64>,
}

fn key(table: &str) -> String {
    format!("validation:{}", table.to_lowercase())
}

pub(crate) fn validations(conn: &duckdb::Connection, table: &str) -> Result<Vec<Validation>> {
    crate::analysis::read_meta(conn, &key(table))
}

/// SQL selecting rows of `table` (by its base relation `t`) that break `v`.
fn violation_sql(v: &Validation) -> String {
    let c = ident(&v.column);
    match &v.rule {
        Rule::OneOf { values } => format!("{c} IS NOT NULL AND CAST({c} AS VARCHAR) NOT IN ({})", values.iter().map(|x| lit(x)).collect::<Vec<_>>().join(", ")),
        Rule::Between { min, max } => {
            let n = format!("TRY_CAST({c} AS DOUBLE)");
            let mut parts = vec![format!("({c} IS NOT NULL AND {n} IS NULL)")];
            if let Some(m) = min { parts.push(format!("{n} < {m}")); }
            if let Some(m) = max { parts.push(format!("{n} > {m}")); }
            format!("({})", parts.join(" OR "))
        }
        Rule::Required => format!("({c} IS NULL OR CAST({c} AS VARCHAR) = '')"),
    }
}

fn violations_now(conn: &duckdb::Connection, table: &str, rules: &[Validation]) -> Result<Vec<u64>> {
    rules.iter().map(|v| {
        let n: i64 = conn.query_row(&format!("SELECT count(*) FROM main.{} WHERE {}", ident(table), violation_sql(v)), [], |r| r.get(0))?;
        Ok(n.max(0) as u64)
    }).collect()
}

/// Refuse a typed value that breaks a rule of its column (`None`: cleared).
pub(crate) fn check_value(rules: &[Validation], column: &str, value: Option<&str>) -> Result<()> {
    for v in rules.iter().filter(|v| v.column == column) {
        let ok = match (&v.rule, value) {
            (Rule::Required, None) => false,
            (Rule::Required, Some(s)) => !s.is_empty(),
            (_, None) => true,
            (Rule::OneOf { values }, Some(s)) => values.iter().any(|x| x == s),
            (Rule::Between { min, max }, Some(s)) => s.trim().parse::<f64>().is_ok_and(|n| min.is_none_or(|m| n >= m) && max.is_none_or(|m| n <= m)),
        };
        if !ok {
            let what = match &v.rule {
                Rule::OneOf { values } => format!("one of {}", values.iter().take(10).map(|x| format!("{x:?}")).collect::<Vec<_>>().join(", ")),
                Rule::Between { min, max } => format!("a number{}{}", min.map(|m| format!(" ≥ {m}")).unwrap_or_default(), max.map(|m| format!(" ≤ {m}")).unwrap_or_default()),
                Rule::Required => "not blank".into(),
            };
            bail!("{:?} is not allowed in column {column:?}: it must be {what}", value.unwrap_or(""));
        }
    }
    Ok(())
}

/// Violations of every validated table, to compare before and after a SQL change.
pub(crate) fn all_violations(conn: &duckdb::Connection) -> Result<Vec<(String, Vec<u64>)>> {
    let mut out = Vec::new();
    for t in storage::user_tables(conn)? {
        let rules = validations(conn, &t)?;
        if !rules.is_empty() {
            out.push((t.clone(), violations_now(conn, &t, &rules)?));
        }
    }
    Ok(out)
}

/// Refuse a change that added violations to any validated table.
pub(crate) fn refuse_new_violations(conn: &duckdb::Connection, before: &[(String, Vec<u64>)]) -> Result<()> {
    for (t, after) in all_violations(conn)? {
        let prior = before.iter().find(|(b, _)| *b == t).map(|(_, v)| v.clone()).unwrap_or_default();
        for (k, n) in after.iter().enumerate() {
            if *n > prior.get(k).copied().unwrap_or(0) {
                let rule = validations(conn, &t)?.get(k).cloned();
                bail!("this change breaks the validation of {t}.{:?} ({n} rows would not be allowed); change the values or the rule", rule.map(|r| r.column).unwrap_or_default());
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Transform {
    /// Rows whose `keys` columns repeat an earlier row are left out (first kept, row order).
    Dedupe { keys: Vec<String> },
    /// `column` split by `delimiter` into `parts` new text columns `<column>_1…`.
    Split { column: String, delimiter: String, parts: u32 },
    /// `column` without leading and trailing spaces (and repeated inner spaces when `inner`).
    Trim { column: String, #[serde(default)] inner: bool },
    /// `column` converted to `to` (BIGINT, DOUBLE, DATE, BOOLEAN or VARCHAR); values that do
    /// not convert become blank and are counted.
    Convert { column: String, to: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct TransformPreview {
    /// The first rows of the result.
    pub sample: crate::model::QueryPreview,
    /// Rows the result would have.
    pub rows: u64,
    /// Rows left out (dedupe) or values that did not convert (convert).
    pub affected: u64,
}

fn transform_sql(db: &Db, table: &str, t: &Transform) -> Result<(String, String)> {
    let cols: Vec<Column> = db.columns(table)?.into_iter().filter(|c| c.formula.is_none()).collect();
    let has = |c: &str| cols.iter().any(|x| x.name == c);
    let src = format!("main.{}", ident(table));
    let all = cols.iter().map(|c| ident(&c.name)).collect::<Vec<_>>();
    Ok(match t {
        Transform::Dedupe { keys } => {
            if keys.is_empty() || keys.len() > 8 || keys.iter().any(|k| !has(k)) {
                bail!("deduplicate by 1–8 stored columns");
            }
            let k = keys.iter().map(|x| ident(x)).collect::<Vec<_>>().join(", ");
            (
                format!("SELECT {} FROM (SELECT *, row_number() OVER (PARTITION BY {k} ORDER BY _rf_ord) AS _rf_n FROM {src}) WHERE _rf_n = 1 ORDER BY _rf_ord", all.join(", ")),
                format!("SELECT count(*) - count(DISTINCT ({k})) FROM {src}"),
            )
        }
        Transform::Split { column, delimiter, parts } => {
            if !has(column) || delimiter.is_empty() || *parts == 0 || *parts > 16 {
                bail!("split a stored column by a non-empty delimiter into 1–16 parts");
            }
            let pieces = (1..=*parts).map(|i| format!("string_split(CAST({} AS VARCHAR), {})[{i}] AS {}", ident(column), lit(delimiter), ident(&format!("{column}_{i}")))).collect::<Vec<_>>();
            (format!("SELECT {}, {} FROM {src} ORDER BY _rf_ord", all.join(", "), pieces.join(", ")),
             format!("SELECT count(*) FILTER (WHERE len(string_split(CAST({} AS VARCHAR), {})) > {parts}) FROM {src}", ident(column), lit(delimiter)))
        }
        Transform::Trim { column, inner } => {
            if !has(column) {
                bail!("trim a stored column");
            }
            let c = ident(column);
            let trimmed = if *inner { format!("regexp_replace(trim(CAST({c} AS VARCHAR)), ' +', ' ', 'g')") } else { format!("trim(CAST({c} AS VARCHAR))") };
            let sel = cols.iter().map(|x| if x.name == *column { format!("{trimmed} AS {c}") } else { ident(&x.name) }).collect::<Vec<_>>();
            (format!("SELECT {} FROM {src} ORDER BY _rf_ord", sel.join(", ")), format!("SELECT count(*) FILTER (WHERE CAST({c} AS VARCHAR) IS DISTINCT FROM {trimmed}) FROM {src}"))
        }
        Transform::Convert { column, to } => {
            let to = to.to_uppercase();
            if !has(column) || !["BIGINT", "DOUBLE", "DATE", "BOOLEAN", "VARCHAR"].contains(&to.as_str()) {
                bail!("convert a stored column to BIGINT, DOUBLE, DATE, BOOLEAN or VARCHAR");
            }
            let c = ident(column);
            let conv = format!("TRY_CAST(trim(CAST({c} AS VARCHAR)) AS {to})");
            let sel = cols.iter().map(|x| if x.name == *column { format!("{conv} AS {c}") } else { ident(&x.name) }).collect::<Vec<_>>();
            (format!("SELECT {} FROM {src} ORDER BY _rf_ord", sel.join(", ")), format!("SELECT count(*) FILTER (WHERE {c} IS NOT NULL AND {conv} IS NULL) FROM {src}"))
        }
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct FormulaInspection {
    pub table: String,
    pub column: String,
    pub formula: String,
    /// Columns of the table itself, whole columns of other tables, and sheet cells it reads.
    pub reads_columns: Vec<String>,
    pub reads_tables: Vec<String>,
    pub reads_cells: Vec<String>,
    /// Formula columns (`table.column`) and sheet cells that read this column.
    pub read_by: Vec<String>,
    /// How it is evaluated: `lookup join`, `aggregate join`, `nearest-match join`, `row
    /// expression`.
    pub plan: Vec<String>,
    /// Rows of the table whose value is an error, by code.
    pub errors: Vec<(String, u64)>,
}

impl Workbook {
    pub fn validations(&self, table: &str) -> Result<Vec<Validation>> {
        validations(&self.conn, table)
    }

    /// Replace a table's validation rules; reports the rows breaking each one now.
    pub fn set_validations(&mut self, table: &str, rules: Vec<Validation>) -> Result<ValidationReport> {
        if !self.db().table_exists(table)? {
            bail!("unknown table {table:?}");
        }
        if rules.len() > MAX_RULES {
            bail!("at most {MAX_RULES} rules per table");
        }
        let cols = self.columns(table)?;
        for v in &rules {
            let c = cols.iter().find(|c| c.name == v.column).ok_or_else(|| anyhow!("unknown column {:?}", v.column))?;
            if c.formula.is_some() {
                bail!("{:?} is a formula column; validate the columns it reads", v.column);
            }
            match &v.rule {
                Rule::OneOf { values } if values.is_empty() || values.len() > MAX_LIST_VALUES => bail!("a list needs 1–{MAX_LIST_VALUES} values"),
                Rule::Between { min: Some(a), max: Some(b) } if a > b => bail!("the minimum is above the maximum"),
                _ => {}
            }
        }
        self.mutating("set_validations", None, |wb| {
            crate::analysis::save_meta(&wb.conn, &key(table), &rules)?;
            let violations = violations_now(&wb.conn, table, &rules)?;
            Ok(ValidationReport { rules, violations })
        })
    }

    /// Preview a transform: the first rows of its result, its size and what it changes.
    pub fn preview_transform(&self, table: &str, t: &Transform) -> Result<TransformPreview> {
        let db = self.db();
        let (sql, affected) = transform_sql(&db, table, t)?;
        let rows: i64 = self.conn.query_row(&format!("SELECT count(*) FROM ({sql})"), [], |r| r.get(0))?;
        let affected: i64 = self.conn.query_row(&affected, [], |r| r.get(0))?;
        Ok(TransformPreview { sample: db.run_select(&sql, PREVIEW_ROWS)?, rows: rows.max(0) as u64, affected: affected.max(0) as u64 })
    }

    /// Apply a transform as a new table `name`; the source is not changed.
    pub fn apply_transform(&mut self, table: &str, t: &Transform, name: &str) -> Result<TableInfo> {
        let (sql, _) = transform_sql(&self.db(), table, t)?;
        self.create_table_from_query(name, &sql)
    }

    /// What a formula column reads, what reads it, how it runs, and its errors.
    pub fn inspect_formula(&self, table: &str, column: &str) -> Result<FormulaInspection> {
        use crate::table_formula::{references, Reference};
        let (_, formula) = self.db().formulas(table)?.into_iter().find(|(n, _)| n == column).ok_or_else(|| anyhow!("{column:?} is not a formula column"))?;
        let mut out = FormulaInspection { table: table.into(), column: column.into(), formula: formula.clone(), reads_columns: vec![], reads_tables: vec![], reads_cells: vec![], read_by: vec![], plan: vec![], errors: vec![] };
        if self.db().sheet_formulas(table)?.contains(column) {
            for r in references(&formula).map_err(|e| anyhow!("{e}"))? {
                match r {
                    Reference::Own { column, .. } => out.reads_columns.push(column),
                    Reference::Column { table, column, .. } => out.reads_tables.push(format!("{table}[{column}]")),
                    Reference::Table { table, .. } => out.reads_tables.push(table),
                    Reference::Cell { cell, .. } => out.reads_cells.push(cell.text((false, false))),
                }
            }
            let upper = formula.to_uppercase();
            for (f, label) in [("XLOOKUP(", "lookup join"), ("VLOOKUP(", "lookup join"), ("MATCH(", "lookup join"), ("INDEX(", "position join"), ("IFS(", "row expression"), ("SUMIF", "aggregate join"), ("COUNTIF", "aggregate join"), ("AVERAGEIF", "aggregate join"), ("MINIFS(", "aggregate join"), ("MAXIFS(", "aggregate join")] {
                if upper.contains(f) && !out.plan.iter().any(|p| p == label) {
                    out.plan.push(label.into());
                }
            }
            if out.plan.is_empty() {
                out.plan.push("row expression".into());
            }
            let e = crate::reads::error_column(column);
            out.errors = self.conn.prepare(&format!("SELECT {e}, count(*) FROM ({}) WHERE {e} IS NOT NULL GROUP BY 1 ORDER BY 2 DESC", self.db().base_select(table)?, e = ident(&e)))?
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)))?.collect::<Result<_, _>>()?;
        } else {
            out.plan.push("SQL expression".into());
        }
        for (t, name, f) in self.all_sheet_formulas()? {
            let Ok(refs) = references(&f) else { continue };
            let reads = refs.iter().any(|r| match r {
                Reference::Own { column: c, .. } => t == table && c.eq_ignore_ascii_case(column) && name != column,
                Reference::Column { table: rt, column: c, .. } => rt.eq_ignore_ascii_case(table) && c.eq_ignore_ascii_case(column),
                _ => false,
            });
            if reads {
                out.read_by.push(format!("{t}.{name}"));
            }
        }
        for (place, f) in self.table_cells_in_use()? {
            if references(&f).is_ok_and(|refs| refs.iter().any(|r| matches!(r, Reference::Column { table: rt, column: c, .. } if rt.eq_ignore_ascii_case(table) && c.eq_ignore_ascii_case(column)))) {
                out.read_by.push(place.text((false, false)));
            }
        }
        Ok(out)
    }
}
