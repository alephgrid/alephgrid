//! S.11: comments on sheet cells. Each comment is anchored to a cell of a sheet (by the
//! sheet's stable id, so renames and moves of the sheet keep it), with its author and time;
//! replies point at their thread's first comment. Inserting or deleting rows and columns
//! moves the anchors with the cells (comments on deleted cells are removed with them).

use anyhow::{anyhow, bail, Result};
use duckdb::Connection;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::workbook::Workbook;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS _rf_comments (
    id VARCHAR PRIMARY KEY, sheet BIGINT NOT NULL, row_ix BIGINT NOT NULL, col_ix BIGINT NOT NULL,
    author VARCHAR NOT NULL, created_ms BIGINT NOT NULL, edited_ms BIGINT, body VARCHAR NOT NULL,
    resolved BOOLEAN NOT NULL DEFAULT false, parent VARCHAR);";

/// The longest comment, in bytes.
pub const MAX_COMMENT_BYTES: usize = 10_000;

/// A comment on a sheet cell (row and column 0-based).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct CellComment {
    pub id: String,
    pub row: u32,
    pub column: u32,
    pub author: String,
    pub created_ms: u64,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_ms: Option<u64>,
    pub text: String,
    pub resolved: bool,
    /// The thread's first comment, for a reply.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// Which way rows or columns moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Rows,
    Columns,
}

fn ensure(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

/// A random id (the engine's UUID generator).
fn new_id(conn: &Connection) -> Result<String> {
    let id: String = conn.query_row("SELECT replace(gen_random_uuid()::VARCHAR, '-', '')", [], |r| r.get(0))?;
    Ok(id[..16].to_string())
}

fn list(conn: &Connection, key: i64) -> Result<Vec<CellComment>> {
    let exists: i64 = conn.query_row("SELECT count(*) FROM duckdb_tables() WHERE table_name = '_rf_comments' AND database_name = current_database()", [], |r| r.get(0))?;
    if exists == 0 {
        return Ok(vec![]);
    }
    let mut stmt = conn.prepare(
        "SELECT id, row_ix, col_ix, author, created_ms, edited_ms, body, resolved, parent FROM _rf_comments WHERE sheet = ? ORDER BY created_ms, id",
    )?;
    let rows = stmt.query_map([key], |r| {
        Ok(CellComment {
            id: r.get(0)?,
            row: r.get::<_, i64>(1)? as u32,
            column: r.get::<_, i64>(2)? as u32,
            author: r.get(3)?,
            created_ms: r.get::<_, i64>(4)? as u64,
            edited_ms: r.get::<_, Option<i64>>(5)?.map(|v| v as u64),
            text: r.get(6)?,
            resolved: r.get(7)?,
            parent: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

impl crate::workbook::Reader {
    /// The comments of a sheet, oldest first (S.11).
    pub fn comments(&self, sheet: u32) -> Result<Vec<CellComment>> {
        let key = {
            let s = self.sheets();
            s.info(sheet)?;
            s.sheet_id(sheet)? as i64
        };
        list(&self.conn, key)
    }
}

impl Workbook {
    fn sheet_key(&self, index: u32) -> Result<i64> {
        let s = self.sheets();
        s.info(index)?;
        s.sheet_id(index).map(|id| id as i64)
    }

    /// The comments of a sheet, oldest first.
    pub fn comments(&self, sheet: u32) -> Result<Vec<CellComment>> {
        let key = self.sheet_key(sheet)?;
        list(&self.conn, key)
    }

    /// Add a comment (or, with `parent`, a reply) on a cell.
    pub fn add_comment(&mut self, sheet: u32, row: u32, column: u32, text: &str, parent: Option<&str>) -> Result<CellComment> {
        let text = text.trim();
        if text.is_empty() || text.len() > MAX_COMMENT_BYTES {
            bail!("a comment is 1 to {MAX_COMMENT_BYTES} bytes");
        }
        if row >= crate::sheets::MAX_ROWS || column >= crate::sheets::MAX_COLUMNS {
            bail!("the cell is outside the sheet");
        }
        let key = self.sheet_key(sheet)?;
        self.mutating("add_comment", None, |wb| {
            ensure(&wb.conn)?;
            if let Some(p) = parent {
                let n: i64 = wb.conn.query_row("SELECT count(*) FROM _rf_comments WHERE id = ? AND parent IS NULL AND sheet = ?", duckdb::params![p, key], |r| r.get(0))?;
                if n == 0 {
                    bail!("the comment being replied to no longer exists");
                }
            }
            let c = CellComment {
                id: new_id(&wb.conn)?,
                row,
                column,
                author: wb.txn_actor.clone().or_else(|| wb.actor.clone()).unwrap_or_else(|| "Someone".into()),
                created_ms: crate::storage::now_ms(),
                edited_ms: None,
                text: text.to_string(),
                resolved: false,
                parent: parent.map(str::to_string),
            };
            wb.conn.execute(
                "INSERT INTO _rf_comments VALUES (?, ?, ?, ?, ?, ?, NULL, ?, false, ?)",
                duckdb::params![c.id, key, row as i64, column as i64, c.author, c.created_ms as i64, c.text, c.parent],
            )?;
            Ok(c)
        })
    }

    /// Change a comment's text.
    pub fn edit_comment(&mut self, id: &str, text: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() || text.len() > MAX_COMMENT_BYTES {
            bail!("a comment is 1 to {MAX_COMMENT_BYTES} bytes");
        }
        self.mutating("edit_comment", None, |wb| {
            ensure(&wb.conn)?;
            let n = wb.conn.execute("UPDATE _rf_comments SET body = ?, edited_ms = ? WHERE id = ?", duckdb::params![text, crate::storage::now_ms() as i64, id])?;
            if n == 0 { bail!("no such comment"); }
            Ok(())
        })
    }

    /// Resolve (or reopen) a thread.
    pub fn resolve_comment(&mut self, id: &str, resolved: bool) -> Result<()> {
        self.mutating("resolve_comment", None, |wb| {
            ensure(&wb.conn)?;
            let n = wb.conn.execute("UPDATE _rf_comments SET resolved = ? WHERE id = ? OR parent = ?", duckdb::params![resolved, id, id])?;
            if n == 0 { bail!("no such comment"); }
            Ok(())
        })
    }

    /// Delete a comment; deleting a thread's first comment deletes its replies.
    pub fn delete_comment(&mut self, id: &str) -> Result<()> {
        self.mutating("delete_comment", None, |wb| {
            ensure(&wb.conn)?;
            let n = wb.conn.execute("DELETE FROM _rf_comments WHERE id = ? OR parent = ?", duckdb::params![id, id])?;
            if n == 0 { bail!("no such comment"); }
            Ok(())
        })
    }

    /// Move comment anchors after `count` rows or columns were inserted (`count > 0`) or
    /// deleted (`count < 0`) at `at` on a sheet. Comments on deleted cells go with them.
    pub fn shift_comments(&mut self, sheet: u32, axis: Axis, at: u32, count: i64) -> Result<()> {
        let key = self.sheet_key(sheet)?;
        let col = if axis == Axis::Rows { "row_ix" } else { "col_ix" };
        self.mutating("shift_comments", None, |wb| {
            ensure(&wb.conn)?;
            if count < 0 {
                let end = at as i64 - count;
                wb.conn.execute(&format!("DELETE FROM _rf_comments WHERE sheet = ? AND {col} >= ? AND {col} < ?"), duckdb::params![key, at as i64, end])?;
                wb.conn.execute(&format!("UPDATE _rf_comments SET {col} = {col} + ? WHERE sheet = ? AND {col} >= ?"), duckdb::params![count, key, end])?;
            } else {
                wb.conn.execute(&format!("UPDATE _rf_comments SET {col} = {col} + ? WHERE sheet = ? AND {col} >= ?"), duckdb::params![count, key, at as i64])?;
            }
            Ok(())
        })
        .map_err(|e| anyhow!(e))
    }
}
