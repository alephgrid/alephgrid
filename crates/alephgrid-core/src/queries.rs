//! Saved queries with immutable versions, and tables materialized from queries (4.04–4.06).
//!
//! Three different things, stored apart:
//! * a **saved query** (`_rf_queries` + `_rf_query_versions`): SQL text and parameters. Saving
//!   changed text or parameters appends a version; versions never change.
//! * a **live query** is a saved query of kind `live`: clients run it on current data every
//!   time it is opened (the result says which revision it read).
//! * a **materialized result** (`_rf_materialized`): a table created from a query, with the
//!   SQL, parameters, source tables and their data revisions at the time. It is a snapshot;
//!   [`Workbook::refresh_materialized`] recomputes it on request, all or nothing.

use anyhow::{anyhow, bail, Result};
use duckdb::Connection;

use crate::model::*;
use crate::reads::{is_internal, Db};
use crate::sql::ident;
use crate::sqlpolicy::{self, QueryParam};
use crate::storage::{self, ID, ORD, ORD_GAP};
use crate::Workbook;

/// The query's columns no longer match the table it refreshes. Nothing was changed.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct SchemaChanged(pub String);

pub(crate) fn saved_queries(db: &Db) -> Result<Vec<SavedQuery>> {
    let mut stmt = db.conn.prepare(
        "SELECT q.id, q.name, q.kind, q.created_at, q.updated_at, v.version, v.sql, v.params
         FROM _rf_queries q JOIN _rf_query_versions v ON v.query_id = q.id
         WHERE v.version = (SELECT max(version) FROM _rf_query_versions WHERE query_id = q.id)
         ORDER BY lower(q.name)",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, String>(7)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, name, kind, created_at, updated_at, version, sql, params) = row?;
        out.push(SavedQuery {
            id,
            name,
            kind: parse_kind(&kind),
            version: version as u64,
            sql,
            params: serde_json::from_str(&params)?,
            created_at: created_at as u64,
            updated_at: updated_at as u64,
        });
    }
    Ok(out)
}

pub(crate) fn versions(db: &Db, id: &str) -> Result<Vec<QueryVersion>> {
    if !saved_queries(db)?.iter().any(|q| q.id == id) {
        bail!("unknown saved query {id}");
    }
    let mut stmt =
        db.conn.prepare("SELECT version, sql, params, created_at, revision FROM _rf_query_versions WHERE query_id = ? ORDER BY version")?;
    let rows = stmt.query_map([id], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (version, sql, params, created_at, revision) = row?;
        out.push(QueryVersion {
            version: version as u64,
            sql,
            params: serde_json::from_str(&params)?,
            created_at: created_at as u64,
            revision: revision.max(0) as u64,
        });
    }
    Ok(out)
}

fn parse_kind(kind: &str) -> QueryKind {
    if kind == "live" { QueryKind::Live } else { QueryKind::Query }
}

fn kind_text(kind: QueryKind) -> &'static str {
    match kind {
        QueryKind::Query => "query",
        QueryKind::Live => "live",
    }
}

/// Record how `table` was computed, inside the transaction that created it.
pub(crate) fn record_materialized(
    conn: &Connection,
    table: &str,
    sql: &str,
    params: &[QueryParam],
    sources: &[String],
    origin: Option<(&str, u64)>,
) -> Result<()> {
    let data = storage::data_revisions(conn)?;
    let sources: Vec<MaterializedSource> =
        sources.iter().map(|s| MaterializedSource { table: s.clone(), data_rev: data.get(s).copied().unwrap_or(0), current_rev: None }).collect();
    conn.execute("DELETE FROM _rf_materialized WHERE table_name = ?", [table])?;
    conn.execute(
        "INSERT INTO _rf_materialized VALUES (?, ?, ?, ?, ?, ?, -1, -1, ?)",
        duckdb::params![
            table,
            origin.map(|o| o.0),
            origin.map(|o| o.1 as i64),
            sql,
            serde_json::to_string(params)?,
            serde_json::to_string(&sources)?,
            storage::now_ms() as i64
        ],
    )?;
    Ok(())
}

/// Every materialized table, with staleness computed from the current data revisions.
pub(crate) fn materialized(db: &Db) -> Result<Vec<MaterializedTable>> {
    db.snapshot(|| {
        let data = storage::data_revisions(db.conn)?;
        let mut stmt = db.conn.prepare(
            "SELECT table_name, query_id, query_version, sql, params, sources, revision, own_rev, refreshed_at
             FROM _rf_materialized ORDER BY lower(table_name)",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (table, query, query_version, sql, params, sources, revision, own_rev, refreshed_at) = row?;
            let mut sources: Vec<MaterializedSource> = serde_json::from_str(&sources)?;
            for s in &mut sources {
                s.current_rev = data.get(&s.table).copied();
            }
            let stale = sources.iter().any(|s| s.current_rev != Some(s.data_rev));
            let edited = data.get(&table).is_some_and(|&now| now as i64 > own_rev);
            out.push(MaterializedTable {
                table,
                query,
                query_version: query_version.map(|v| v as u64),
                sql,
                params: serde_json::from_str(&params)?,
                sources,
                revision: revision.max(0) as u64,
                refreshed_at: refreshed_at as u64,
                stale,
                edited,
            });
        }
        Ok(out)
    })
}

impl Workbook {
    pub fn saved_queries(&self) -> Result<Vec<SavedQuery>> {
        saved_queries(&self.db())
    }

    pub fn query_versions(&self, id: &str) -> Result<Vec<QueryVersion>> {
        versions(&self.db(), id)
    }

    pub fn materialized(&self) -> Result<Vec<MaterializedTable>> {
        materialized(&self.db())
    }

    /// Create a saved query, or update the one with id `id`: a new version when the SQL or
    /// parameters changed, and the name and kind in any case. The SQL must pass the policy.
    pub fn save_query(&mut self, id: Option<&str>, name: &str, kind: QueryKind, sql: &str, params: &[QueryParam]) -> Result<SavedQuery> {
        let name = name.trim();
        if name.is_empty() {
            bail!("a saved query needs a name");
        }
        let sql = sql.trim();
        let checked = self.db().check(sql)?;
        sqlpolicy::validate_params(&checked.params, params)?;
        let existing = self.saved_queries()?;
        if existing.iter().any(|q| q.name.to_lowercase() == name.to_lowercase() && Some(q.id.as_str()) != id) {
            bail!("a saved query named {name:?} already exists");
        }
        let current = match id {
            Some(id) => Some(existing.iter().find(|q| q.id == id).ok_or_else(|| anyhow!("unknown saved query {id}"))?.clone()),
            None => None,
        };
        let now = storage::now_ms();
        let id = match &current {
            Some(q) => q.id.clone(),
            None => format!("q{now:x}{:04x}", existing.len()),
        };
        let params_json = serde_json::to_string(params)?;
        self.mutating("save_query", None, |wb| {
            let version = match &current {
                None => {
                    wb.conn.execute(
                        "INSERT INTO _rf_queries VALUES (?, ?, ?, ?, ?)",
                        duckdb::params![id, name, kind_text(kind), now as i64, now as i64],
                    )?;
                    Some(1)
                }
                Some(q) => {
                    wb.conn.execute(
                        "UPDATE _rf_queries SET name = ?, kind = ?, updated_at = ? WHERE id = ?",
                        duckdb::params![name, kind_text(kind), now as i64, id],
                    )?;
                    (q.sql != sql || q.params != params).then_some(q.version + 1)
                }
            };
            if let Some(v) = version {
                wb.conn.execute(
                    "INSERT INTO _rf_query_versions VALUES (?, ?, ?, ?, ?, -1)",
                    duckdb::params![id, v as i64, sql, params_json, now as i64],
                )?;
            }
            wb.saved_queries()?.into_iter().find(|q| q.id == id).ok_or_else(|| anyhow!("saved query {id} vanished"))
        })
    }

    pub fn rename_query(&mut self, id: &str, name: &str) -> Result<SavedQuery> {
        let q = self.saved_queries()?.into_iter().find(|q| q.id == id).ok_or_else(|| anyhow!("unknown saved query {id}"))?;
        let (sql, params) = (q.sql.clone(), q.params.clone());
        self.save_query(Some(id), name, q.kind, &sql, &params)
    }

    pub fn delete_query(&mut self, id: &str) -> Result<()> {
        if !self.saved_queries()?.iter().any(|q| q.id == id) {
            bail!("unknown saved query {id}");
        }
        self.mutating("delete_query", None, |wb| {
            wb.conn.execute("DELETE FROM _rf_query_versions WHERE query_id = ?", [id])?;
            wb.conn.execute("DELETE FROM _rf_queries WHERE id = ?", [id])?;
            Ok(())
        })
    }

    /// Recompute a materialized table from its recorded SQL and parameters, replacing all
    /// of its rows in one transaction (new rows get new record ids). Fails without changing
    /// anything when a source is gone, the SQL no longer passes the policy, or the result's
    /// columns differ from the table's.
    pub fn refresh_materialized(&mut self, table: &str) -> Result<MaterializedTable> {
        let rec = self
            .materialized()?
            .into_iter()
            .find(|m| m.table == table)
            .ok_or_else(|| anyhow!("{table:?} was not made from a query, so there is nothing to refresh"))?;
        let checked = self.db().check(&rec.sql).map_err(|e| anyhow!("cannot refresh {table:?}: {e:#}"))?;
        sqlpolicy::validate_params(&checked.params, &rec.params)?;
        let data_columns: Vec<Column> = self.columns(table)?.into_iter().filter(|c| !c.is_computed()).collect();
        self.mutating("refresh_materialized", None, |wb| {
            let q = wb.db().expand_query(&checked)?;
            let produced = wb.db().describe_query(&q, &rec.params)?;
            let expected: Vec<(String, String)> = data_columns.iter().map(|c| (c.name.to_lowercase(), c.sql_type.clone())).collect();
            let got: Vec<(String, String)> =
                produced.iter().map(|c| (c.name.trim_start_matches(|ch| is_internal(&c.name) && ch == '_').to_lowercase(), c.sql_type.clone())).collect();
            if expected != got {
                let show = |cols: &[(String, String)]| cols.iter().map(|(n, t)| format!("{n} {t}")).collect::<Vec<_>>().join(", ");
                bail!(SchemaChanged(format!(
                    "the query now produces ({}) but {table:?} has ({}); nothing was changed. Create a new table from the query instead",
                    show(&got),
                    show(&expected)
                )));
            }
            let t = ident(table);
            let meta = storage::table_meta(&wb.conn, table)?;
            let cols: Vec<String> = data_columns.iter().map(|c| ident(&c.name)).collect();
            let aliases: Vec<String> = (0..produced.len()).map(|i| format!("_c{i}")).collect();
            wb.conn.execute_batch(&format!("DELETE FROM main.{t};"))?;
            let insert = format!(
                "INSERT INTO main.{t} ({ID}, {ORD}, {cols}) SELECT {next} + _rf_n, _rf_n * {ORD_GAP}, {aliases} FROM (
                   SELECT (row_number() OVER () - 1)::BIGINT AS _rf_n, * FROM (\n{sql}\n) AS _q({aliases})
                 )",
                cols = cols.join(", "),
                next = meta.next_id,
                aliases = aliases.join(", "),
                sql = q.sql,
            );
            let mut stmt = wb.conn.prepare(&insert).map_err(|e| crate::reads::user_error(e.into(), q.lines_before + 2))?;
            sqlpolicy::bind(&mut stmt, &rec.params)?;
            let n = stmt.raw_execute().map_err(|e| crate::reads::user_error(e.into(), q.lines_before + 2))? as i64;
            crate::faults::check("refresh_materialized")?;
            storage::update_table(&wb.conn, table, Some(meta.next_id + n), Some(true))?;
            record_materialized(&wb.conn, table, &rec.sql, &rec.params, &q.sources, rec.query.as_deref().zip(rec.query_version))?;
            Ok(())
        })?;
        self.materialized()?.into_iter().find(|m| m.table == table).ok_or_else(|| anyhow!("{table:?} vanished"))
    }
}
