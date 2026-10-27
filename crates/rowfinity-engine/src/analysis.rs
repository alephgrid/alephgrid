//! Typed no-code analysis plans. Only validated identifiers and literals enter SQL;
//! the existing query path owns snapshots, calculation expansion and reply bounds.
use crate::{
    reads::Db,
    sql::{ident, lit},
    storage, Column, QueryResult, TableInfo, Workbook,
};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use ts_rs::TS;

pub const OUTPUT_ROWS: usize = 500;
const COPY_ROWS: usize = 10_000;
const ANALYSES: &str = "analyses";
const PRESENTATIONS: &str = "presentations";
const UNDO: &str = "replace_undo";
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Aggregate {
    Count,
    Sum,
    Average,
    Min,
    Max,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct Pivot {
    pub column: String,
    pub values: Vec<Option<String>>,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum JoinKind {
    Inner,
    Left,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnalysisSpec {
    Summary { table: String, groups: Vec<String>, aggregate: Aggregate, value: Option<String>, pivot: Option<Pivot> },
    Join { left: String, right: String, left_key: String, right_key: String, join: JoinKind },
    Profile { table: String, columns: Vec<String> },
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct AnalysisResult {
    pub result: QueryResult,
    pub diagnostics: Option<QueryResult>,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct SavedAnalysis {
    pub id: String,
    pub name: String,
    pub spec: AnalysisSpec,
    pub chart: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ReplaceSpec {
    pub table: String,
    pub column: String,
    pub find: String,
    pub replacement: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ReplacePreview {
    pub matches: u64,
    pub sample: QueryResult,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ReplaceOutcome {
    pub table: String,
    pub rows: u64,
    pub undo_revision: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DisplayKind {
    General,
    Number,
    Percent,
    Date,
    Datetime,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DisplayFormat {
    pub kind: DisplayKind,
    pub decimals: u8,
    pub locale: String,
    pub timezone: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    Greater,
    Less,
    Equal,
    Empty,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ColorRule {
    pub column: String,
    pub condition: Condition,
    pub value: f64,
    pub color: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
pub struct Presentation {
    pub hidden: Vec<String>,
    pub frozen: u8,
    pub formats: BTreeMap<String, DisplayFormat>,
    pub rules: Vec<ColorRule>,
}
#[derive(Serialize, Deserialize)]
struct ReplacementUndo {
    spec: ReplaceSpec,
    revision: u64,
    cells: Vec<(i64, String)>,
}

fn read_meta<T: serde::de::DeserializeOwned + Default>(conn: &duckdb::Connection, key: &str) -> Result<T> {
    match storage::meta_get(conn, key)? {
        Some(s) => Ok(serde_json::from_str(&s)?),
        None => Ok(T::default()),
    }
}
fn save_meta<T: Serialize>(conn: &duckdb::Connection, key: &str, value: &T) -> Result<()> {
    let json = serde_json::to_string(value)?;
    if json.len() > 4 << 20 {
        bail!("saved analysis data exceeds 4 MiB");
    }
    storage::meta_set(conn, key, &json)
}
pub(crate) fn analyses(conn: &duckdb::Connection) -> Result<Vec<SavedAnalysis>> {
    read_meta(conn, ANALYSES)
}
pub(crate) fn presentation(db: &Db, table: &str) -> Result<Presentation> {
    db.columns(table)?;
    let all: BTreeMap<String, Presentation> = read_meta(db.conn, PRESENTATIONS)?;
    let columns = db.columns(table)?;
    let valid: BTreeSet<_> = columns.iter().map(|c| c.name.as_str()).collect();
    let mut p = all.get(table).cloned().unwrap_or_default();
    p.hidden.retain(|n| valid.contains(n.as_str()));
    if p.hidden.len() >= columns.len() {
        p.hidden.clear();
    }
    p.formats.retain(|n, _| valid.contains(n.as_str()));
    p.rules.retain(|r| valid.contains(r.column.as_str()));
    Ok(p)
}
fn column(db: &Db, table: &str, name: &str) -> Result<Column> {
    db.columns(table)?
        .into_iter()
        .find(|c| c.name == name)
        .ok_or_else(|| anyhow!("unknown column {name} in {table}; update this analysis after schema changes"))
}
fn names(db: &Db, table: &str, list: &[String], max: usize) -> Result<()> {
    if list.len() > max || list.iter().collect::<BTreeSet<_>>().len() != list.len() {
        bail!("select at most {max} different columns");
    }
    for n in list {
        column(db, table, n)?;
    }
    Ok(())
}
fn compile(db: &Db, spec: &AnalysisSpec) -> Result<(String, Option<String>)> {
    Ok(match spec {
        AnalysisSpec::Summary { table, groups, aggregate, value, pivot } => {
            db.columns(table)?;
            names(db, table, groups, 4)?;
            let measure = match aggregate {
                Aggregate::Count => "count(*)".into(),
                other => {
                    let name = value.as_deref().ok_or_else(|| anyhow!("select a value column"))?;
                    let c = column(db, table, name)?;
                    if matches!(other, Aggregate::Sum | Aggregate::Average) && !c.kind.is_numeric() {
                        bail!("sum and average require a numeric column");
                    }
                    let op = match other {
                        Aggregate::Sum => "sum",
                        Aggregate::Average => "avg",
                        Aggregate::Min => "min",
                        Aggregate::Max => "max",
                        _ => unreachable!(),
                    };
                    format!("{op}({})", ident(name))
                }
            };
            let mut select: Vec<String> = groups.iter().map(|n| ident(n)).collect();
            if let Some(p) = pivot {
                column(db, table, &p.column)?;
                if p.values.is_empty() || p.values.len() > 24 || p.values.iter().collect::<BTreeSet<_>>().len() != p.values.len() {
                    bail!("pivot needs 1–24 distinct explicit values");
                }
                for (i, v) in p.values.iter().enumerate() {
                    if v.as_ref().is_some_and(|s| s.len() > 1024) {
                        bail!("pivot value too long");
                    }
                    let cond = match v {
                        Some(v) => format!("CAST({} AS VARCHAR)={}", ident(&p.column), lit(v)),
                        None => format!("{} IS NULL", ident(&p.column)),
                    };
                    select.push(format!(
                        "{measure} FILTER (WHERE {cond}) AS {}",
                        ident(&format!("pivot_{}: {}", i + 1, v.as_deref().unwrap_or("NULL")))
                    ));
                }
            } else {
                select.push(format!("{measure} AS {}", ident("measure")));
            }
            let group = if groups.is_empty() {
                String::new()
            } else {
                format!(
                    " GROUP BY {} ORDER BY {}",
                    groups.iter().map(|n| ident(n)).collect::<Vec<_>>().join(","),
                    (1..=groups.len()).map(|n| n.to_string()).collect::<Vec<_>>().join(",")
                )
            };
            (format!("SELECT {} FROM {}{group}", select.join(","), ident(table)), None)
        }
        AnalysisSpec::Join { left, right, left_key, right_key, join } => {
            let l = column(db, left, left_key)?;
            let r = column(db, right, right_key)?;
            if l.sql_type != r.sql_type {
                bail!("join keys must have identical types ({} versus {}); convert them explicitly first", l.sql_type, r.sql_type);
            }
            let left_cols = db.columns(left)?;
            let right_cols = db.columns(right)?;
            if left_cols.len() + right_cols.len() > 100 {
                bail!("join output exceeds 100 columns");
            }
            let select: Vec<String> = left_cols
                .iter()
                .map(|c| format!("l.{} AS {}", ident(&c.name), ident(&format!("left_{}", c.name))))
                .chain(right_cols.iter().map(|c| format!("r.{} AS {}", ident(&c.name), ident(&format!("right_{}", c.name)))))
                .collect();
            let op = match join {
                JoinKind::Inner => "INNER",
                JoinKind::Left => "LEFT",
            };
            let sql = format!(
                "SELECT {} FROM {} l {op} JOIN {} r ON l.{}=r.{}",
                select.join(","),
                ident(left),
                ident(right),
                ident(left_key),
                ident(right_key)
            );
            // Count multiplicities before joining rows, so duplicate diagnostics do not
            // materialize a potentially enormous Cartesian product.
            let diag=format!("WITH l AS (SELECT {} AS k,count(*) AS n FROM {} GROUP BY 1), r AS (SELECT {} AS k,count(*) AS n FROM {} GROUP BY 1) SELECT (SELECT count(*) FROM l WHERE k IS NOT NULL AND n>1) AS left_duplicate_keys,(SELECT count(*) FROM r WHERE k IS NOT NULL AND n>1) AS right_duplicate_keys,(SELECT coalesce(sum(l.n::HUGEINT * {}),0) FROM l {op} JOIN r ON l.k=r.k) AS output_rows",ident(left_key),ident(left),ident(right_key),ident(right),if matches!(join,JoinKind::Left){"coalesce(r.n,1)"}else{"r.n"});
            (sql, Some(diag))
        }
        AnalysisSpec::Profile { table, columns } => {
            if columns.is_empty() {
                bail!("select at least one column to profile");
            }
            names(db, table, columns, 32)?;
            let parts=columns.iter().map(|name|{
                let c=column(db,table,name)?;let n=ident(name);
                Ok(format!("SELECT {} AS column_name,{} AS sql_type,count(*) AS rows,count(*)-count({n}) AS nulls,count(DISTINCT {n}) AS distinct_non_null,count({n})-count(DISTINCT {n}) AS duplicate_non_null FROM {}",lit(name),lit(&c.sql_type),ident(table)))
            }).collect::<Result<Vec<_>>>()?;
            (parts.join(" UNION ALL "), None)
        }
    })
}
pub(crate) fn analyze(db: &Db, spec: &AnalysisSpec) -> Result<AnalysisResult> {
    db.snapshot(|| {
        let snap = Db { conn: db.conn, shared: db.shared, in_txn: true };
        let (sql, diagnostics) = compile(&snap, spec)?;
        let diagnostics = diagnostics.map(|q| snap.run_query(&q, &[], 1)).transpose()?;
        Ok(AnalysisResult { result: snap.run_query(&sql, &[], OUTPUT_ROWS)?, diagnostics })
    })
}
fn replace_parts(db: &Db, spec: &ReplaceSpec) -> Result<(String, String)> {
    let c = column(db, &spec.table, &spec.column)?;
    if c.sql_type != "VARCHAR" || c.formula.is_some() {
        bail!("replace requires an editable VARCHAR column");
    }
    if spec.find.is_empty() || spec.find.len() > 1024 || spec.replacement.len() > 4096 {
        bail!("find must be 1–1024 bytes; replacement at most 4096 bytes");
    }
    let n = ident(&spec.column);
    Ok((format!("contains({n},{})", lit(&spec.find)), format!("replace({n},{},{})", lit(&spec.find), lit(&spec.replacement))))
}
pub(crate) fn replace_preview(db: &Db, spec: &ReplaceSpec) -> Result<ReplacePreview> {
    db.snapshot(|| {
        let snap = Db { conn: db.conn, shared: db.shared, in_txn: true };
        let (cond, expr) = replace_parts(&snap, spec)?;
        let count: u64 = snap.conn.query_row(&format!("SELECT count(*) FROM {} WHERE {cond}", ident(&spec.table)), [], |r| r.get(0))?;
        Ok(ReplacePreview {
            matches: count,
            sample: snap.run_query(
                &format!("SELECT {} AS before,{expr} AS after FROM {} WHERE {cond}", ident(&spec.column), ident(&spec.table)),
                &[],
                50,
            )?,
        })
    })
}
impl Workbook {
    pub fn analyze(&self, spec: &AnalysisSpec) -> Result<AnalysisResult> {
        analyze(&self.db(), spec)
    }
    pub fn analyses(&self) -> Result<Vec<SavedAnalysis>> {
        analyses(&self.conn)
    }
    pub fn presentation(&self, table: &str) -> Result<Presentation> {
        presentation(&self.db(), table)
    }
    pub fn replace_preview(&self, spec: &ReplaceSpec) -> Result<ReplacePreview> {
        replace_preview(&self.db(), spec)
    }
    pub fn save_analysis(&mut self, id: Option<&str>, name: &str, spec: AnalysisSpec, chart: bool) -> Result<SavedAnalysis> {
        self.mutating("save_analysis", None, |wb| {
            if name.trim().is_empty() || name.len() > 200 {
                bail!("analysis name must be 1–200 bytes");
            }
            compile(&wb.db(), &spec)?;
            if chart && !matches!(spec, AnalysisSpec::Summary { .. }) {
                bail!("charts require a summary, not raw joined rows");
            }
            let mut all = wb.analyses()?;
            let i = if let Some(id) = id {
                Some(all.iter().position(|a| a.id == id).ok_or_else(|| anyhow!("unknown analysis"))?)
            } else {
                None
            };
            if i.is_none() && all.len() >= 128 {
                bail!("at most 128 saved analyses");
            }
            let id = if let Some(id) = id {
                id.to_string()
            } else {
                let next = storage::meta_get(&wb.conn, "analysis_seq")?
                    .unwrap_or_else(|| "0".into())
                    .parse::<u64>()?
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("analysis id limit"))?;
                storage::meta_set(&wb.conn, "analysis_seq", &next.to_string())?;
                format!("a{next}")
            };
            let a = SavedAnalysis { id, name: name.trim().into(), spec, chart };
            if let Some(i) = i {
                all[i] = a.clone();
            } else {
                all.push(a.clone());
            }
            save_meta(&wb.conn, ANALYSES, &all)?;
            Ok(a)
        })
    }
    pub fn delete_analysis(&mut self, id: &str) -> Result<()> {
        self.mutating("delete_analysis", None, |wb| {
            let mut all = wb.analyses()?;
            if !all.iter().any(|a| a.id == id) {
                bail!("unknown analysis");
            }
            all.retain(|a| a.id != id);
            save_meta(&wb.conn, ANALYSES, &all)
        })
    }
    pub fn materialize_analysis(&mut self, name: &str, spec: &AnalysisSpec) -> Result<TableInfo> {
        self.mutating("materialize_analysis", None, |wb| {
            let (sql, _) = compile(&wb.db(), spec)?;
            if wb.run_query(&sql, &[], COPY_ROWS)?.truncated {
                bail!("analysis exceeds the 10000-row snapshot limit; summarize it first");
            }
            wb.create_table_from_query(name, &sql)
        })
    }
    pub fn replace_apply(&mut self, spec: &ReplaceSpec, revision: u64) -> Result<ReplaceOutcome> {
        self.mutating("replace_apply", None, |wb| {
            if wb.revision()? != revision {
                bail!("workbook changed; preview replacement again");
            }
            let (cond, expr) = replace_parts(&wb.db(), spec)?;
            let mut stmt = wb.conn.prepare(&format!(
                "SELECT {},{} FROM {} WHERE {cond} LIMIT {}",
                storage::ID,
                ident(&spec.column),
                ident(&spec.table),
                COPY_ROWS + 1
            ))?;
            let mut cells = Vec::new();
            let mut before_bytes = 0usize;
            let mut after_bytes = 0usize;
            let mut cursor = stmt.query([])?;
            while let Some(row) = cursor.next()? {
                let value: String = row.get(1)?;
                before_bytes = before_bytes.saturating_add(value.len());
                let matches = value.matches(&spec.find).count();
                let after = value
                    .len()
                    .saturating_sub(matches.saturating_mul(spec.find.len()))
                    .saturating_add(matches.saturating_mul(spec.replacement.len()));
                after_bytes = after_bytes.saturating_add(after);
                if before_bytes > 4 << 20 || after_bytes > 4 << 20 {
                    bail!("replacement values exceed the 4 MiB undo/result limit");
                }
                cells.push((row.get::<_, i64>(0)?, value));
            }
            drop(cursor);
            if cells.len() > COPY_ROWS {
                bail!("replace is limited to 10000 affected cells");
            }
            drop(stmt);
            let rows = cells.len() as u64;
            let undo_revision = revision + 1;
            save_meta(&wb.conn, UNDO, &ReplacementUndo { spec: spec.clone(), revision: undo_revision, cells })?;
            wb.sql_change(&format!("UPDATE {} SET {}={expr} WHERE {cond}", ident(&spec.table), ident(&spec.column)), &[])?;
            Ok(ReplaceOutcome { table: spec.table.clone(), rows, undo_revision })
        })
    }
    pub fn replace_undo(&mut self, revision: u64) -> Result<ReplaceOutcome> {
        self.mutating("replace_undo", None, |wb| {
            let undo: ReplacementUndo =
                serde_json::from_str(&storage::meta_get(&wb.conn, UNDO)?.ok_or_else(|| anyhow!("no replacement to undo"))?)?;
            if undo.revision != revision || wb.revision()? != revision {
                bail!("workbook changed since replacement; undo refused to protect later edits");
            }
            replace_parts(&wb.db(), &undo.spec)?;
            let sql = format!("UPDATE {} SET {}=? WHERE {}=?", ident(&undo.spec.table), ident(&undo.spec.column), storage::ID);
            let mut stmt = wb.conn.prepare(&sql)?;
            for (id, value) in &undo.cells {
                stmt.execute(duckdb::params![value, id])?;
            }
            storage::update_table(&wb.conn, &undo.spec.table, None, None)?;
            wb.conn.execute("DELETE FROM _rf_meta WHERE key=?", [UNDO])?;
            Ok(ReplaceOutcome { table: undo.spec.table, rows: undo.cells.len() as u64, undo_revision: 0 })
        })
    }
    pub fn save_presentation(&mut self, table: &str, p: Presentation) -> Result<Presentation> {
        self.mutating("save_presentation", None, |wb| {
            names(&wb.db(), table, &p.hidden, 1000)?;
            let cols = wb.columns(table)?;
            if p.hidden.len() >= cols.len() || p.frozen > 8 || p.rules.len() > 16 || p.formats.len() > 100 {
                bail!("keep a visible column; at most 8 frozen columns, 16 rules and 100 formats");
            }
            for (name, f) in &p.formats {
                let c = column(&wb.db(), table, name)?;
                if matches!(f.kind, DisplayKind::Number | DisplayKind::Percent) && !c.kind.is_numeric() {
                    bail!("numeric format requires a numeric column");
                }
                if matches!(f.kind, DisplayKind::Date | DisplayKind::Datetime)
                    && !matches!(c.kind, crate::Kind::Date | crate::Kind::Datetime)
                {
                    bail!("date format requires a date or timestamp column");
                }
                if f.decimals > 12
                    || !["en-US", "en-GB", "de-DE", "fr-FR", "hi-IN", "ja-JP"].contains(&f.locale.as_str())
                    || !["UTC", "Asia/Kolkata", "America/New_York", "Europe/London", "Asia/Tokyo"].contains(&f.timezone.as_str())
                {
                    bail!("unsupported locale, timezone or precision");
                }
            }
            for r in &p.rules {
                let c = column(&wb.db(), table, &r.column)?;
                if !matches!(r.condition, Condition::Empty) && !c.kind.is_numeric() {
                    bail!("numeric conditional rules require a numeric column");
                }
                if !r.value.is_finite() || !["red", "green", "amber"].contains(&r.color.as_str()) {
                    bail!("invalid conditional format");
                }
            }
            let mut all: BTreeMap<String, Presentation> = read_meta(&wb.conn, PRESENTATIONS)?;
            all.insert(table.into(), p.clone());
            save_meta(&wb.conn, PRESENTATIONS, &all)?;
            Ok(p)
        })
    }
}
