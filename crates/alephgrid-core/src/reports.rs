//! Pivot reports, charts and dashboards (F.11), extending phase 6 summaries.
//!
//! A report groups a table (its formula columns included) by up to four columns and computes
//! up to eight measures, optionally pivoted by the explicit values of one column. Calculated
//! measures are table formulas over the measures (`=[@revenue]/[@orders]`). With `totals`,
//! `ROLLUP` adds a subtotal row per group level and a grand total, marked in `row_kind`.
//! Filters narrow the rows first; dashboards add shared slicers to every report of the same
//! table. Results are bounded like every analysis (`OUTPUT_ROWS`); drill-down returns a view
//! spec of the rows behind a result row, which is read page by page like any view.

use std::collections::BTreeSet;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::analysis::{column, names, read_meta, save_meta, Aggregate, AnalysisResult, AnalysisSpec, Pivot, SavedAnalysis};
use crate::reads::{filter_sql, Db};
use crate::sql::{ident, lit};
use crate::{Filter, FilterOp, ViewSpec, Workbook};

pub const MAX_MEASURES: usize = 8;
pub const MAX_CALCULATED: usize = 4;
pub const MAX_FILTERS: usize = 8;
pub const MAX_DASHBOARDS: usize = 32;
pub const MAX_TILES: usize = 12;
pub const MAX_SLICERS: usize = 6;
pub const MAX_SLICER_VALUES: usize = 24;
/// Dashboard grid: tiles are placed on 12 columns by 24 rows.
pub const GRID_COLUMNS: u32 = 12;
pub const GRID_ROWS: u32 = 24;
const DASHBOARDS: &str = "dashboards";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Measure {
    pub aggregate: Aggregate,
    #[serde(default)]
    #[ts(optional)]
    pub value: Option<String>,
    pub name: String,
}

/// A measure computed from the others with a table formula, e.g. `=[@revenue]/[@orders]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Calculated {
    pub name: String,
    pub formula: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ChartKind {
    #[default]
    Bar,
    Line,
    Scatter,
}

/// A dashboard filter: rows of `table` whose `column` is one of `values` (None: blank).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Slicer {
    pub table: String,
    pub column: String,
    pub values: Vec<Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Tile {
    pub analysis: String,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Dashboard {
    pub id: String,
    pub name: String,
    pub tiles: Vec<Tile>,
    #[serde(default)]
    pub slicers: Vec<Slicer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct TileResult {
    pub tile: Tile,
    #[ts(optional)]
    pub analysis: Option<SavedAnalysis>,
    #[ts(optional)]
    pub result: Option<AnalysisResult>,
    /// Why the tile shows nothing (a deleted analysis, a renamed column, …).
    #[ts(optional)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DashboardResult {
    pub dashboard: Dashboard,
    pub tiles: Vec<TileResult>,
}

fn aggregate_sql(db: &Db, table: &str, m: &Measure) -> Result<String> {
    Ok(match m.aggregate {
        Aggregate::Count => match &m.value {
            None => "count(*)".into(),
            Some(v) => { column(db, table, v)?; format!("count({})", ident(v)) }
        },
        a => {
            let name = m.value.as_deref().ok_or_else(|| anyhow!("measure {:?} needs a value column", m.name))?;
            let c = column(db, table, name)?;
            if matches!(a, Aggregate::Sum | Aggregate::Average) && !c.kind.is_numeric() {
                bail!("sum and average require a numeric column ({:?} is {})", name, c.sql_type);
            }
            let op = match a { Aggregate::Sum => "sum", Aggregate::Average => "avg", Aggregate::Min => "min", _ => "max" };
            format!("{op}({})", ident(name))
        }
    })
}

/// The SQL of a report (see the module documentation), with dashboard `slicers` applied.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compile_report(db: &Db, table: &str, groups: &[String], measures: &[Measure], calculated: &[Calculated], totals: bool, filters: &[Filter], pivot: Option<&Pivot>, slicers: &[Slicer]) -> Result<String> {
    let all = db.columns(table)?;
    names(db, table, groups, 4)?;
    if measures.is_empty() || measures.len() > MAX_MEASURES {
        bail!("a report needs 1–{MAX_MEASURES} measures");
    }
    if calculated.len() > MAX_CALCULATED {
        bail!("at most {MAX_CALCULATED} calculated measures");
    }
    if filters.len() > MAX_FILTERS {
        bail!("at most {MAX_FILTERS} filters");
    }
    let mut seen = BTreeSet::new();
    for n in measures.iter().map(|m| &m.name).chain(calculated.iter().map(|c| &c.name)).chain(groups.iter()) {
        if n.trim().is_empty() || n.len() > 64 || !seen.insert(n.to_lowercase()) || n.eq_ignore_ascii_case("row_kind") {
            bail!("measure and group names must be distinct, 1–64 bytes, and not row_kind ({n:?})");
        }
    }
    if pivot.is_some() && !calculated.is_empty() {
        bail!("calculated measures are not available with a pivot column; drop the pivot or the calculated measures");
    }
    // Rows: the table with its formula columns, filtered, then sliced.
    let mut conditions = Vec::new();
    for f in filters {
        let c = all.iter().find(|c| c.name == f.column).ok_or_else(|| anyhow!("unknown column {:?} in {table}", f.column))?;
        conditions.push(filter_sql(c, f)?);
    }
    for s in slicers.iter().filter(|s| s.table.eq_ignore_ascii_case(table)) {
        column(db, table, &s.column)?;
        let c = ident(&s.column);
        let mut any: Vec<String> = s.values.iter().flatten().map(|v| format!("CAST({c} AS VARCHAR) = {}", lit(v))).collect();
        if s.values.iter().any(|v| v.is_none()) {
            any.push(format!("{c} IS NULL"));
        }
        conditions.push(if any.is_empty() { "FALSE".into() } else { format!("({})", any.join(" OR ")) });
    }
    let source = format!(
        "(SELECT * FROM ({}){}) t",
        db.base_select(table)?,
        if conditions.is_empty() { String::new() } else { format!(" WHERE {}", conditions.join(" AND ")) }
    );
    let mut select: Vec<String> = groups.iter().map(|g| ident(g)).collect();
    for m in measures {
        let agg = aggregate_sql(db, table, m)?;
        match pivot {
            None => select.push(format!("{agg} AS {}", ident(&m.name))),
            Some(p) => {
                column(db, table, &p.column)?;
                if p.values.is_empty() || p.values.len() * measures.len() > 64 {
                    bail!("a pivot needs 1 or more explicit values, and at most 64 pivoted columns in all");
                }
                for v in &p.values {
                    let cond = match v {
                        Some(v) => format!("CAST({} AS VARCHAR) = {}", ident(&p.column), lit(v)),
                        None => format!("{} IS NULL", ident(&p.column)),
                    };
                    select.push(format!("{agg} FILTER (WHERE {cond}) AS {}", ident(&format!("{}: {}", m.name, v.as_deref().unwrap_or("(blank)")))));
                }
            }
        }
    }
    let keys: Vec<String> = groups.iter().map(|g| ident(g)).collect();
    let (group_by, kind, order) = if groups.is_empty() {
        (String::new(), "'total'".to_string(), String::new())
    } else if totals {
        let all_bits = (1u32 << groups.len()) - 1;
        let grouping = format!("grouping({})", keys.join(", "));
        (
            format!(" GROUP BY ROLLUP({})", keys.join(", ")),
            format!("CASE {grouping} WHEN 0 THEN 'row' WHEN {all_bits} THEN 'total' ELSE 'subtotal' END"),
            format!(" ORDER BY {}", keys.iter().map(|k| format!("grouping({k}), {k} NULLS LAST")).collect::<Vec<_>>().join(", ")),
        )
    } else {
        (format!(" GROUP BY {}", keys.join(", ")), "'row'".to_string(), format!(" ORDER BY {}", keys.iter().map(|k| format!("{k} NULLS LAST")).collect::<Vec<_>>().join(", ")))
    };
    select.push(format!("{kind} AS row_kind"));
    let mut sql = format!("SELECT {} FROM {source}{group_by}{order}", select.join(", "));
    // Calculated measures: table formulas over the measures, as numbers.
    if !calculated.is_empty() {
        let cols: Vec<(String, String)> = measures.iter().map(|m| (m.name.clone(), "DOUBLE".to_string())).collect();
        let mut out: Vec<String> = keys.clone();
        out.extend(measures.iter().map(|m| format!("CAST({n} AS DOUBLE) AS {n}", n = ident(&m.name))));
        out.push("row_kind".into());
        out.push("row_number() OVER () AS _rf_ord".into());
        let mut base = format!("SELECT {} FROM ({sql})", out.join(", "));
        let mut extra = Vec::new();
        for (k, c) in calculated.iter().enumerate() {
            let compiled = crate::table_formula::compile(&c.formula, &cols, &format!("_rf_m{k}_")).map_err(|e| anyhow!("calculated measure {:?}: {e}", c.name))?;
            let temps: Vec<String> = compiled.temps().iter().map(|t| ident(t)).collect();
            let exclude = if temps.is_empty() { String::new() } else { format!(" EXCLUDE ({})", temps.join(", ")) };
            base = format!("SELECT *{exclude}, CASE WHEN {} IS NULL THEN {} END AS {} FROM ({})", compiled.error, compiled.value, ident(&c.name), compiled.wrap(&base));
            extra.push(ident(&c.name));
        }
        let mut cols_out: Vec<String> = keys.iter().cloned().chain(measures.iter().map(|m| ident(&m.name))).collect();
        cols_out.extend(extra);
        cols_out.push("row_kind".into());
        sql = format!("SELECT {} FROM ({base}) ORDER BY _rf_ord", cols_out.join(", "));
    }
    Ok(sql)
}

/// The rows behind one result row of a summary or report: a view spec of its table filtered by
/// the row's group values (subtotal and total rows leave out the levels they add up).
pub fn drill(spec: &AnalysisSpec, row: &[Value]) -> Result<ViewSpec> {
    let (table, groups, filters) = match spec {
        AnalysisSpec::Summary { table, groups, .. } => (table, groups, Vec::new()),
        AnalysisSpec::Report { table, groups, filters, .. } => (table, groups, filters.clone()),
        _ => bail!("only summaries and reports can be drilled into"),
    };
    let kind = row.last().and_then(|v| v.as_str());
    let mut filters = filters;
    for (g, v) in groups.iter().zip(row) {
        // ROLLUP leaves the levels a subtotal adds up blank.
        if v.is_null() && matches!(kind, Some("subtotal" | "total")) {
            continue;
        }
        filters.push(if v.is_null() { Filter { column: g.clone(), op: FilterOp::IsEmpty, value: Value::Null } } else { Filter { column: g.clone(), op: FilterOp::Eq, value: v.clone() } });
    }
    Ok(ViewSpec { table: table.clone(), filters, ..Default::default() })
}

fn dashboards(conn: &duckdb::Connection) -> Result<Vec<Dashboard>> {
    read_meta(conn, DASHBOARDS)
}

fn check_dashboard(d: &Dashboard, analyses: &[SavedAnalysis]) -> Result<()> {
    if d.name.trim().is_empty() || d.name.len() > 200 {
        bail!("dashboard name must be 1–200 bytes");
    }
    if d.tiles.len() > MAX_TILES || d.slicers.len() > MAX_SLICERS {
        bail!("a dashboard holds at most {MAX_TILES} tiles and {MAX_SLICERS} slicers");
    }
    for (k, t) in d.tiles.iter().enumerate() {
        if !analyses.iter().any(|a| a.id == t.analysis) {
            bail!("unknown analysis {:?}", t.analysis);
        }
        if t.w == 0 || t.h == 0 || t.x + t.w > GRID_COLUMNS || t.y + t.h > GRID_ROWS {
            bail!("tile {} does not fit the {GRID_COLUMNS}×{GRID_ROWS} grid", k + 1);
        }
        for (j, o) in d.tiles.iter().enumerate().take(k) {
            if t.x < o.x + o.w && o.x < t.x + t.w && t.y < o.y + o.h && o.y < t.y + t.h {
                bail!("tiles {} and {} overlap", j + 1, k + 1);
            }
        }
    }
    for s in &d.slicers {
        if s.values.is_empty() || s.values.len() > MAX_SLICER_VALUES || s.values.iter().flatten().any(|v| v.len() > 1024) {
            bail!("a slicer selects 1–{MAX_SLICER_VALUES} values of at most 1024 bytes");
        }
    }
    Ok(())
}

impl Workbook {
    pub fn dashboards(&self) -> Result<Vec<Dashboard>> {
        dashboards(&self.conn)
    }

    /// Create (`id` None) or replace a dashboard; tiles must fit the grid without overlapping.
    pub fn save_dashboard(&mut self, id: Option<&str>, name: &str, tiles: Vec<Tile>, slicers: Vec<Slicer>) -> Result<Dashboard> {
        self.mutating("save_dashboard", None, |wb| {
            let analyses = wb.analyses()?;
            let mut all = dashboards(&wb.conn)?;
            let id = match id {
                Some(id) if all.iter().any(|d| d.id == id) => id.to_string(),
                Some(_) => bail!("unknown dashboard"),
                None => {
                    if all.len() >= MAX_DASHBOARDS {
                        bail!("at most {MAX_DASHBOARDS} dashboards");
                    }
                    let next = crate::storage::meta_get(&wb.conn, "dashboard_seq")?.unwrap_or_else(|| "0".into()).parse::<u64>()? + 1;
                    crate::storage::meta_set(&wb.conn, "dashboard_seq", &next.to_string())?;
                    format!("d{next}")
                }
            };
            let d = Dashboard { id: id.clone(), name: name.trim().into(), tiles, slicers };
            check_dashboard(&d, &analyses)?;
            for s in &d.slicers {
                column(&wb.db(), &s.table, &s.column)?;
            }
            all.retain(|x| x.id != id);
            all.push(d.clone());
            save_meta(&wb.conn, DASHBOARDS, &all)?;
            Ok(d)
        })
    }

    pub fn delete_dashboard(&mut self, id: &str) -> Result<()> {
        self.mutating("delete_dashboard", None, |wb| {
            let mut all = dashboards(&wb.conn)?;
            let before = all.len();
            all.retain(|d| d.id != id);
            if all.len() == before {
                bail!("unknown dashboard");
            }
            save_meta(&wb.conn, DASHBOARDS, &all)
        })
    }

    pub fn dashboard_results(&self, id: &str) -> Result<DashboardResult> {
        dashboard_results(&self.db(), &self.conn, id)
    }
}

impl crate::Reader {
    pub fn dashboards(&self) -> Result<Vec<Dashboard>> {
        dashboards(self.connection())
    }

    pub fn dashboard_results(&self, id: &str) -> Result<DashboardResult> {
        dashboard_results(&self.db_for_reports(), self.connection(), id)
    }

    pub fn drill(&self, spec: &AnalysisSpec, row: &[Value]) -> Result<ViewSpec> {
        drill(spec, row)
    }
}

/// Every tile's result, with the dashboard's slicers applied to reports of their table. A
/// tile that cannot be computed says why instead of failing the dashboard.
fn dashboard_results(db: &Db, conn: &duckdb::Connection, id: &str) -> Result<DashboardResult> {
    let dashboard = dashboards(conn)?.into_iter().find(|d| d.id == id).ok_or_else(|| anyhow!("unknown dashboard"))?;
    let analyses = crate::analysis::analyses(conn)?;
    let mut tiles = Vec::new();
    for t in &dashboard.tiles {
        let Some(a) = analyses.iter().find(|a| a.id == t.analysis).cloned() else {
            tiles.push(TileResult { tile: t.clone(), analysis: None, result: None, error: Some("this analysis was deleted".into()) });
            continue;
        };
        let out = match &a.spec {
            AnalysisSpec::Report { table, groups, measures, calculated, totals, filters, pivot } => {
                db.snapshot(|| {
                    let snap = Db { conn: db.conn, shared: db.shared, in_txn: true, keep: Default::default(), built: Default::default(), in_snapshot: Default::default() };
                    let sql = compile_report(&snap, table, groups, measures, calculated, *totals, filters, pivot.as_ref(), &dashboard.slicers)?;
                    Ok(AnalysisResult { result: snap.run_query(&sql, &[], crate::analysis::OUTPUT_ROWS)?, diagnostics: None })
                })
            }
            spec => crate::analysis::analyze(db, spec),
        };
        match out {
            Ok(r) => tiles.push(TileResult { tile: t.clone(), analysis: Some(a), result: Some(r), error: None }),
            Err(e) => tiles.push(TileResult { tile: t.clone(), analysis: Some(a), result: None, error: Some(format!("{e:#}")) }),
        }
    }
    Ok(DashboardResult { dashboard, tiles })
}
