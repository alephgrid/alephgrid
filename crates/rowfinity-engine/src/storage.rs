//! On-disk format of a workbook database: the format version, migrations from older
//! versions, the workbook revision and its operation history.
//!
//! Format history:
//! * **1** (phase 1–2): user tables carry a dense `_rf_id` that is also the row position;
//!   inserts and deletes renumber it. No `_rf_meta`.
//! * **2**: `_rf_id` is a stable record id, never reused within its table; display order
//!   is `_rf_ord` (see `catalog.rs`). `_rf_meta` holds the format, the revision and the
//!   workbook id; `_rf_tables` per-table id counters and structure generations;
//!   `_rf_history` one entry per committed mutation.
//! * **3** (phase 4): `_rf_tables.data_rev`, the revision that last changed a table's rows,
//!   columns or formulas; `_rf_queries` / `_rf_query_versions` saved queries with immutable
//!   versions; `_rf_materialized` how a table made from a query was computed.
//!
//! Opening an older file keeps a copy of it (`<file>.v<N>.bak`) and migrates it in one
//! transaction. There is no downgrade: the copy is the only way back. Opening a file
//! written by a newer format is refused.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use duckdb::{Connection, OptionalExt};

use crate::faults;
use crate::sql::{ident, lit};

/// The format this build writes.
pub const FORMAT: u32 = 3;
/// Spacing of `_rf_ord` values for rows written in order, leaving room for inserts.
pub const ORD_GAP: i64 = 1 << 20;
/// History entries kept; a mutation id older than this many revisions can be applied again.
pub const HISTORY_KEEP: u64 = 10_000;

pub const ID: &str = "_rf_id";
pub const ORD: &str = "_rf_ord";

/// Opening refused because the file needs a newer Rowfinity.
#[derive(Debug, Clone, thiserror::Error)]
#[error("this workbook was written by a newer version of Rowfinity (storage format {found}; this version reads up to {FORMAT}); upgrade Rowfinity to open it")]
pub struct TooNew {
    pub found: u32,
}

/// The file belongs to another workbook (for example a restore into the wrong place).
#[derive(Debug, Clone, thiserror::Error)]
#[error("this database belongs to workbook {stored}, not {expected}")]
pub struct WrongWorkbook {
    pub stored: String,
    pub expected: String,
}

/// What opening did to the file.
#[derive(Debug, Clone, Default)]
pub struct OpenReport {
    /// Format the file had before it was migrated, if it was.
    pub migrated_from: Option<u32>,
    /// Copy of the file as it was before migrating.
    pub backup: Option<PathBuf>,
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        &format!("SELECT count(*) FROM duckdb_tables() WHERE schema_name = 'main' AND database_name = current_database() AND table_name = {}", lit(name)),
        [],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Format of an open database: 0 for a new, empty file.
pub fn format_of(conn: &Connection) -> Result<u32> {
    if table_exists(conn, "_rf_meta")? {
        let v: Option<String> = conn
            .query_row("SELECT value FROM _rf_meta WHERE key = 'format'", [], |r| r.get(0))
            .ok();
        return v.as_deref().unwrap_or("0").parse().context("unreadable storage format");
    }
    if table_exists(conn, "_rf_columns")? || !user_tables(conn)?.is_empty() {
        return Ok(1);
    }
    Ok(0)
}

pub fn user_tables(conn: &Connection) -> Result<Vec<String>> {
    Ok(conn
        .prepare(
            "SELECT table_name FROM duckdb_tables()
             WHERE schema_name = 'main' AND database_name = current_database() AND NOT temporary
               AND NOT starts_with(table_name, '_rf_')
             ORDER BY table_name",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?)
}

const BASE_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS _rf_columns (table_name VARCHAR, name VARCHAR, formula VARCHAR, position INTEGER);
    CREATE TABLE IF NOT EXISTS _rf_sheets (data BLOB);
    CREATE TABLE IF NOT EXISTS _rf_views (
        id VARCHAR PRIMARY KEY, name VARCHAR, table_name VARCHAR, spec VARCHAR, widths VARCHAR, updated_at BIGINT
    );";

/// Tables added in format 2. No primary keys: DuckDB checks unique constraints eagerly
/// inside a transaction, which would refuse a delete followed by a re-insert of a key.
const V2_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS _rf_meta (key VARCHAR NOT NULL, value VARCHAR);
    CREATE TABLE IF NOT EXISTS _rf_tables (
        table_name VARCHAR NOT NULL, next_id BIGINT NOT NULL, dense BOOLEAN NOT NULL, gen BIGINT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS _rf_history (
        revision BIGINT NOT NULL, mutation_id VARCHAR, op VARCHAR NOT NULL, actor VARCHAR,
        at_ms BIGINT NOT NULL, summary VARCHAR, result VARCHAR
    );";

/// Added in format 3. `data_rev = -1` marks a table changed by the transaction in progress;
/// [`record`] stamps it with the new revision.
const V3_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS _rf_queries (
        id VARCHAR NOT NULL, name VARCHAR NOT NULL, kind VARCHAR NOT NULL, created_at BIGINT NOT NULL, updated_at BIGINT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS _rf_query_versions (
        query_id VARCHAR NOT NULL, version BIGINT NOT NULL, sql VARCHAR NOT NULL, params VARCHAR NOT NULL,
        created_at BIGINT NOT NULL, revision BIGINT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS _rf_materialized (
        table_name VARCHAR NOT NULL, query_id VARCHAR, query_version BIGINT, sql VARCHAR NOT NULL, params VARCHAR NOT NULL,
        sources VARCHAR NOT NULL, revision BIGINT NOT NULL, own_rev BIGINT NOT NULL, refreshed_at BIGINT NOT NULL
    );";

/// Bring a database at `path` to [`FORMAT`], copying it first when it is migrated.
/// Returns the open connection.
pub fn open(path: &Path) -> Result<(Connection, OpenReport)> {
    let conn = Connection::open(path).with_context(|| format!("opening workbook {}", path.display()))?;
    let found = format_of(&conn)?;
    if found > FORMAT {
        bail!(TooNew { found });
    }
    if found == FORMAT {
        return Ok((conn, OpenReport::default()));
    }
    if found == 0 {
        create_fresh(&conn)?;
        return Ok((conn, OpenReport::default()));
    }
    // Fold the WAL into the file and close it, so the copy is a complete database.
    conn.execute_batch("CHECKPOINT;")?;
    drop(conn);
    let backup = backup_path(path, found);
    copy_durably(path, &backup)?;
    let conn = Connection::open(path)?;
    migrate(&conn, found)?;
    conn.execute_batch("CHECKPOINT;")?;
    Ok((conn, OpenReport { migrated_from: Some(found), backup: Some(backup) }))
}

pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    create_fresh(&conn)?;
    Ok(conn)
}

fn backup_path(path: &Path, version: u32) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!(".v{version}.bak"));
    path.with_file_name(name)
}

/// Copy through a temporary name and rename, syncing the file and its directory, so a
/// crash leaves either no copy or a complete one.
pub fn copy_durably(from: &Path, to: &Path) -> Result<()> {
    let tmp = to.with_extension("partial");
    std::fs::copy(from, &tmp).with_context(|| format!("copying {} to {}", from.display(), tmp.display()))?;
    std::fs::File::open(&tmp)?.sync_all()?;
    std::fs::rename(&tmp, to)?;
    if let Some(dir) = to.parent() {
        sync_dir(dir);
    }
    Ok(())
}

/// Make a rename or new file in `dir` durable. Best effort: not every platform can
/// open a directory for syncing.
pub fn sync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

fn create_fresh(conn: &Connection) -> Result<()> {
    conn.execute_batch(&format!(
        "BEGIN TRANSACTION; {BASE_SCHEMA} {V2_SCHEMA} {V3_SCHEMA}
         ALTER TABLE _rf_tables ADD COLUMN data_rev BIGINT NOT NULL DEFAULT 0;
         INSERT INTO _rf_meta VALUES ('format', '{FORMAT}'), ('revision', '0');
         COMMIT;"
    ))?;
    Ok(())
}

/// Migrate from `from` to [`FORMAT`] in one transaction.
fn migrate(conn: &Connection, from: u32) -> Result<()> {
    conn.execute_batch("BEGIN TRANSACTION;")?;
    let result = (|| -> Result<()> {
        if from < 2 {
            migrate_v1_to_v2(conn)?;
        }
        if from < 3 {
            migrate_v2_to_v3(conn)?;
        }
        faults::check("migrate")?;
        Ok(())
    })();
    match result {
        Ok(()) => conn.execute_batch("COMMIT;")?,
        Err(e) => {
            conn.execute_batch("ROLLBACK;")?;
            return Err(e.context(format!("migrating workbook from storage format {from} to {FORMAT}")));
        }
    }
    Ok(())
}

/// Format 1 ids were dense positions in display order, so the order key is the id
/// spread by [`ORD_GAP`] and the next id is the row count.
fn migrate_v1_to_v2(conn: &Connection) -> Result<()> {
    conn.execute_batch(BASE_SCHEMA)?;
    conn.execute_batch(V2_SCHEMA)?;
    for table in user_tables(conn)? {
        let t = ident(&table);
        conn.execute_batch(&format!("ALTER TABLE {t} ADD COLUMN {ORD} BIGINT; UPDATE {t} SET {ORD} = {ID} * {ORD_GAP};"))?;
        let (count, max): (i64, Option<i64>) =
            conn.query_row(&format!("SELECT count(*), max({ID}) FROM {t}"), [], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let next = max.map(|m| m + 1).unwrap_or(0).max(count);
        let dense = max.map(|m| m + 1).unwrap_or(0) == count;
        conn.execute(
            "INSERT INTO _rf_tables VALUES (?, ?, ?, 0)",
            duckdb::params![table, next, dense],
        )?;
    }
    conn.execute_batch("INSERT INTO _rf_meta VALUES ('format', '2'), ('revision', '0');")?;
    Ok(())
}

/// Tables existing before format 3 count as last changed at the current revision.
fn migrate_v2_to_v3(conn: &Connection) -> Result<()> {
    conn.execute_batch(V3_SCHEMA)?;
    let rev = revision(conn)? as i64;
    conn.execute_batch(&format!("ALTER TABLE _rf_tables ADD COLUMN data_rev BIGINT NOT NULL DEFAULT {rev};"))?;
    meta_set(conn, "format", &FORMAT.to_string())
}

// ------------------------------------------------------------------ metadata

pub fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    // Only "no such row" is None; any other failure (an interrupt) must not be swallowed,
    // or the caller's transaction is left aborted behind its back.
    Ok(conn.query_row("SELECT value FROM _rf_meta WHERE key = ?", [key], |r| r.get(0)).optional()?)
}

pub fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    let n = conn.execute("UPDATE _rf_meta SET value = ? WHERE key = ?", duckdb::params![value, key])?;
    if n == 0 {
        conn.execute("INSERT INTO _rf_meta VALUES (?, ?)", duckdb::params![key, value])?;
    }
    Ok(())
}

pub fn revision(conn: &Connection) -> Result<u64> {
    Ok(meta_get(conn, "revision")?.as_deref().unwrap_or("0").parse()?)
}

/// Per-table counters, read in the caller's snapshot.
#[derive(Debug, Clone, Copy)]
pub struct TableMeta {
    pub next_id: i64,
    /// Every row's `_rf_ord` is its position × [`ORD_GAP`].
    pub dense: bool,
    /// Bumped by every change of row positions or columns; caches built from an older
    /// generation are stale.
    pub gen: i64,
}

pub fn table_meta(conn: &Connection, table: &str) -> Result<TableMeta> {
    conn.query_row(
        "SELECT next_id, dense, gen FROM _rf_tables WHERE table_name = ?",
        [table],
        |r| Ok(TableMeta { next_id: r.get(0)?, dense: r.get(1)?, gen: r.get(2)? }),
    )
    .with_context(|| format!("no metadata for table {table:?}"))
}

pub fn register_table(conn: &Connection, table: &str, rows: i64) -> Result<()> {
    conn.execute("DELETE FROM _rf_tables WHERE table_name = ?", [table])?;
    conn.execute("INSERT INTO _rf_tables (table_name, next_id, dense, gen, data_rev) VALUES (?, ?, true, 0, -1)", duckdb::params![table, rows])?;
    Ok(())
}

/// Mark a table's data as changed by the transaction in progress (see [`record`]).
pub fn touch_table(conn: &Connection, table: &str) -> Result<()> {
    conn.execute("UPDATE _rf_tables SET data_rev = -1 WHERE table_name = ?", [table])?;
    Ok(())
}

/// The revision that last changed each table's data, by table name.
pub fn data_revisions(conn: &Connection) -> Result<std::collections::HashMap<String, u64>> {
    Ok(conn
        .prepare("SELECT table_name, data_rev FROM _rf_tables")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64)))?
        .collect::<Result<_, _>>()?)
}

/// Record a structural change: new id counter and density, generation + 1.
pub fn update_table(conn: &Connection, table: &str, next_id: Option<i64>, dense: Option<bool>) -> Result<()> {
    let n = conn.execute(
        "UPDATE _rf_tables SET next_id = coalesce(?, next_id), dense = coalesce(?, dense), gen = gen + 1, data_rev = -1 WHERE table_name = ?",
        duckdb::params![next_id, dense, table],
    )?;
    if n != 1 {
        bail!("no metadata for table {table:?}");
    }
    Ok(())
}

pub fn rename_table(conn: &Connection, from: &str, to: &str) -> Result<()> {
    // A rename does not change the rows; materializations that read the old name see it gone.
    conn.execute("UPDATE _rf_tables SET table_name = ?, gen = gen + 1 WHERE table_name = ?", duckdb::params![to, from])?;
    conn.execute("UPDATE _rf_materialized SET table_name = ? WHERE table_name = ?", duckdb::params![to, from])?;
    Ok(())
}

pub fn forget_table(conn: &Connection, table: &str) -> Result<()> {
    conn.execute("DELETE FROM _rf_tables WHERE table_name = ?", [table])?;
    conn.execute("DELETE FROM _rf_materialized WHERE table_name = ?", [table])?;
    Ok(())
}

// ------------------------------------------------------------------- history

/// One committed mutation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ts_rs::TS)]
pub struct HistoryEntry {
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mutation_id: Option<String>,
    pub op: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub actor: Option<String>,
    pub at_ms: u64,
    /// Short description of the change, at most [`SUMMARY_BYTES`] bytes.
    pub summary: String,
}

pub const SUMMARY_BYTES: usize = 512;
/// Results longer than this are not kept for replay; a replay then returns `null`.
pub const RESULT_BYTES: usize = 64 * 1024;

/// The revision and result recorded for `mutation_id`, if it is in the history.
pub fn find_mutation(conn: &Connection, mutation_id: &str) -> Result<Option<(u64, Option<String>)>> {
    Ok(conn
        .query_row(
            "SELECT revision, result FROM _rf_history WHERE mutation_id = ? ORDER BY revision DESC LIMIT 1",
            [mutation_id],
            |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, Option<String>>(1)?)),
        )
        .optional()?)
}

/// Append a history entry for the next revision and bump the revision; the caller's
/// transaction makes this atomic with the change itself.
pub fn record(conn: &Connection, mutation_id: Option<&str>, op: &str, actor: Option<&str>, summary: &str, result: &str) -> Result<u64> {
    let next = revision(conn)? + 1;
    let summary = truncate(summary, SUMMARY_BYTES);
    let result = (result.len() <= RESULT_BYTES).then_some(result);
    faults::check("history")?;
    conn.execute(
        "INSERT INTO _rf_history VALUES (?, ?, ?, ?, ?, ?, ?)",
        duckdb::params![next as i64, mutation_id, op, actor, now_ms() as i64, summary, result],
    )?;
    meta_set(conn, "revision", &next.to_string())?;
    // Stamp what this transaction changed with its revision.
    conn.execute("UPDATE _rf_tables SET data_rev = ? WHERE data_rev = -1", [next as i64])?;
    conn.execute("UPDATE _rf_materialized SET revision = ? WHERE revision = -1", [next as i64])?;
    conn.execute("UPDATE _rf_materialized SET own_rev = ? WHERE own_rev = -1", [next as i64])?;
    conn.execute("UPDATE _rf_query_versions SET revision = ? WHERE revision = -1", [next as i64])?;
    if next > HISTORY_KEEP {
        conn.execute("DELETE FROM _rf_history WHERE revision <= ?", [(next - HISTORY_KEEP) as i64])?;
    }
    Ok(next)
}

pub fn history(conn: &Connection, after: u64, limit: usize) -> Result<Vec<HistoryEntry>> {
    let mut stmt = conn.prepare(
        "SELECT revision, mutation_id, op, actor, at_ms, summary FROM _rf_history WHERE revision > ? ORDER BY revision LIMIT ?",
    )?;
    let rows = stmt.query_map(duckdb::params![after as i64, limit as i64], |r| {
        Ok(HistoryEntry {
            revision: r.get::<_, i64>(0)? as u64,
            mutation_id: r.get(1)?,
            op: r.get(2)?,
            actor: r.get(3)?,
            at_ms: r.get::<_, i64>(4)? as u64,
            summary: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
