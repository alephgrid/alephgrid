//! Undo and redo of table changes (7.09).
//!
//! An undoable change keeps two copies of the rows it touched, in hidden tables of the
//! workbook: *before* (the rows that existed before it and that it changed or deleted) and
//! *after* (the rows that exist after it and that it changed or inserted), with the columns
//! whose values it set. Copies are made with set-based SQL, never row by row.
//!
//! Undoing is a new change (a new revision, in the history) that brings those rows back to
//! *before*: rows it inserted are deleted, rows it deleted come back with their ids and
//! their old place in the order, and the cells it set get their old values. It is refused
//! with a [`Conflict`] when anything it would take back changed since, so nobody's later
//! work is silently lost:
//!
//! * a cell it set holds another value now, or its row was deleted;
//! * a row it inserted was edited or deleted;
//! * a row it deleted is back, or another row took its place in the order;
//! * the table's columns changed (added, removed, renamed or retyped).
//!
//! Changes to other cells and rows never block it. The undo's own entry swaps the two
//! copies, so undoing an undo is redo, under the same rules.
//!
//! Without a revision, undo takes back the caller's own latest change (not someone
//! else's), and redo the caller's own latest undo when nothing of theirs came after it.
//! Any change still kept can be undone by its revision. The last [`UNDO_KEEP`] undoable
//! changes are kept; a change that touches more than [`MAX_UNDO_ROWS`] rows, and sheet,
//! column and table changes, are not undoable here (sheets have their own undo).

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use duckdb::{Connection, OptionalExt};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::Conflict;
use crate::sql::{ident, lit};
use crate::storage::{self, ID, ORD};
use crate::workbook::Workbook;

/// Undoable changes kept per workbook.
pub const UNDO_KEEP: i64 = 50;
/// Rows one change may copy for its undo (before or after).
pub const MAX_UNDO_ROWS: u64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum UndoKind {
    /// A change made by someone.
    Change,
    /// It took back another entry.
    Undo,
    /// It took back an undo.
    Redo,
}

impl UndoKind {
    fn as_str(self) -> &'static str {
        match self {
            UndoKind::Change => "change",
            UndoKind::Undo => "undo",
            UndoKind::Redo => "redo",
        }
    }

    pub(crate) fn parse(s: &str) -> UndoKind {
        match s {
            "undo" => UndoKind::Undo,
            "redo" => UndoKind::Redo,
            _ => UndoKind::Change,
        }
    }
}

/// How a history entry can be taken back.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct UndoState {
    pub table: String,
    pub kind: UndoKind,
    /// The revision this entry took back (undo and redo entries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub undoes: Option<u64>,
    /// The revision that took this entry back, if one did; it cannot be undone again (undo
    /// that one instead).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub undone_by: Option<u64>,
}

/// What an undo or redo did.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct UndoOutcome {
    pub table: String,
    /// The revision taken back.
    pub undone: u64,
    /// `undo` or `redo`.
    pub kind: UndoKind,
    /// Rows deleted, restored or changed.
    pub rows: u64,
}

/// Columns whose values a change set, each with its records (`None`: every row in both
/// copies).
pub(crate) type Cells = BTreeMap<String, Option<Vec<i64>>>;

/// Rows copied before a change, waiting for the change to finish.
pub(crate) struct Capture {
    table: String,
    before: String,
}

struct Entry {
    revision: u64,
    table: String,
    kind: UndoKind,
    undone_by: Option<u64>,
    before: String,
    after: String,
    cells: Cells,
}

fn side_table(revision: u64, which: &str) -> String {
    format!("_rf_undo_{revision}_{which}")
}

/// `(name, type)` of a table's stored columns, by name.
fn columns(conn: &Connection, table: &str) -> Result<BTreeMap<String, String>> {
    Ok(conn
        .prepare(
            "SELECT column_name, data_type FROM duckdb_columns()
             WHERE schema_name = 'main' AND database_name = current_database() AND table_name = ?",
        )?
        .query_map([table], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<_, _>>()?)
}

fn count(conn: &Connection, sql: &str) -> Result<u64> {
    Ok(conn.query_row(sql, [], |r| r.get::<_, i64>(0))? as u64)
}

/// Drop row copies no entry refers to any more.
pub(crate) fn drop_unused(conn: &Connection) -> Result<()> {
    let unused: Vec<String> = conn
        .prepare(
            "SELECT table_name FROM duckdb_tables()
             WHERE schema_name = 'main' AND database_name = current_database() AND starts_with(table_name, '_rf_undo_')
               AND table_name NOT IN (SELECT before_rows FROM _rf_undo UNION ALL SELECT after_rows FROM _rf_undo)",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for t in unused {
        conn.execute_batch(&format!("DROP TABLE main.{};", ident(&t)))?;
    }
    Ok(())
}

/// Keep the newest [`UNDO_KEEP`] entries.
fn prune(conn: &Connection) -> Result<()> {
    conn.execute(
        "DELETE FROM _rf_undo WHERE revision NOT IN (SELECT revision FROM _rf_undo ORDER BY revision DESC LIMIT ?)",
        [UNDO_KEEP],
    )?;
    drop_unused(conn)
}

fn entry(conn: &Connection, condition: &str) -> Result<Option<Entry>> {
    let row = conn
        .query_row(
            &format!(
                "SELECT revision, table_name, kind, undone_by, before_rows, after_rows, cells FROM _rf_undo WHERE {condition}
                 ORDER BY revision DESC LIMIT 1"
            ),
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((revision, table, kind, undone_by, before, after, cells)) = row else { return Ok(None) };
    Ok(Some(Entry {
        revision,
        table,
        kind: UndoKind::parse(&kind),
        undone_by: undone_by.map(|v| v as u64),
        before,
        after,
        cells: serde_json::from_str(&cells)?,
    }))
}

impl Workbook {
    /// Copy the rows of `table` matching `condition` before a change touches them. `None`
    /// when they are too many to keep (the change is then not undoable).
    pub(crate) fn undo_begin(&mut self, table: &str, condition: &str) -> Result<Option<Capture>> {
        let t = ident(table);
        if count(&self.conn, &format!("SELECT count(*) FROM main.{t} WHERE {condition}"))? > MAX_UNDO_ROWS {
            return Ok(None);
        }
        let before = side_table(self.txn_revision, "b");
        self.conn.execute_batch(&format!(
            "DROP TABLE IF EXISTS main.{b}; CREATE TABLE main.{b} AS SELECT * FROM main.{t} WHERE {condition};",
            b = ident(&before)
        ))?;
        Ok(Some(Capture { table: table.to_string(), before }))
    }

    /// Finish a change begun with [`Self::undo_begin`]: copy the rows matching `condition`
    /// after it and record how to take it back. `cells` are the columns whose values it set.
    pub(crate) fn undo_finish(&mut self, capture: Option<Capture>, condition: &str, cells: Cells) -> Result<()> {
        let Some(Capture { table, before }) = capture else { return Ok(()) };
        let revision = self.txn_revision;
        // Two undoable changes in one transaction cannot be taken back separately: neither is.
        let twice = count(&self.conn, &format!("SELECT count(*) FROM _rf_undo WHERE revision = {revision}"))? > 0;
        let t = ident(&table);
        if twice || count(&self.conn, &format!("SELECT count(*) FROM main.{t} WHERE {condition}"))? > MAX_UNDO_ROWS {
            self.conn.execute(&format!("DELETE FROM _rf_undo WHERE revision = {revision}"), [])?;
            return drop_unused(&self.conn);
        }
        let after = side_table(revision, "a");
        self.conn.execute_batch(&format!(
            "DROP TABLE IF EXISTS main.{a}; CREATE TABLE main.{a} AS SELECT * FROM main.{t} WHERE {condition};",
            a = ident(&after)
        ))?;
        self.conn.execute(
            "INSERT INTO _rf_undo VALUES (?, ?, ?, 'change', NULL, NULL, ?, ?, ?)",
            duckdb::params![revision as i64, table, self.txn_actor, before, after, serde_json::to_string(&cells)?],
        )?;
        prune(&self.conn)
    }

    /// Take back a table change: revision `revision`, or without one the caller's own
    /// latest change (`redo`: their latest undo). `None` when there is nothing to take back.
    /// See the module docs for what is refused.
    pub fn undo_change(&mut self, revision: Option<u64>, redo: bool) -> Result<Option<UndoOutcome>> {
        self.mutating(if redo { "redo" } else { "undo" }, None, |wb| wb.undo_in_txn(revision, redo))
    }

    fn undo_in_txn(&mut self, revision: Option<u64>, redo: bool) -> Result<Option<UndoOutcome>> {
        let actor = match &self.txn_actor {
            Some(a) => format!("actor = {}", lit(a)),
            None => "actor IS NULL".to_string(),
        };
        let found = match revision {
            Some(r) => match entry(&self.conn, &format!("revision = {r}"))? {
                Some(e) => e,
                None => bail!("revision {r} cannot be undone: it is not a table change, it was too large, or it is no longer kept (the last {UNDO_KEEP} are)"),
            },
            None if redo => {
                // Redo only right after the caller's own undo (any change of theirs since ends it).
                match entry(&self.conn, &actor)? {
                    Some(e) if e.kind == UndoKind::Undo && e.undone_by.is_none() => e,
                    _ => return Ok(None),
                }
            }
            None => match entry(&self.conn, &format!("{actor} AND kind <> 'undo' AND undone_by IS NULL"))? {
                Some(e) => e,
                None => return Ok(None),
            },
        };
        if let Some(by) = found.undone_by {
            bail!("revision {} was already taken back by revision {by}; undo that one instead", found.revision);
        }
        let outcome = self.take_back(&found)?;
        let kind = if found.kind == UndoKind::Undo { UndoKind::Redo } else { UndoKind::Undo };
        let now = self.txn_revision;
        // The inverse swaps the copies: undoing it is redo.
        self.conn.execute(
            "INSERT INTO _rf_undo VALUES (?, ?, ?, ?, ?, NULL, ?, ?, ?)",
            duckdb::params![
                now as i64,
                found.table,
                self.txn_actor,
                kind.as_str(),
                found.revision as i64,
                found.after,
                found.before,
                serde_json::to_string(&found.cells)?
            ],
        )?;
        self.conn.execute("UPDATE _rf_undo SET undone_by = ? WHERE revision = ?", duckdb::params![now as i64, found.revision as i64])?;
        prune(&self.conn)?;
        Ok(Some(UndoOutcome { table: found.table.clone(), undone: found.revision, kind, rows: outcome }))
    }

    /// Bring the rows of `e` from its *after* copy back to its *before* copy, or refuse.
    fn take_back(&mut self, e: &Entry) -> Result<u64> {
        let conn = &self.conn;
        let what = format!("revision {}", e.revision);
        let exists = count(conn, &format!("SELECT count(*) FROM _rf_tables WHERE table_name = {}", lit(&e.table)))? > 0;
        if !exists {
            bail!(Conflict::moved(format!("{what} cannot be undone: its table {:?} no longer exists", e.table)));
        }
        let now = columns(conn, &e.table)?;
        if columns(conn, &e.before)? != now || columns(conn, &e.after)? != now {
            bail!(Conflict::moved(format!("{what} cannot be undone: the columns of {:?} changed since", e.table)));
        }
        let (t, b, a) = (ident(&e.table), ident(&e.before), ident(&e.after));
        let data: Vec<&String> = now.keys().filter(|c| c.as_str() != ID && c.as_str() != ORD).collect();
        let differs = |x: &str, y: &str| {
            if data.is_empty() {
                "false".to_string()
            } else {
                data.iter().map(|c| format!("{x}.{c} IS DISTINCT FROM {y}.{c}", c = ident(c))).collect::<Vec<_>>().join(" OR ")
            }
        };
        let mut problems = Vec::new();
        // Rows it inserted must be as it left them.
        let inserted = format!("a.{ID} NOT IN (SELECT {ID} FROM main.{b})");
        let n = count(
            conn,
            &format!("SELECT count(*) FROM main.{a} a LEFT JOIN main.{t} t ON t.{ID} = a.{ID} WHERE {inserted} AND (t.{ID} IS NULL OR {})", differs("t", "a")),
        )?;
        if n > 0 {
            problems.push(format!("{n} row(s) it added were changed or deleted"));
        }
        // Cells it set must still hold what it wrote.
        for (column, ids) in &e.cells {
            if !now.contains_key(column) {
                bail!(Conflict::moved(format!("{what} cannot be undone: column {column:?} of {:?} is gone", e.table)));
            }
            let only = match ids {
                Some(ids) if ids.is_empty() => continue,
                Some(ids) => format!(" AND a.{ID} IN ({})", ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")),
                None => String::new(),
            };
            let n = count(
                conn,
                &format!(
                    "SELECT count(*) FROM main.{a} a JOIN main.{b} b ON b.{ID} = a.{ID} LEFT JOIN main.{t} t ON t.{ID} = a.{ID}
                     WHERE (t.{ID} IS NULL OR t.{c} IS DISTINCT FROM a.{c}){only}",
                    c = ident(column)
                ),
            )?;
            if n > 0 {
                problems.push(format!("{n} cell(s) of {column:?} it set were changed or their rows deleted"));
            }
        }
        // Rows it deleted must still be gone, and their places in the order free.
        let deleted = format!("b.{ID} NOT IN (SELECT {ID} FROM main.{a})");
        let back = count(conn, &format!("SELECT count(*) FROM main.{b} b JOIN main.{t} t ON t.{ID} = b.{ID} WHERE {deleted}"))?;
        if back > 0 {
            problems.push(format!("{back} row(s) it deleted are back"));
        }
        let taken = count(conn, &format!("SELECT count(*) FROM main.{b} b JOIN main.{t} t ON t.{ORD} = b.{ORD} WHERE {deleted}"))?;
        if taken > 0 {
            problems.push(format!("rows were inserted where {taken} row(s) it deleted were"));
        }
        if !problems.is_empty() {
            bail!(Conflict::values(format!("{what} cannot be undone: since then {}; nothing was changed", problems.join(", "))));
        }
        // Take it back: remove what it added, bring back what it deleted, restore what it set.
        let removed = conn.execute(&format!("DELETE FROM main.{t} WHERE {ID} IN (SELECT {ID} FROM main.{a} a WHERE {inserted})"), [])? as u64;
        crate::faults::check("undo")?;
        let restored = conn.execute(&format!("INSERT INTO main.{t} BY NAME SELECT * FROM main.{b} b WHERE {deleted}"), [])? as u64;
        let mut changed = 0;
        for (column, ids) in &e.cells {
            let only = match ids {
                Some(ids) if ids.is_empty() => continue,
                Some(ids) => format!(" AND b.{ID} IN ({})", ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")),
                None => String::new(),
            };
            changed = changed.max(conn.execute(
                &format!(
                    "UPDATE main.{t} SET {c} = b.{c} FROM main.{b} b WHERE {t}.{ID} = b.{ID} AND b.{ID} IN (SELECT {ID} FROM main.{a}){only}",
                    c = ident(column)
                ),
                [],
            )? as u64);
        }
        storage::touch_table(conn, &e.table)?;
        if removed > 0 || restored > 0 {
            // Rows moved: views and the position index are rebuilt on next use.
            storage::update_table(conn, &e.table, None, Some(false))?;
        }
        Ok(removed + restored + changed)
    }
}
