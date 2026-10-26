//! Read paths shared by the workbook's writer connection and its reader connections.
//!
//! A read runs in one DuckDB snapshot: it ensures the view's materialization and the
//! table's position index (built in their own short transactions when stale), then opens a
//! snapshot, checks that the table generation in that snapshot still matches what it is
//! about to use, and reads. A structural change committed in between makes the check
//! fail and the read starts over, so a reply never mixes an old order with new rows.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use duckdb::Connection;
use serde_json::Value;

use crate::catalog::{Materialized, PosIndex, Shared, ViewEntry};
use crate::limits;
use crate::model::*;
use crate::sql::{formula_to_sql, ident, lit};
use crate::storage::{self, ID, ORD};
use crate::wire::{self, Codec};

/// Internal columns never shown to users.
pub(crate) fn is_internal(name: &str) -> bool {
    name == ID || name == ORD
}

pub(crate) struct Db<'a> {
    pub conn: &'a Connection,
    pub shared: &'a Shared,
    /// Inside the writer's transaction: no snapshots of our own, no caching of what is
    /// read (it may be rolled back), no scratch writes (a transaction writes one database).
    pub in_txn: bool,
}

impl Db<'_> {
    /// Run `f` in one snapshot (a read transaction), or in the caller's transaction.
    pub fn snapshot<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        if self.in_txn {
            return f();
        }
        self.conn.execute_batch("BEGIN TRANSACTION;")?;
        match f() {
            Ok(v) => {
                self.conn.execute_batch("COMMIT;")?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    // ------------------------------------------------------------- schema

    pub fn table_exists(&self, name: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            &format!(
                "SELECT count(*) FROM duckdb_tables() WHERE schema_name = 'main' AND database_name = current_database() AND table_name = {}",
                lit(name)
            ),
            [],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn count_rows(&self, table: &str) -> Result<u64> {
        let n: i64 = self.conn.query_row(&format!("SELECT count(*) FROM main.{}", ident(table)), [], |r| r.get(0))?;
        Ok(n as u64)
    }

    pub fn formulas(&self, table: &str) -> Result<Vec<(String, String)>> {
        Ok(self
            .conn
            .prepare(&format!("SELECT name, formula FROM _rf_columns WHERE table_name = {} ORDER BY position", lit(table)))?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?)
    }

    /// `SELECT _rf_id, _rf_ord, <data columns>, <formula columns> FROM table`.
    pub fn base_select(&self, table: &str) -> Result<String> {
        // Qualified, so a CTE that shadows the table (see `expand_query`) can read it.
        let mut select = format!("SELECT * FROM main.{}", ident(table));
        for (name, formula) in self.formulas(table)? {
            select = format!("SELECT *, ({}) AS {} FROM ({select})", formula_to_sql(&formula), ident(&name));
        }
        Ok(select)
    }

    pub fn describe(&self, select: &str) -> Result<Vec<Column>> {
        let cols: Vec<(String, String)> = self
            .conn
            .prepare(&format!("DESCRIBE {select}"))?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(cols
            .into_iter()
            .map(|(name, sql_type)| Column { kind: Kind::from_sql_type(&sql_type), name, sql_type, formula: None })
            .collect())
    }

    pub fn columns(&self, table: &str) -> Result<Vec<Column>> {
        let formulas: HashMap<String, String> = self.formulas(table)?.into_iter().collect();
        let mut cols = self.describe(&self.base_select(table)?)?;
        cols.retain(|c| !is_internal(&c.name));
        for c in &mut cols {
            c.formula = formulas.get(&c.name).cloned();
        }
        Ok(cols)
    }

    pub fn list_tables(&self) -> Result<Vec<TableInfo>> {
        self.snapshot(|| {
            storage::user_tables(self.conn)?
                .into_iter()
                .map(|name| self.table_info(&name))
                .collect()
        })
    }

    pub fn table_info(&self, table: &str) -> Result<TableInfo> {
        Ok(TableInfo { name: table.to_string(), row_count: self.count_rows(table)?, column_count: self.columns(table)?.len() })
    }

    // -------------------------------------------------------------- views

    /// Check a spec against the table without building anything.
    pub fn validate_spec(&self, spec: &ViewSpec) -> Result<Vec<Column>> {
        let all = self.columns(&spec.table)?;
        for h in &spec.hidden {
            if !all.iter().any(|c| &c.name == h) {
                bail!("unknown column {h:?}");
            }
        }
        if all.iter().all(|c| spec.hidden.contains(&c.name)) && !all.is_empty() {
            bail!("a view must show at least one column");
        }
        let by_name: HashMap<&str, &Column> = all.iter().map(|c| (c.name.as_str(), c)).collect();
        for f in &spec.filters {
            let col = by_name.get(f.column.as_str()).ok_or_else(|| anyhow!("unknown column {:?}", f.column))?;
            filter_sql(col, f)?;
        }
        for k in &spec.sort {
            if !by_name.contains_key(k.column.as_str()) {
                bail!("unknown column {:?}", k.column);
            }
        }
        Ok(all)
    }

    /// The table's position index as of this snapshot.
    pub fn index(&self, table: &str) -> Result<Arc<PosIndex>> {
        let meta = storage::table_meta(self.conn, table)?;
        if let Some(i) = self.shared.cached_index(table, meta.gen) {
            return Ok(i);
        }
        let built = PosIndex::build(self.conn, table, meta.gen, meta.dense)?;
        if !self.in_txn {
            self.shared.store_index(table, built.clone());
        }
        Ok(Arc::new(built))
    }

    /// The view as of the current table generation, building it if needed. Sorted or
    /// filtered views write a permutation to the scratch database, so inside the writer's
    /// transaction they must already be built (see `Workbook::mutate`).
    pub fn materialize(&self, entry: &ViewEntry) -> Result<Arc<Materialized>> {
        let spec = entry.spec();
        let gen = storage::table_meta(self.conn, &spec.table)?.gen;
        let mut slot = entry.mat.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = slot.as_ref().filter(|m| m.gen == gen && m.spec == spec) {
            return Ok(m.clone());
        }
        let all = self.validate_spec(&spec)?;
        let columns: Vec<Column> = all.iter().filter(|c| !spec.hidden.contains(&c.name)).cloned().collect();
        let built = if spec.is_identity() {
            let row_count = self.count_rows(&spec.table)?;
            Materialized { gen, spec: spec.clone(), columns, row_count, perm: None }
        } else {
            if self.in_txn {
                bail!("view {} changed during this operation; try again", entry.id);
            }
            let by_name: HashMap<&str, &Column> = all.iter().map(|c| (c.name.as_str(), c)).collect();
            let mut conditions = Vec::new();
            for f in &spec.filters {
                conditions.push(filter_sql(by_name[f.column.as_str()], f)?);
            }
            let mut order: Vec<String> = spec
                .sort
                .iter()
                .map(|k| format!("{} {} NULLS LAST", ident(&k.column), if k.desc { "DESC" } else { "ASC" }))
                .collect();
            // Ties keep display order.
            order.push(ORD.to_string());
            let where_sql = if conditions.is_empty() { String::new() } else { format!("WHERE {}", conditions.join(" AND ")) };
            let perm = self.shared.new_scratch(&format!("v{}", entry.id));
            // Build from one snapshot and stamp it with that snapshot's generation.
            let (row_count, gen) = self.snapshot(|| {
                let gen = storage::table_meta(self.conn, &spec.table)?.gen;
                self.conn.execute_batch(&format!(
                    "CREATE TABLE {} AS
                     SELECT (row_number() OVER () - 1)::BIGINT AS _rf_pos, {ID}
                     FROM (SELECT {ID} FROM ({}) {where_sql} ORDER BY {});",
                    perm.qualified(),
                    self.base_select(&spec.table)?,
                    order.join(", ")
                ))?;
                let n: i64 = self.conn.query_row(&format!("SELECT count(*) FROM {}", perm.qualified()), [], |r| r.get(0))?;
                Ok((n as u64, gen))
            })?;
            Materialized { gen, spec: spec.clone(), columns, row_count, perm: Some(Arc::new(perm)) }
        };
        let built = Arc::new(built);
        if !self.in_txn {
            *slot = Some(built.clone());
        }
        Ok(built)
    }

    /// Run `f` with the view's materialization in a snapshot that matches it.
    pub fn with_view<T>(&self, id: u64, mut f: impl FnMut(&Materialized) -> Result<T>) -> Result<T> {
        let entry = self.shared.view(id)?;
        for _ in 0..8 {
            let m = self.materialize(&entry)?;
            let out = self.snapshot(|| {
                if storage::table_meta(self.conn, &m.spec.table)?.gen != m.gen || entry.spec() != m.spec {
                    return Ok(None);
                }
                f(&m).map(Some)
            })?;
            if let Some(v) = out {
                return Ok(v);
            }
        }
        bail!("the table kept changing while it was being read; try again")
    }

    pub fn view_info(&self, id: u64) -> Result<ViewInfo> {
        self.with_view(id, |m| {
            Ok(ViewInfo {
                view_id: id,
                spec: m.spec.clone(),
                row_count: m.row_count,
                columns: m.columns.clone(),
                can_append: m.perm.is_none(),
            })
        })
    }

    /// Record ids for view positions `[start, end)`, in view order, with their order keys
    /// for identity views.
    pub fn ids_for_range(&self, m: &Materialized, start: u64, end: u64) -> Result<Vec<i64>> {
        let end = end.min(m.row_count);
        if start >= end {
            return Ok(vec![]);
        }
        let sql = match &m.perm {
            None => {
                let cond = self.index(&m.spec.table)?.range_condition(self.conn, &m.spec.table, start, end)?;
                format!("SELECT {ID} FROM main.{} WHERE {cond} ORDER BY {ORD}", ident(&m.spec.table))
            }
            Some(p) => format!("SELECT {ID} FROM {} WHERE _rf_pos >= {start} AND _rf_pos < {end} ORDER BY _rf_pos", p.qualified()),
        };
        Ok(self.conn.prepare(&sql)?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)
    }

    /// SQL condition selecting the rows at view positions `[start, end)`.
    pub fn range_condition(&self, m: &Materialized, start: u64, end: u64) -> Result<String> {
        match &m.perm {
            None => self.index(&m.spec.table)?.range_condition(self.conn, &m.spec.table, start, end),
            Some(p) => Ok(format!("{ID} IN (SELECT {ID} FROM {} WHERE _rf_pos >= {start} AND _rf_pos < {end})", p.qualified())),
        }
    }

    /// Rows `[row_start, row_start + row_count)` of a view, columns
    /// `[column_start, column_start + column_count)` of its visible columns. Both ranges
    /// are clamped before anything is allocated, and the reply stops at
    /// [`limits::MAX_REPLY_BYTES`] (always at least one row).
    pub fn fetch(&self, id: u64, row_start: u64, row_count: u64, column_start: u32, column_count: Option<u32>) -> Result<RowBlock> {
        self.with_view(id, |m| {
            let total_cols = m.columns.len() as u32;
            let c0 = column_start.min(total_cols);
            let c1 = c0.saturating_add(column_count.unwrap_or(total_cols)).min(total_cols);
            let cols = &m.columns[c0 as usize..c1 as usize];
            let rows_allowed = limits::fetch_rows(row_count, cols.len());
            let end = row_start.saturating_add(rows_allowed).min(m.row_count);
            let empty = RowBlock { row_start, rows: vec![], ids: vec![], column_start: c0, truncated: false };
            if row_start >= end {
                return Ok(empty);
            }
            let projection: Vec<String> = cols.iter().map(|c| wire::projection(c, &ident(&c.name))).collect();
            let codecs: Vec<Codec> = cols.iter().map(|c| wire::codec(&c.sql_type)).collect();
            let projection = if projection.is_empty() { String::new() } else { format!(", {}", projection.join(", ")) };
            let base = self.base_select(&m.spec.table)?;
            // Identity views: one ordered range scan. Permutations: ids first, then rows by id.
            let (ids, sql) = match &m.perm {
                None => {
                    let cond = self.range_condition(m, row_start, end)?;
                    (None, format!("SELECT {ID}{projection} FROM ({base}) WHERE {cond} ORDER BY {ORD}"))
                }
                Some(_) => {
                    let ids = self.ids_for_range(m, row_start, end)?;
                    let list = ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
                    (Some(ids), format!("SELECT {ID}{projection} FROM ({base}) WHERE {ID} IN ({list})"))
                }
            };
            let mut stmt = self.conn.prepare(&sql)?;
            let mut rows = stmt.query([])?;
            let mut by_id: HashMap<i64, Vec<Value>> = HashMap::new();
            let mut in_order: Vec<i64> = Vec::new();
            while let Some(row) = rows.next()? {
                let rid: i64 = row.get(0)?;
                let mut values = Vec::with_capacity(cols.len());
                for (i, codec) in codecs.iter().enumerate() {
                    values.push(wire::decode(*codec, row.get_ref(i + 1)?));
                }
                by_id.insert(rid, values);
                in_order.push(rid);
            }
            let ids = ids.unwrap_or(in_order);
            if ids.len() as u64 != end - row_start {
                bail!("expected {} rows at {row_start}, read {}; the table changed during the read", end - row_start, ids.len());
            }
            let mut out = Vec::with_capacity(ids.len());
            let mut out_ids = Vec::with_capacity(ids.len());
            let mut bytes = 0usize;
            for rid in &ids {
                let values = by_id.remove(rid).ok_or_else(|| anyhow!("record {rid} vanished during the read"))?;
                let size: usize = values.iter().map(limits::json_size).sum::<usize>() + 2 * values.len() + 16;
                if !out.is_empty() && bytes + size > limits::MAX_REPLY_BYTES {
                    break;
                }
                bytes += size;
                out.push(values);
                out_ids.push(*rid);
            }
            let truncated = out.len() < ids.len();
            Ok(RowBlock { row_start, rows: out, ids: out_ids, column_start: c0, truncated })
        })
    }

    pub fn stats(&self, id: u64, column: &str, start: u64, end: u64) -> Result<Stats> {
        self.with_view(id, |m| {
            let col = m.columns.iter().find(|c| c.name == column).ok_or_else(|| anyhow!("unknown column {column:?}"))?;
            let end = end.min(m.row_count);
            if start >= end {
                return Ok(Stats::default());
            }
            let condition = if start == 0 && end == m.row_count {
                match &m.perm {
                    None => "TRUE".to_string(),
                    Some(p) => format!("{ID} IN (SELECT {ID} FROM {})", p.qualified()),
                }
            } else {
                self.range_condition(m, start, end)?
            };
            let c = ident(column);
            let base = self.base_select(&m.spec.table)?;
            let codec = wire::codec(&col.sql_type);
            // Sums of integers and decimals stay exact (HUGEINT / DECIMAL(38,s)); doubles stay doubles.
            let (sum, sum_codec) = match (col.kind, codec) {
                (Kind::Int, _) => (format!("sum({c})::VARCHAR"), Codec::IntText),
                (Kind::Number, Codec::ExactText) => (format!("sum({c})::VARCHAR"), Codec::ExactText),
                (Kind::Number, _) => (format!("sum({c})::DOUBLE"), Codec::Native),
                _ => ("NULL::VARCHAR".to_string(), Codec::Native),
            };
            let numeric = col.kind.is_numeric();
            let (min, max) = if numeric {
                (wire::projection(col, &format!("min({c})")), wire::projection(col, &format!("max({c})")))
            } else {
                (format!("min({c})::VARCHAR"), format!("max({c})::VARCHAR"))
            };
            let avg = if numeric { format!("avg({c})::DOUBLE") } else { "NULL::DOUBLE".to_string() };
            let sql = format!("SELECT count(*), count({c}), {sum}, {avg}, {min}, {max} FROM ({base}) WHERE {condition}");
            let mut stmt = self.conn.prepare(&sql)?;
            let mut rows = stmt.query([])?;
            let row = rows.next()?.ok_or_else(|| anyhow!("no stats row"))?;
            let rows_n: i64 = row.get(0)?;
            let count: i64 = row.get(1)?;
            let mm_codec = if numeric { codec } else { Codec::Native };
            let present = |v: Value| Some(v).filter(|v| !v.is_null());
            Ok(Stats {
                rows: rows_n as u64,
                count: count as u64,
                sum: present(wire::decode(sum_codec, row.get_ref(2)?)),
                avg: row.get(3)?,
                min: present(wire::decode(mm_codec, row.get_ref(4)?)),
                max: present(wire::decode(mm_codec, row.get_ref(5)?)),
            })
        })
    }

    /// The view's visible columns in view order, optionally only its first `limit` rows.
    pub fn view_select(&self, m: &Materialized, limit: Option<usize>) -> Result<String> {
        let base = self.base_select(&m.spec.table)?;
        let cols: Vec<String> = m.columns.iter().map(|c| format!("b.{}", ident(&c.name))).collect();
        let cols = cols.join(", ");
        Ok(match &m.perm {
            None => {
                let limit = limit.map(|l| format!("LIMIT {l}")).unwrap_or_default();
                format!("SELECT {cols} FROM ({base}) b ORDER BY b.{ORD} {limit}")
            }
            Some(p) => {
                let limit = limit.map(|l| format!("WHERE p._rf_pos < {l}")).unwrap_or_default();
                format!("SELECT {cols} FROM ({base}) b JOIN {} p USING ({ID}) {limit} ORDER BY p._rf_pos", p.qualified())
            }
        })
    }

    // ------------------------------------------------------------ queries

    /// Expose formula columns to ad-hoc SQL by shadowing each table with a CTE. When the
    /// query has its own `WITH` list, the shadows are prepended to it, and a user CTE with
    /// the same name as a table takes precedence.
    pub fn expand_query(&self, sql: &str) -> Result<String> {
        let sql = sql.trim().trim_end_matches(';').trim_end();
        let mut ctes = Vec::new();
        for name in storage::user_tables(self.conn)? {
            if !self.formulas(&name)?.is_empty() {
                ctes.push((name.clone(), format!("{} AS ({})", ident(&name), self.base_select(&name)?)));
            }
        }
        if ctes.is_empty() {
            return Ok(sql.to_string());
        }
        match crate::sql::leading_with(sql) {
            None => Ok(format!("WITH {} {sql}", ctes.into_iter().map(|(_, c)| c).collect::<Vec<_>>().join(", "))),
            Some(after_with) => {
                let user = self.cte_names(sql);
                let ours: Vec<String> = ctes
                    .into_iter()
                    .filter(|(name, _)| !user.iter().any(|u| crate::formula::same_name(u, name)))
                    .map(|(_, c)| c)
                    .collect();
                if ours.is_empty() {
                    return Ok(sql.to_string());
                }
                Ok(format!("{} {}, {}", &sql[..after_with], ours.join(", "), sql[after_with..].trim_start()))
            }
        }
    }

    /// Names of the top-level CTEs of a query, from DuckDB's parser (empty if it cannot parse).
    fn cte_names(&self, sql: &str) -> Vec<String> {
        let json: Option<String> = self.conn.query_row("SELECT json_serialize_sql(?::VARCHAR)", [sql], |r| r.get(0)).ok();
        let tree: Value = json.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or(Value::Null);
        tree["statements"][0]["node"]["cte_map"]["map"]
            .as_array()
            .map(|m| m.iter().filter_map(|e| e["key"].as_str().map(String::from)).collect())
            .unwrap_or_default()
    }

    pub fn preview_query(&self, sql: &str, limit: usize) -> Result<QueryPreview> {
        self.snapshot(|| {
            let select = self.expand_query(sql)?;
            self.run_preview(&select, limit)
        })
    }

    /// First `limit` rows of a SELECT (at most [`limits::MAX_QUERY_ROWS`] and
    /// [`limits::MAX_REPLY_BYTES`]), in wire form, with truncation reported.
    pub fn run_preview(&self, select: &str, limit: usize) -> Result<QueryPreview> {
        let limit = limit.min(limits::MAX_QUERY_ROWS);
        let described = self.describe(select)?;
        // Address result columns by position, so duplicate or odd names still work.
        let aliases: Vec<String> = (0..described.len()).map(|i| format!("_c{i}")).collect();
        let keep: Vec<usize> = (0..described.len()).filter(|&i| !is_internal(&described[i].name)).collect();
        let columns: Vec<Column> = keep.iter().map(|&i| described[i].clone()).collect();
        let projection: Vec<String> = keep.iter().map(|&i| wire::projection(&described[i], &aliases[i])).collect();
        let codecs: Vec<Codec> = columns.iter().map(|c| wire::codec(&c.sql_type)).collect();
        let sql = format!(
            "SELECT {} FROM ({select}) AS _q({}) LIMIT {}",
            if projection.is_empty() { "NULL".to_string() } else { projection.join(", ") },
            aliases.join(", "),
            limit + 1
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        let mut bytes = 0usize;
        let mut truncated = false;
        while let Some(row) = rows.next()? {
            if out.len() == limit {
                truncated = true;
                break;
            }
            let mut values = Vec::with_capacity(columns.len());
            for (i, codec) in codecs.iter().enumerate() {
                values.push(wire::decode(*codec, row.get_ref(i)?));
            }
            bytes += values.iter().map(limits::json_size).sum::<usize>() + 2 * values.len() + 2;
            if !out.is_empty() && bytes > limits::MAX_REPLY_BYTES {
                truncated = true;
                break;
            }
            out.push(values);
        }
        Ok(QueryPreview { columns, rows: out, truncated })
    }

    pub fn export_view(&self, id: u64, path: &Path, format: ExportFormat) -> Result<()> {
        self.with_view(id, |m| self.copy_to(&self.view_select(m, None)?, path, format))
    }

    pub fn export_table(&self, table: &str, path: &Path, format: ExportFormat) -> Result<()> {
        self.snapshot(|| {
            let select = format!("SELECT * EXCLUDE ({ID}, {ORD}) FROM ({}) ORDER BY {ORD}", self.base_select(table)?);
            self.copy_to(&select, path, format)
        })
    }

    pub fn copy_to(&self, select: &str, path: &Path, format: ExportFormat) -> Result<()> {
        let options = match format {
            ExportFormat::Csv => "FORMAT csv, HEADER",
            ExportFormat::Parquet => "FORMAT parquet, COMPRESSION zstd",
            ExportFormat::Json => "FORMAT json",
        };
        self.conn
            .execute_batch(&format!("COPY ({select}) TO {} ({options});", lit(&path.to_string_lossy())))
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    // ------------------------------------------------------- saved views

    pub fn saved_views(&self, table: Option<&str>) -> Result<Vec<SavedView>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, spec, widths, updated_at FROM _rf_views WHERE ? IS NULL OR table_name = ? ORDER BY lower(name)",
        )?;
        let rows = stmt.query_map(duckdb::params![table, table], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, i64>(4)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, spec, widths, updated_at) = row?;
            out.push(SavedView {
                id,
                name,
                spec: serde_json::from_str(&spec)?,
                widths: serde_json::from_str(&widths)?,
                updated_at: updated_at as u64,
            });
        }
        Ok(out)
    }
}

pub(crate) fn filter_sql(col: &Column, f: &Filter) -> Result<String> {
    let c = ident(&col.name);
    let text = || match &f.value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let rhs = || -> String {
        match col.kind {
            Kind::Int | Kind::Number => format!("TRY_CAST({} AS DOUBLE)", lit(&text())),
            Kind::Bool => format!("TRY_CAST({} AS BOOLEAN)", lit(&text())),
            Kind::Date => format!("TRY_CAST({} AS DATE)", lit(&text())),
            Kind::Datetime => format!("TRY_CAST({} AS TIMESTAMP)", lit(&text())),
            _ => lit(&text()),
        }
    };
    let lhs = if col.kind == Kind::Text { format!("CAST({c} AS VARCHAR)") } else { c.clone() };
    Ok(match f.op {
        FilterOp::Eq => format!("{lhs} = {}", rhs()),
        FilterOp::Neq => format!("({lhs} IS DISTINCT FROM {})", rhs()),
        FilterOp::Gt => format!("{lhs} > {}", rhs()),
        FilterOp::Gte => format!("{lhs} >= {}", rhs()),
        FilterOp::Lt => format!("{lhs} < {}", rhs()),
        FilterOp::Lte => format!("{lhs} <= {}", rhs()),
        FilterOp::Contains => format!("contains(lower(CAST({c} AS VARCHAR)), lower({}))", lit(&text())),
        FilterOp::NotContains => {
            format!("NOT coalesce(contains(lower(CAST({c} AS VARCHAR)), lower({})), false)", lit(&text()))
        }
        FilterOp::StartsWith => format!("starts_with(lower(CAST({c} AS VARCHAR)), lower({}))", lit(&text())),
        FilterOp::IsEmpty => format!("({c} IS NULL OR CAST({c} AS VARCHAR) = '')"),
        FilterOp::NotEmpty => format!("({c} IS NOT NULL AND CAST({c} AS VARCHAR) <> '')"),
    })
}
