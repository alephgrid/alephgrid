//! A workbook is one DuckDB database file. Every user table carries two hidden columns:
//! `_rf_id`, a stable record id that is never reused, and `_rf_ord`, the display order
//! (see `catalog.rs` for how positions map to it without renumbering).
//!
//! Sorted or filtered views materialize only a permutation `(_rf_pos, _rf_id)` into a
//! scratch database; rows are then fetched by id. Formula columns are plain expressions
//! in the projection, so they are computed only for rows that are actually read.
//!
//! Every change goes through [`Workbook::mutate`]: one transaction holding the change,
//! the next revision and its history entry. Reads can run on [`Reader`] connections at
//! the same time as a change, each in its own snapshot.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, MutexGuard};

use anyhow::{anyhow, bail, Result};
use duckdb::Connection;
use serde_json::Value;

use crate::catalog::{PosIndex, Shared, SCRATCH};
use crate::faults;
use crate::formula;
use crate::import::{self, ColumnType, FilePreview, PreviewColumn};
use crate::limits;
use crate::model::*;
use crate::reads::{is_internal, Db};
use crate::sheets::{SheetInfo, SheetInput, Sheets};
use crate::sql::{formula_to_sql, ident, lit, sanitize_table_name};
use crate::storage::{self, HistoryEntry, OpenReport, ID, ORD, ORD_GAP};

pub struct Workbook {
    conn: Connection,
    shared: Arc<Shared>,
    /// 1 while [`Workbook::mutate`] holds a transaction.
    txn_depth: u32,
    /// Bumped whenever a sheet change was attempted inside the current transaction, so a
    /// rollback knows to reload the sheet model from the file.
    sheet_epoch: u64,
    after_commit: Vec<AfterCommit>,
    report: OpenReport,
}

/// In-memory updates that may only happen once the transaction has committed.
enum AfterCommit {
    /// Store this position index for `table`, stamped with the committed generation.
    Index(String, PosIndex),
    RenameTable { from: String, to: String },
    RenameColumn { table: String, from: String, to: String },
    /// Drop references to columns that no longer exist from open views of `table`.
    PruneSpecs(String),
    ForgetTable(String),
}

/// Who asked for a change and how to recognise a retry of it.
#[derive(Debug, Clone, Default)]
pub struct Mutation<'a> {
    /// Client-chosen id. A mutation whose id is already in the history is not applied
    /// again (see [`storage::HISTORY_KEEP`]).
    pub id: Option<&'a str>,
    pub op: &'a str,
    pub actor: Option<&'a str>,
    pub summary: String,
}

/// Outcome of [`Workbook::mutate`].
#[derive(Debug, Clone)]
pub struct Committed {
    pub revision: u64,
    pub result: Value,
    /// True when this was a retry of a mutation already in the history.
    pub replayed: bool,
}

/// Result of copying table data into a sheet, which holds far fewer rows than a table.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ts_rs::TS)]
pub struct SheetCopy {
    pub sheet: SheetInfo,
    pub rows_copied: u64,
    /// Rows in the source, when known without running the query to the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub total_rows: Option<u64>,
    /// True when rows were left out because of `row_limit`.
    pub truncated: bool,
    pub row_limit: u64,
}

/// What an upload would produce, shown before importing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum UploadPreview {
    Table { preview: FilePreview },
    Sheets { sheets: Vec<SheetInfo>, warnings: Vec<String> },
}

/// What an import produced: a columnar table, or IronCalc sheets for .xlsx files.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Imported {
    Table { table: TableInfo },
    /// `warnings` lists what an .xlsx file contained that sheets do not carry over.
    Sheets { sheets: Vec<SheetInfo>, warnings: Vec<String> },
}

/// A second connection to a workbook for reads. Reads see committed data only, run at the
/// same time as the writer's work, and share open views with it.
pub struct Reader {
    conn: Connection,
    shared: Arc<Shared>,
}

impl Workbook {
    /// Open a workbook file, migrating it from an older format if needed (see
    /// `storage.rs`). Fails for files written by a newer format.
    pub fn open(path: &Path) -> Result<Workbook> {
        let (conn, report) = storage::open(path)?;
        Self::init(conn, report)
    }

    pub fn open_in_memory() -> Result<Workbook> {
        Self::init(storage::open_in_memory()?, OpenReport::default())
    }

    fn init(conn: Connection, report: OpenReport) -> Result<Workbook> {
        conn.execute_batch(&format!("ATTACH ':memory:' AS {SCRATCH};"))?;
        // Show TIMESTAMPTZ values in UTC so results do not depend on the server's zone.
        let _ = conn.execute_batch("SET TimeZone = 'UTC';");
        // Lets long statements (imports, sorts) report progress to the UI.
        conn.set_query_progress_tracking(true)?;
        let bytes: Option<Vec<u8>> = conn.query_row("SELECT data FROM _rf_sheets LIMIT 1", [], |r| r.get(0)).ok();
        let sheets = Sheets::from_bytes(bytes)?;
        Ok(Workbook { conn, shared: Arc::new(Shared::new(sheets)), txn_depth: 0, sheet_epoch: 0, after_commit: vec![], report })
    }

    /// What opening did (a migration, and where the pre-migration copy is).
    pub fn open_report(&self) -> &OpenReport {
        &self.report
    }

    /// A new reader connection sharing this workbook's views.
    pub fn reader(&self) -> Result<Reader> {
        let conn = self.conn.try_clone()?;
        let _ = conn.execute_batch("SET TimeZone = 'UTC';");
        conn.set_query_progress_tracking(true)?;
        Ok(Reader { conn, shared: self.shared.clone() })
    }

    pub fn shared(&self) -> &Shared {
        &self.shared
    }

    fn db(&self) -> Db<'_> {
        let in_txn = self.txn_depth > 0;
        if !in_txn {
            self.shared.sweep(&self.conn);
        }
        Db { conn: &self.conn, shared: &self.shared, in_txn }
    }

    /// Apply DuckDB settings such as `memory_limit` or `threads`.
    pub fn configure(&self, settings: &[(&str, String)]) -> Result<()> {
        for (key, value) in settings {
            self.conn.execute_batch(&format!("SET {key} = {};", lit(value)))?;
        }
        Ok(())
    }

    /// Current value of a DuckDB setting.
    pub fn setting(&self, key: &str) -> Result<String> {
        Ok(self.conn.query_row("SELECT current_setting(?)::VARCHAR", [key], |r| r.get(0))?)
    }

    /// Handle for reading the progress of the statement running on this workbook.
    pub fn progress_handle(&self) -> duckdb::progress::ProgressHandle {
        self.conn.progress_handle()
    }

    /// Handle that interrupts the statement running on this workbook.
    pub fn interrupt_handle(&self) -> Arc<duckdb::InterruptHandle> {
        self.conn.interrupt_handle()
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.conn.execute_batch("CHECKPOINT;")?;
        Ok(())
    }

    /// The last committed revision.
    pub fn revision(&self) -> Result<u64> {
        storage::revision(&self.conn)
    }

    /// History entries after `after`, oldest first.
    pub fn history(&self, after: u64, limit: usize) -> Result<Vec<HistoryEntry>> {
        storage::history(&self.conn, after, limit)
    }

    /// Bind the file to a workbook id the first time, and refuse a file bound to another.
    pub fn bind_identity(&self, workbook_id: &str) -> Result<()> {
        match storage::meta_get(&self.conn, "workbook_id")? {
            Some(stored) if stored != workbook_id => {
                bail!(storage::WrongWorkbook { stored, expected: workbook_id.to_string() })
            }
            Some(_) => Ok(()),
            None => storage::meta_set(&self.conn, "workbook_id", workbook_id),
        }
    }

    /// Bind a copy of a workbook's file to the new workbook it now belongs to.
    pub fn rebind_identity(&self, workbook_id: &str) -> Result<()> {
        storage::meta_set(&self.conn, "workbook_id", workbook_id)?;
        self.checkpoint()
    }

    pub fn identity(&self) -> Result<Option<String>> {
        storage::meta_get(&self.conn, "workbook_id")
    }

    // ------------------------------------------------------------ mutations

    /// Apply a change as one transaction together with the next revision and its history
    /// entry. A mutation id already in the history returns the recorded outcome without
    /// applying anything. `view` names a view the change reads positions from; it is
    /// brought up to date first, because building a permutation writes the scratch
    /// database and a transaction may write only one database.
    pub fn mutate(&mut self, m: Mutation, view: Option<u64>, f: impl FnOnce(&mut Self) -> Result<Value>) -> Result<Committed> {
        if let Some(id) = m.id {
            if let Some((revision, result)) = storage::find_mutation(&self.conn, id)? {
                let result = result.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or(Value::Null);
                return Ok(Committed { revision, result, replayed: true });
            }
        }
        let keep = m.id.is_some();
        let (revision, result) = self.run_mutation(&m, view, f, |v| if keep { Some(v.to_string()) } else { None })?;
        Ok(Committed { revision, result, replayed: false })
    }

    /// Run a public mutation method: inside an outer [`Workbook::mutate`] it is part of
    /// that transaction, otherwise it gets one (and a revision) of its own.
    fn mutating<T>(&mut self, op: &str, view: Option<u64>, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        if self.txn_depth > 0 {
            return f(self);
        }
        let m = Mutation { id: None, op, actor: None, summary: op.to_string() };
        Ok(self.run_mutation(&m, view, f, |_| None)?.1)
    }

    fn run_mutation<T>(
        &mut self,
        m: &Mutation,
        view: Option<u64>,
        f: impl FnOnce(&mut Self) -> Result<T>,
        encode: impl FnOnce(&T) -> Option<String>,
    ) -> Result<(u64, T)> {
        if self.txn_depth > 0 {
            bail!("a change is already in progress on this connection");
        }
        self.shared.sweep(&self.conn);
        if let Some(v) = view {
            let entry = self.shared.view(v)?;
            self.db().materialize(&entry)?;
        }
        let epoch = self.sheet_epoch;
        self.after_commit.clear();
        self.conn.execute_batch("BEGIN TRANSACTION;")?;
        self.txn_depth = 1;
        let result = f(self).and_then(|out| {
            let encoded = encode(&out);
            let rev = storage::record(&self.conn, m.id, m.op, m.actor, &m.summary, encoded.as_deref().unwrap_or(""))?;
            faults::check("commit")?;
            Ok((rev, out))
        });
        self.txn_depth = 0;
        let result = result.and_then(|out| match self.conn.execute_batch("COMMIT;") {
            Ok(()) => Ok(out),
            Err(e) => Err(anyhow!(e).context("commit failed")),
        });
        match result {
            Ok(out) => {
                self.apply_after_commit();
                Ok(out)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK;");
                self.after_commit.clear();
                if self.sheet_epoch != epoch {
                    match self.load_sheets() {
                        Ok(s) => *self.shared.sheets.lock().unwrap_or_else(|e| e.into_inner()) = s,
                        Err(re) => eprintln!("rowfinity-engine: could not reload sheets after a failed change: {re:#}"),
                    }
                }
                Err(e)
            }
        }
    }

    fn apply_after_commit(&mut self) {
        for action in std::mem::take(&mut self.after_commit) {
            match action {
                AfterCommit::Index(table, mut index) => {
                    if let Ok(meta) = storage::table_meta(&self.conn, &table) {
                        index.gen = meta.gen;
                        if let Ok(index) = index.split_large(&self.conn, &table) {
                            self.shared.store_index(&table, index);
                        }
                    }
                }
                AfterCommit::RenameTable { from, to } => self.shared.rewrite_specs(&from, |s| s.table = to.clone()),
                AfterCommit::RenameColumn { table, from, to } => self.shared.rewrite_specs(&table, |spec| {
                    for k in &mut spec.sort {
                        if k.column == from {
                            k.column = to.clone();
                        }
                    }
                    for f in &mut spec.filters {
                        if f.column == from {
                            f.column = to.clone();
                        }
                    }
                    for h in &mut spec.hidden {
                        if *h == from {
                            *h = to.clone();
                        }
                    }
                }),
                AfterCommit::PruneSpecs(table) => {
                    let Ok(cols) = self.db().columns(&table) else { continue };
                    let names: Vec<String> = cols.into_iter().map(|c| c.name).collect();
                    self.shared.rewrite_specs(&table, |spec| {
                        spec.hidden.retain(|h| names.contains(h));
                        spec.sort.retain(|k| names.contains(&k.column));
                        spec.filters.retain(|f| names.contains(&f.column));
                    });
                }
                AfterCommit::ForgetTable(table) => self.shared.forget_table(&table),
            }
        }
    }

    // ---------------------------------------------------------------- tables

    pub fn list_tables(&self) -> Result<Vec<TableInfo>> {
        self.db().list_tables()
    }

    fn table_exists(&self, name: &str) -> Result<bool> {
        self.db().table_exists(name)
    }

    fn unique_table_name(&self, raw: &str) -> Result<String> {
        let base = sanitize_table_name(raw);
        let mut name = base.clone();
        let mut i = 2;
        while self.table_exists(&name)? {
            name = format!("{base}_{i}");
            i += 1;
        }
        Ok(name)
    }

    pub fn table_info(&self, table: &str) -> Result<TableInfo> {
        self.db().table_info(table)
    }

    /// Import a CSV/TSV/Parquet/JSON(L) file as a new table.
    pub fn import_file(&mut self, path: &Path, table_name: Option<&str>) -> Result<TableInfo> {
        self.import_file_with(path, table_name, &[])
    }

    /// Import a CSV/TSV/Parquet/JSON(L) file as a new table, with optional per-column
    /// types. Failures become [`import::ImportProblem`]s that say what to change.
    pub fn import_file_with(&mut self, path: &Path, table_name: Option<&str>, types: &[ColumnType]) -> Result<TableInfo> {
        if path.to_string_lossy().to_ascii_lowercase().ends_with(".xlsx") {
            bail!("use import_any for .xlsx files (they become sheets)");
        }
        self.check_type_overrides(path, types)?;
        let select = import::reader_select(path, types)?;
        let name = self.unique_table_name(table_name.filter(|n| !n.trim().is_empty()).unwrap_or(&import::suggested_name(path)))?;
        self.mutating("import", None, |wb| {
            faults::check("import")?;
            wb.create_table_from_select(&name, &select).map_err(import::import_error)?;
            wb.table_info(&name)
        })
    }

    /// Detected columns and the first rows of a file, as they would be imported.
    pub fn preview_file(&self, path: &Path, types: &[ColumnType]) -> Result<FilePreview> {
        let db = self.db();
        let detected = db.describe(&import::reader_select(path, &[])?).map_err(import::import_error)?;
        self.check_type_overrides(path, types)?;
        let select = import::reader_select(path, types)?;
        let rows = db.run_preview(&select, import::PREVIEW_ROWS).map_err(import::import_error)?;
        let columns = rows
            .columns
            .iter()
            .map(|c| {
                let i = detected.iter().position(|d| d.name == c.name);
                PreviewColumn {
                    name: c.name.clone(),
                    sql_type: c.sql_type.clone(),
                    kind: c.kind,
                    detected: i.map(|i| detected[i].sql_type.clone()).unwrap_or_else(|| c.sql_type.clone()),
                }
            })
            .collect();
        Ok(FilePreview {
            format: import::format_label(path).to_string(),
            suggested_name: import::suggested_name(path),
            columns,
            rows: rows.rows,
            type_choices: import::TYPE_CHOICES.iter().map(|t| t.to_string()).collect(),
        })
    }

    fn check_type_overrides(&self, path: &Path, types: &[ColumnType]) -> Result<()> {
        if types.is_empty() {
            return Ok(());
        }
        let detected = self.db().describe(&import::reader_select(path, &[])?).map_err(import::import_error)?;
        for t in types {
            if !detected.iter().any(|c| c.name == t.name) {
                bail!("the file has no column {:?}", t.name);
            }
        }
        Ok(())
    }

    /// Preview any supported upload: a table preview, or the sheets an .xlsx would add.
    pub fn preview_any(&self, path: &Path, types: &[ColumnType]) -> Result<UploadPreview> {
        if path.to_string_lossy().to_ascii_lowercase().ends_with(".xlsx") {
            let (sheets, warnings) = Sheets::preview_xlsx(path)?;
            Ok(UploadPreview::Sheets { sheets, warnings })
        } else {
            Ok(UploadPreview::Table { preview: self.preview_file(path, types)? })
        }
    }

    /// Materialize any SELECT as a new table (e.g. a group-by or a join).
    pub fn create_table_from_query(&mut self, name: &str, sql: &str) -> Result<TableInfo> {
        let name = self.unique_table_name(name)?;
        let select = self.db().expand_query(sql)?;
        self.mutating("create_table_from_query", None, |wb| {
            wb.create_table_from_select(&name, &select)?;
            wb.table_info(&name)
        })
    }

    /// New table from a SELECT, keeping its row order. Source columns that use an internal
    /// name are renamed rather than dropped.
    fn create_table_from_select(&mut self, name: &str, select: &str) -> Result<()> {
        let cols = self.db().describe(select)?;
        let renames: Vec<String> = cols
            .iter()
            .filter(|c| is_internal(&c.name))
            .map(|c| format!("{} AS {}", ident(&c.name), ident(&self.free_name(&cols, c.name.trim_start_matches('_')))))
            .collect();
        let rest = if renames.is_empty() { "*".to_string() } else { format!("* RENAME ({})", renames.join(", ")) };
        // A streaming row_number() keeps the source order.
        self.conn.execute_batch(&format!(
            "CREATE TABLE {} AS SELECT (row_number() OVER () - 1)::BIGINT AS {ID}, {ID} * {ORD_GAP} AS {ORD}, {rest} FROM ({select});",
            ident(name)
        ))?;
        let rows = self.db().count_rows(name)?;
        storage::register_table(&self.conn, name, rows as i64)
    }

    fn free_name(&self, cols: &[Column], base: &str) -> String {
        let mut name = base.to_string();
        let mut i = 2;
        while cols.iter().any(|c| c.name.eq_ignore_ascii_case(&name)) {
            name = format!("{base}_{i}");
            i += 1;
        }
        name
    }

    /// Generate a synthetic flights table, handy for trying big-data behaviour.
    pub fn create_demo_table(&mut self, rows: u64) -> Result<TableInfo> {
        let rows = rows.min(limits::MAX_DEMO_ROWS);
        let name = self.unique_table_name("flights")?;
        self.mutating("create_demo_table", None, |wb| {
            let airports = "['ATL','DFW','DEN','ORD','LAX','JFK','LAS','MCO','MIA','CLT','SEA','PHX','EWR','SFO','IAH','BOS','FLL','MSP','LGA','DTW']";
            wb.conn.execute_batch(&format!(
                "CREATE TABLE {table} AS
                 SELECT i::BIGINT AS {ID}, (i * {ORD_GAP})::BIGINT AS {ORD},
                   (DATE '2022-01-01' + CAST((i * 1096) // {rows} AS INTEGER)) AS flight_date,
                   (['AA','DL','UA','WN','B6','AS','NK','F9'])[1 + (hash(i) % 8)::INTEGER] AS carrier,
                   carrier || CAST(100 + hash(i + 23) % 4900 AS VARCHAR) AS flight_number,
                   ({airports})[1 + (hash(i * 2 + 1) % 20)::INTEGER] AS origin,
                   ({airports})[1 + (hash(i * 3 + 7) % 20)::INTEGER] AS dest,
                   ((hash(i + 11) % 180)::INTEGER - 30) AS dep_delay_min,
                   (200 + hash(i + 5) % 2500)::INTEGER AS distance_mi,
                   round(49 + (hash(i + 3) % 45000) / 100.0, 2) AS fare_usd,
                   (hash(i + 17) % 100) < 2 AS cancelled
                 FROM range({rows}) t(i);",
                table = ident(&name),
            ))?;
            storage::register_table(&wb.conn, &name, rows as i64)?;
            wb.table_info(&name)
        })
    }

    pub fn drop_table(&mut self, table: &str) -> Result<()> {
        if !self.table_exists(table)? {
            bail!("unknown table {table:?}");
        }
        self.mutating("drop_table", None, |wb| {
            wb.conn.execute("DELETE FROM _rf_views WHERE table_name = ?", duckdb::params![table])?;
            wb.conn.execute_batch(&format!(
                "DROP TABLE {}; DELETE FROM _rf_columns WHERE table_name = {};",
                ident(table),
                lit(table)
            ))?;
            storage::forget_table(&wb.conn, table)?;
            wb.after_commit.push(AfterCommit::ForgetTable(table.to_string()));
            Ok(())
        })
    }

    pub fn rename_table(&mut self, table: &str, new_name: &str) -> Result<String> {
        if !self.table_exists(table)? {
            bail!("unknown table {table:?}");
        }
        let new_name = self.unique_table_name(new_name)?;
        self.mutating("rename_table", None, |wb| {
            wb.conn.execute_batch(&format!(
                "ALTER TABLE {} RENAME TO {}; UPDATE _rf_columns SET table_name = {} WHERE table_name = {};",
                ident(table),
                ident(&new_name),
                lit(&new_name),
                lit(table)
            ))?;
            storage::rename_table(&wb.conn, table, &new_name)?;
            wb.rewrite_saved_views(table, |spec, _| {
                spec.table = new_name.clone();
                Ok(())
            })?;
            wb.after_commit.push(AfterCommit::RenameTable { from: table.to_string(), to: new_name.clone() });
            Ok(new_name)
        })
    }

    // --------------------------------------------------------------- columns

    pub fn columns(&self, table: &str) -> Result<Vec<Column>> {
        self.db().columns(table)
    }

    fn formulas(&self, table: &str) -> Result<Vec<(String, String)>> {
        self.db().formulas(table)
    }

    fn base_select(&self, table: &str) -> Result<String> {
        self.db().base_select(table)
    }

    pub fn add_column(&mut self, table: &str, name: &str, sql_type: &str) -> Result<()> {
        self.ensure_new_column(table, name, None)?;
        let ty = sql_type.trim();
        if ty.is_empty() || !ty.chars().all(|c| c.is_ascii_alphanumeric() || " (),".contains(c)) {
            bail!("invalid column type {sql_type:?}");
        }
        self.mutating("add_column", None, |wb| {
            wb.conn.execute_batch(&format!("ALTER TABLE {} ADD COLUMN {} {ty};", ident(table), ident(name)))?;
            storage::update_table(&wb.conn, table, None, None)
        })
    }

    /// Add a formula column. The formula is a DuckDB expression; `[Col]` refers to a column.
    pub fn add_formula_column(&mut self, table: &str, name: &str, formula: &str) -> Result<()> {
        self.ensure_new_column(table, name, None)?;
        formula::analyze(&self.conn, formula)?;
        let probe = format!("SELECT ({}) AS {} FROM ({}) LIMIT 1", formula_to_sql(formula), ident(name), self.base_select(table)?);
        self.conn.execute_batch(&probe).map_err(|e| anyhow!("formula error: {e}"))?;
        self.mutating("add_formula_column", None, |wb| {
            wb.conn.execute(
                "INSERT INTO _rf_columns VALUES (?, ?, ?, (SELECT coalesce(max(position) + 1, 0) FROM _rf_columns WHERE table_name = ?))",
                duckdb::params![table, name, formula, table],
            )?;
            storage::update_table(&wb.conn, table, None, None)
        })
    }

    pub fn update_formula(&mut self, table: &str, name: &str, formula: &str) -> Result<()> {
        if !self.formulas(table)?.iter().any(|(n, _)| n == name) {
            bail!("{name:?} is not a formula column");
        }
        formula::analyze(&self.conn, formula)?;
        self.mutating("update_formula", None, |wb| {
            wb.conn.execute(
                "UPDATE _rf_columns SET formula = ? WHERE table_name = ? AND name = ?",
                duckdb::params![formula, table, name],
            )?;
            wb.db().describe(&wb.base_select(table)?).map_err(|e| anyhow!("formula error: {e}"))?;
            storage::update_table(&wb.conn, table, None, None)
        })
    }

    /// Formula columns of `table`, other than `column` itself, that reference `column`.
    fn dependents(&self, table: &str, column: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for (name, f) in self.formulas(table)? {
            if !formula::same_name(&name, column) && formula::references(&self.conn, &f, column)? {
                out.push(name);
            }
        }
        Ok(out)
    }

    /// Delete a column. Fails, without changing anything, while formula columns use it.
    pub fn drop_column(&mut self, table: &str, name: &str) -> Result<()> {
        if !self.columns(table)?.iter().any(|c| c.name == name) {
            bail!("unknown column {name:?}");
        }
        let users = self.dependents(table, name)?;
        if !users.is_empty() {
            bail!("{name:?} is used by formula column{} {}; change or delete {} first",
                if users.len() == 1 { "" } else { "s" },
                users.iter().map(|u| format!("{u:?}")).collect::<Vec<_>>().join(", "),
                if users.len() == 1 { "it" } else { "them" });
        }
        let in_views: Vec<String> = self
            .saved_views(Some(table))?
            .into_iter()
            .filter(|v| v.spec.sort.iter().any(|k| k.column == name) || v.spec.filters.iter().any(|f| f.column == name))
            .map(|v| format!("{:?}", v.name))
            .collect();
        if !in_views.is_empty() {
            bail!("{name:?} sorts or filters the saved view{} {}; change or delete {} first",
                if in_views.len() == 1 { "" } else { "s" }, in_views.join(", "), if in_views.len() == 1 { "it" } else { "them" });
        }
        let is_formula = self.formulas(table)?.iter().any(|(n, _)| n == name);
        self.mutating("drop_column", None, |wb| {
            wb.rewrite_saved_views(table, |spec, widths| {
                spec.hidden.retain(|h| h != name);
                widths.remove(name);
                Ok(())
            })?;
            if is_formula {
                wb.conn.execute("DELETE FROM _rf_columns WHERE table_name = ? AND name = ?", duckdb::params![table, name])?;
            } else {
                wb.conn.execute_batch(&format!("ALTER TABLE {} DROP COLUMN {};", ident(table), ident(name)))?;
            }
            faults::check("drop_column")?;
            wb.db().describe(&wb.base_select(table)?)?;
            storage::update_table(&wb.conn, table, None, None)?;
            wb.after_commit.push(AfterCommit::PruneSpecs(table.to_string()));
            Ok(())
        })
    }

    /// Rename a column and rewrite every formula that refers to it, atomically.
    pub fn rename_column(&mut self, table: &str, name: &str, new_name: &str) -> Result<()> {
        let new_name = new_name.trim();
        if !self.columns(table)?.iter().any(|c| c.name == name) {
            bail!("unknown column {name:?}");
        }
        self.ensure_new_column(table, new_name, Some(name))?;
        if name == new_name {
            return Ok(());
        }
        let formulas = self.formulas(table)?;
        let is_formula = formulas.iter().any(|(n, _)| n == name);
        let mut rewritten = Vec::new();
        for (fname, f) in &formulas {
            if let Some(updated) = formula::rename_reference(&self.conn, f, name, new_name)? {
                rewritten.push((fname.clone(), updated));
            }
        }
        self.mutating("rename_column", None, |wb| {
            if is_formula {
                wb.conn.execute(
                    "UPDATE _rf_columns SET name = ? WHERE table_name = ? AND name = ?",
                    duckdb::params![new_name, table, name],
                )?;
            } else {
                wb.conn.execute_batch(&format!(
                    "ALTER TABLE {} RENAME COLUMN {} TO {};",
                    ident(table),
                    ident(name),
                    ident(new_name)
                ))?;
            }
            for (fname, updated) in &rewritten {
                let fname = if fname == name { new_name } else { fname.as_str() };
                wb.conn.execute(
                    "UPDATE _rf_columns SET formula = ? WHERE table_name = ? AND name = ?",
                    duckdb::params![updated, table, fname],
                )?;
            }
            wb.rewrite_saved_views(table, |spec, widths| {
                for k in &mut spec.sort {
                    if k.column == name {
                        k.column = new_name.to_string();
                    }
                }
                for f in &mut spec.filters {
                    if f.column == name {
                        f.column = new_name.to_string();
                    }
                }
                for h in &mut spec.hidden {
                    if h == name {
                        *h = new_name.to_string();
                    }
                }
                if let Some(w) = widths.remove(name) {
                    widths.insert(new_name.to_string(), w);
                }
                Ok(())
            })?;
            faults::check("rename_column")?;
            wb.db().describe(&wb.base_select(table)?).map_err(|e| anyhow!("rename would break a formula: {e}"))?;
            storage::update_table(&wb.conn, table, None, None)?;
            wb.after_commit.push(AfterCommit::RenameColumn { table: table.to_string(), from: name.to_string(), to: new_name.to_string() });
            Ok(())
        })
    }

    fn ensure_new_column(&self, table: &str, name: &str, renaming: Option<&str>) -> Result<()> {
        if name.trim().is_empty() || is_internal(&name.to_ascii_lowercase()) {
            bail!("invalid column name");
        }
        let clash = self
            .columns(table)?
            .iter()
            .any(|c| formula::same_name(&c.name, name) && Some(c.name.as_str()) != renaming);
        if clash {
            bail!("column {name:?} already exists");
        }
        Ok(())
    }

    // ----------------------------------------------------------- saved views

    /// Saved views, of one table or of all tables, by name.
    pub fn saved_views(&self, table: Option<&str>) -> Result<Vec<SavedView>> {
        self.db().saved_views(table)
    }

    /// Create a saved view, or update the one with `id`. The spec must be valid (columns
    /// exist) and names are unique per table.
    pub fn save_view(
        &mut self,
        id: Option<&str>,
        name: &str,
        spec: ViewSpec,
        widths: std::collections::BTreeMap<String, f64>,
    ) -> Result<SavedView> {
        let name = name.trim();
        if name.is_empty() {
            bail!("a view needs a name");
        }
        self.db().validate_spec(&spec)?;
        let clash = self
            .saved_views(Some(&spec.table))?
            .into_iter()
            .any(|v| v.name.to_lowercase() == name.to_lowercase() && Some(v.id.as_str()) != id);
        if clash {
            bail!("a view named {name:?} already exists for this table");
        }
        let id = match id {
            Some(id) => {
                if !self.saved_views(None)?.iter().any(|v| v.id == id) {
                    bail!("unknown saved view {id}");
                }
                id.to_string()
            }
            None => format!("v{:x}{:04x}", now_secs(), self.saved_views(None)?.len()),
        };
        let view = SavedView { id: id.clone(), name: name.to_string(), spec, widths, updated_at: now_secs() };
        self.mutating("save_view", None, |wb| {
            wb.conn.execute("DELETE FROM _rf_views WHERE id = ?", [&view.id])?;
            wb.conn.execute(
                "INSERT INTO _rf_views VALUES (?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    view.id,
                    view.name,
                    view.spec.table,
                    serde_json::to_string(&view.spec)?,
                    serde_json::to_string(&view.widths)?,
                    view.updated_at as i64
                ],
            )?;
            Ok(view)
        })
    }

    pub fn rename_saved_view(&mut self, id: &str, name: &str) -> Result<SavedView> {
        let v = self.saved_views(None)?.into_iter().find(|v| v.id == id).ok_or_else(|| anyhow!("unknown saved view {id}"))?;
        self.save_view(Some(id), name, v.spec, v.widths)
    }

    pub fn delete_saved_view(&mut self, id: &str) -> Result<()> {
        self.mutating("delete_saved_view", None, |wb| {
            let n = wb.conn.execute("DELETE FROM _rf_views WHERE id = ?", duckdb::params![id])?;
            if n == 0 {
                bail!("unknown saved view {id}");
            }
            Ok(())
        })
    }

    /// Apply `f` to every saved view of `table` (inside the caller's transaction).
    fn rewrite_saved_views(
        &self,
        table: &str,
        mut f: impl FnMut(&mut ViewSpec, &mut std::collections::BTreeMap<String, f64>) -> Result<()>,
    ) -> Result<()> {
        for mut v in self.saved_views(Some(table))? {
            f(&mut v.spec, &mut v.widths)?;
            self.conn.execute(
                "UPDATE _rf_views SET table_name = ?, spec = ?, widths = ? WHERE id = ?",
                duckdb::params![v.spec.table, serde_json::to_string(&v.spec)?, serde_json::to_string(&v.widths)?, v.id],
            )?;
        }
        Ok(())
    }

    // ----------------------------------------------------------------- views

    pub fn open_view(&mut self, spec: ViewSpec) -> Result<ViewInfo> {
        open_view(&self.db(), spec)
    }

    pub fn update_view(&mut self, id: u64, spec: ViewSpec) -> Result<ViewInfo> {
        update_view(&self.db(), id, spec)
    }

    pub fn close_view(&mut self, id: u64) {
        self.shared.remove_view(id);
        if self.txn_depth == 0 {
            self.shared.sweep(&self.conn);
        }
    }

    pub fn view_info(&self, id: u64) -> Result<ViewInfo> {
        self.db().view_info(id)
    }

    pub fn fetch(&self, id: u64, row_start: u64, row_count: u64) -> Result<RowBlock> {
        self.db().fetch(id, row_start, row_count, 0, None)
    }

    pub fn fetch_columns(&self, id: u64, row_start: u64, row_count: u64, column_start: u32, column_count: Option<u32>) -> Result<RowBlock> {
        self.db().fetch(id, row_start, row_count, column_start, column_count)
    }

    /// Count/sum/avg/min/max of a column over view rows `[start, end)`.
    pub fn stats(&self, id: u64, column: &str, start: u64, end: u64) -> Result<Stats> {
        self.db().stats(id, column, start, end)
    }

    /// Check the cached position index of `table` against its data (tests).
    pub fn verify_index(&self, table: &str) -> Result<bool> {
        let index = self.db().index(table)?;
        index.verify(&self.conn, table)?;
        Ok(index.is_dense())
    }

    // ----------------------------------------------------------------- edits

    /// Apply cell edits as one transaction. Typing below the last row appends rows (in the
    /// same transaction). Every value is validated before anything changes; repeated
    /// cells in one batch keep the last value; an empty batch changes nothing.
    pub fn edit(&mut self, id: u64, edits: &[CellEdit]) -> Result<EditOutcome> {
        if edits.len() > limits::MAX_EDIT_CELLS {
            bail!("too many cells in one edit ({}; the limit is {})", edits.len(), limits::MAX_EDIT_CELLS);
        }
        self.mutating("edit", Some(id), |wb| wb.edit_in_txn(id, edits))
    }

    fn edit_in_txn(&mut self, id: u64, edits: &[CellEdit]) -> Result<EditOutcome> {
        let entry = self.shared.view(id)?;
        let m = self.db().materialize(&entry)?;
        let table = m.spec.table.clone();
        if edits.is_empty() {
            return Ok(EditOutcome { table, cells_changed: 0, rows_appended: 0 });
        }
        let by_name: HashMap<String, Column> = m.columns.iter().map(|c| (c.name.clone(), c.clone())).collect();
        for e in edits {
            let col = by_name.get(&e.column).ok_or_else(|| anyhow!("unknown column {:?}", e.column))?;
            if col.is_computed() {
                bail!("{:?} is a formula column; edit its formula instead", col.name);
            }
        }
        // Where each cell goes: an existing record, or an appended row (by position).
        #[derive(Clone, Copy, PartialEq, Eq, Hash)]
        enum Target {
            Record(i64),
            Position(u64),
        }
        let target = |e: &CellEdit| match e.record {
            Some(r) => Target::Record(r),
            None => Target::Position(e.row),
        };
        // Last write wins for repeated cells; keep first-seen order otherwise.
        let mut cells: Vec<(Target, &str, Option<&str>)> = Vec::new();
        let mut seen: HashMap<(Target, &str), usize> = HashMap::new();
        for e in edits {
            let value = e.value.as_deref().filter(|s| !s.is_empty());
            let t = target(e);
            match seen.get(&(t, e.column.as_str())) {
                Some(&i) => cells[i].2 = value,
                None => {
                    seen.insert((t, e.column.as_str()), cells.len());
                    cells.push((t, e.column.as_str(), value));
                }
            }
        }
        let max_row = cells.iter().filter_map(|c| if let Target::Position(p) = c.0 { Some(p) } else { None }).max();
        let appended = match max_row {
            Some(r) if r >= m.row_count => {
                if m.perm.is_some() {
                    bail!("clear sorting and filters to add rows");
                }
                let n = r + 1 - m.row_count;
                if n > limits::MAX_INSERT_ROWS {
                    bail!("that edit would add {n} rows; the limit is {}", limits::MAX_INSERT_ROWS);
                }
                n
            }
            _ => 0,
        };

        // Validate every value before touching the table.
        let mut by_column: HashMap<&str, Vec<(Target, Option<&str>)>> = HashMap::new();
        for (t, column, value) in &cells {
            by_column.entry(column).or_default().push((*t, *value));
        }
        for (column, values) in &by_column {
            let col = &by_name[*column];
            for value in values.iter().filter_map(|(_, v)| *v) {
                crate::wire::check_exact_input(col, value).map_err(|e| anyhow!(e))?;
            }
            let texts: Vec<String> = values.iter().filter_map(|(_, v)| v.map(lit)).collect();
            if texts.is_empty() {
                continue;
            }
            let bad: Option<String> = self
                .conn
                .query_row(
                    &format!(
                        "SELECT val FROM (VALUES ({})) v(val) WHERE TRY_CAST(val AS {}) IS NULL LIMIT 1",
                        texts.join("),("),
                        col.sql_type
                    ),
                    [],
                    |r| r.get(0),
                )
                .ok();
            if let Some(bad) = bad {
                bail!("{bad:?} is not a valid {} for column {:?}", col.sql_type, col.name);
            }
        }

        // Targets -> record ids. Positions inside the view are mapped in contiguous runs.
        let mut positions: Vec<u64> = cells
            .iter()
            .filter_map(|c| if let Target::Position(p) = c.0 { Some(p) } else { None })
            .filter(|&p| p < m.row_count)
            .collect();
        positions.sort_unstable();
        positions.dedup();
        let mut pos_to_id: HashMap<u64, i64> = HashMap::new();
        let mut i = 0;
        while i < positions.len() {
            let mut j = i;
            while j + 1 < positions.len() && positions[j + 1] == positions[j] + 1 {
                j += 1;
            }
            let ids = self.db().ids_for_range(&m, positions[i], positions[j] + 1)?;
            if ids.len() as u64 != positions[j] + 1 - positions[i] {
                bail!("the table changed while the edit was prepared; try again");
            }
            for (k, rid) in ids.into_iter().enumerate() {
                pos_to_id.insert(positions[i] + k as u64, rid);
            }
            i = j + 1;
        }
        let records: BTreeSet<i64> = cells.iter().filter_map(|c| if let Target::Record(r) = c.0 { Some(r) } else { None }).collect();
        if !records.is_empty() {
            let list = records.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(",");
            let found: i64 =
                self.conn.query_row(&format!("SELECT count(*) FROM main.{} WHERE {ID} IN ({list})", ident(&table)), [], |r| r.get(0))?;
            if found as usize != records.len() {
                bail!("a row being edited was deleted by someone else; nothing was changed");
            }
        }
        // Appended rows get new ids and order keys after the last row.
        let meta = storage::table_meta(&self.conn, &table)?;
        let index = self.db().index(&table)?;
        let mut new_ords = Vec::new();
        if appended > 0 {
            let first = m.row_count;
            let base_ord = if index.is_dense() {
                first as i64 * ORD_GAP
            } else if first == 0 {
                0
            } else {
                index.ord_at(&self.conn, &table, first - 1)? + ORD_GAP
            };
            for k in 0..appended {
                let rid = meta.next_id + k as i64;
                pos_to_id.insert(first + k, rid);
                new_ords.push(base_ord + k as i64 * ORD_GAP);
            }
        }
        let rid_of = |t: Target| -> Result<i64> {
            match t {
                Target::Record(r) => Ok(r),
                Target::Position(p) => pos_to_id.get(&p).copied().ok_or_else(|| anyhow!("row {p} is outside the view")),
            }
        };
        let mut updates: Vec<String> = Vec::new();
        for (column, values) in &by_column {
            let col = &by_name[*column];
            let mut rows = Vec::with_capacity(values.len());
            for (t, val) in values {
                rows.push(format!("({}, {})", rid_of(*t)?, val.map(lit).unwrap_or_else(|| "NULL::VARCHAR".into())));
            }
            updates.push(format!(
                "UPDATE {t} SET {c} = CAST(v.val AS {ty}) FROM (VALUES {}) v(id, val) WHERE {t}.{ID} = v.id;",
                rows.join(","),
                t = ident(&table),
                c = ident(&col.name),
                ty = col.sql_type,
            ));
        }
        let cells_changed = cells.len();
        if appended > 0 {
            self.conn.execute_batch(&format!(
                "INSERT INTO {} ({ID}, {ORD}) SELECT {} + i, {} + i * {ORD_GAP} FROM range({appended}) t(i);",
                ident(&table),
                meta.next_id,
                new_ords[0]
            ))?;
            storage::update_table(&self.conn, &table, Some(meta.next_id + appended as i64), None)?;
            self.after_commit.push(AfterCommit::Index(table.clone(), index.appended(appended)));
        }
        for sql in &updates {
            faults::check("edit")?;
            self.conn.execute_batch(sql)?;
        }
        Ok(EditOutcome { table, cells_changed, rows_appended: appended })
    }

    /// Insert `count` empty rows before view position `at` (clamped to the end). Only on
    /// unsorted, unfiltered views. No other row's id or data changes.
    pub fn insert_rows(&mut self, id: u64, at: u64, count: u64) -> Result<String> {
        if count > limits::MAX_INSERT_ROWS {
            bail!("cannot insert {count} rows at once; the limit is {}", limits::MAX_INSERT_ROWS);
        }
        self.mutating("insert_rows", Some(id), |wb| wb.insert_in_txn(id, at, count))
    }

    fn insert_in_txn(&mut self, id: u64, at: u64, count: u64) -> Result<String> {
        let entry = self.shared.view(id)?;
        let m = self.db().materialize(&entry)?;
        if m.perm.is_some() {
            bail!("clear sorting and filters to insert rows");
        }
        let table = m.spec.table.clone();
        if count == 0 {
            return Ok(table);
        }
        let t = ident(&table);
        let meta = storage::table_meta(&self.conn, &table)?;
        let index = self.db().index(&table)?;
        let rows = index.rows;
        let at = at.min(rows);
        let k = count as i64;
        let next_id = meta.next_id;
        if at == rows {
            let base = if index.is_dense() {
                rows as i64 * ORD_GAP
            } else if rows == 0 {
                0
            } else {
                index.ord_at(&self.conn, &table, rows - 1)? + ORD_GAP
            };
            self.conn.execute_batch(&format!(
                "INSERT INTO {t} ({ID}, {ORD}) SELECT {next_id} + i, {base} + i * {ORD_GAP} FROM range({count}) r(i);"
            ))?;
            storage::update_table(&self.conn, &table, Some(next_id + k), None)?;
            self.after_commit.push(AfterCommit::Index(table.clone(), index.appended(count)));
            return Ok(table);
        }
        let next = index.ord_at(&self.conn, &table, at)?;
        let lo = if at > 0 { index.ord_at(&self.conn, &table, at - 1)? } else { next.saturating_sub((k + 1).saturating_mul(ORD_GAP)) };
        if (next as i128 - lo as i128) > k as i128 {
            // Spread the new keys evenly in the gap: lo < key_i < next.
            let ords: Vec<i64> = (0..k).map(|i| (lo as i128 + (next as i128 - lo as i128) * (i as i128 + 1) / (k as i128 + 1)) as i64).collect();
            self.conn.execute_batch(&format!(
                "INSERT INTO {t} ({ID}, {ORD})
                 SELECT {next_id} + i, ({lo}::HUGEINT + ({next}::HUGEINT - {lo}) * (i + 1) // {})::BIGINT FROM range({count}) r(i);",
                k + 1
            ))?;
            storage::update_table(&self.conn, &table, Some(next_id + k), Some(false))?;
            self.after_commit.push(AfterCommit::Index(table.clone(), index.inserted(&ords)));
        } else {
            self.rebalance_insert(&table, &index, at, count, next_id)?;
            storage::update_table(&self.conn, &table, Some(next_id + k), Some(false))?;
            // The window's keys moved across chunk boundaries; the index is rebuilt on next use.
        }
        Ok(table)
    }

    /// No room between two neighbours: re-spread the keys of a window around `at` (growing
    /// it until the new rows fit) and give the new rows their slots in it. Only the
    /// window's order keys change.
    fn rebalance_insert(&mut self, table: &str, index: &PosIndex, at: u64, count: u64, next_id: i64) -> Result<()> {
        let t = ident(table);
        let rows = index.rows;
        let mut w = count.max(16);
        loop {
            let a = at.saturating_sub(w);
            let b = at.saturating_add(w).min(rows);
            let needed = (b - a) as i128 + count as i128;
            let first = index.ord_at(&self.conn, table, a)? as i128;
            let last = index.ord_at(&self.conn, table, b - 1)? as i128;
            let lo = if a > 0 { index.ord_at(&self.conn, table, a - 1)? as i128 } else { first - (needed + 1) * ORD_GAP as i128 };
            let hi = if b < rows { index.ord_at(&self.conn, table, b)? as i128 } else { last + (needed + 1) * ORD_GAP as i128 };
            let whole = a == 0 && b == rows;
            if hi - lo > needed || whole {
                if hi - lo <= needed || lo < i64::MIN as i128 || hi > i64::MAX as i128 {
                    bail!("cannot place {count} more rows here; the table's order keys are exhausted");
                }
                let before = (at - a) as i64;
                let slot = |s: &str| format!("({lo}::HUGEINT + ({hi}::HUGEINT - {lo}) * ({s} + 1) // {})::BIGINT", needed + 1);
                self.conn.execute_batch(&format!(
                    "UPDATE {t} SET {ORD} = {} FROM (
                        SELECT {ID} AS rid, row_number() OVER (ORDER BY {ORD}) - 1 AS rn FROM {t} WHERE {ORD} BETWEEN {first} AND {last}
                     ) w WHERE {t}.{ID} = w.rid;
                     INSERT INTO {t} ({ID}, {ORD}) SELECT {next_id} + i, {} FROM range({count}) r(i);",
                    slot(&format!("(w.rn + CASE WHEN w.rn >= {before} THEN {count} ELSE 0 END)")),
                    slot(&format!("({before} + i)")),
                ))?;
                return Ok(());
            }
            w = w.saturating_mul(2);
        }
    }

    /// Delete view rows `[start, start + count)`. Other rows keep their ids and data.
    pub fn delete_rows(&mut self, id: u64, start: u64, count: u64) -> Result<String> {
        self.mutating("delete_rows", Some(id), |wb| {
            let entry = wb.shared.view(id)?;
            let m = wb.db().materialize(&entry)?;
            let table = m.spec.table.clone();
            let end = start.saturating_add(count).min(m.row_count);
            if start >= end {
                return Ok(table);
            }
            // Read the index before deleting: built inside this transaction afterwards, it
            // would already reflect the delete.
            let index = if m.perm.is_none() { Some(wb.db().index(&table)?) } else { None };
            let cond = wb.db().range_condition(&m, start, end)?;
            let n = wb.conn.execute(&format!("DELETE FROM main.{} WHERE {cond}", ident(&table)), [])?;
            if n as u64 != end - start {
                bail!("expected to delete {} rows, found {n}; nothing was changed", end - start);
            }
            if let Some(index) = index {
                let after = index.deleted(start, end);
                storage::update_table(&wb.conn, &table, None, Some(after.is_dense()))?;
                wb.after_commit.push(AfterCommit::Index(table.clone(), after));
            } else {
                storage::update_table(&wb.conn, &table, None, Some(false))?;
            }
            Ok(table)
        })
    }

    // ---------------------------------------------------------------- sheets

    /// Import any supported file; `.xlsx` becomes IronCalc sheets, the rest a table.
    pub fn import_any(&mut self, path: &Path, table_name: Option<&str>) -> Result<Imported> {
        self.import_any_with(path, table_name, &[])
    }

    pub fn import_any_with(&mut self, path: &Path, table_name: Option<&str>, types: &[ColumnType]) -> Result<Imported> {
        if path.to_string_lossy().to_ascii_lowercase().ends_with(".xlsx") {
            let (sheets, warnings) = self.with_sheets(|s| s.import_xlsx(path))?;
            Ok(Imported::Sheets { sheets, warnings })
        } else {
            Ok(Imported::Table { table: self.import_file_with(path, table_name, types)? })
        }
    }

    /// The sheet model, for reading.
    pub fn sheets(&self) -> MutexGuard<'_, Sheets> {
        self.shared.sheets.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Run a change on the sheet model and persist it in the same transaction as the
    /// revision. If the change fails part-way, or anything later in the transaction fails,
    /// the model is reloaded from the file, so memory never runs ahead of what is durable
    /// (undo history is lost in that case). Requests rejected before any change keep the
    /// model and its history.
    pub fn with_sheets<T>(&mut self, f: impl FnOnce(&mut Sheets) -> Result<T>) -> Result<T> {
        self.mutating("sheets", None, |wb| {
            let (result, changed) = {
                let mut sheets = wb.shared.sheets.lock().unwrap_or_else(|e| e.into_inner());
                let before = sheets.changes();
                let result = f(&mut sheets);
                (result, sheets.changes() != before)
            };
            if changed {
                wb.sheet_epoch += 1;
            }
            let out = result?;
            if changed {
                wb.save_sheets()?;
            }
            Ok(out)
        })
    }

    fn load_sheets(&self) -> Result<Sheets> {
        let bytes: Option<Vec<u8>> = self.conn.query_row("SELECT data FROM _rf_sheets LIMIT 1", [], |r| r.get(0)).ok();
        Sheets::from_bytes(bytes)
    }

    /// Write the model (inside the caller's transaction).
    fn save_sheets(&self) -> Result<()> {
        let bytes = self.sheets().to_bytes();
        self.conn.execute_batch("DELETE FROM _rf_sheets;")?;
        if let Some(bytes) = bytes {
            faults::check("save_sheets")?;
            self.conn.execute("INSERT INTO _rf_sheets VALUES (?)", duckdb::params![bytes])?;
        }
        Ok(())
    }

    /// Largest number of data rows copied into a sheet with `columns` columns: at most
    /// `max_rows`, and within the cell budget of one sheet edit.
    pub fn sheet_copy_limit(columns: usize, max_rows: usize) -> usize {
        let budget = crate::sheets::MAX_BULK_CELLS / columns.max(1);
        max_rows.min(budget.saturating_sub(1)).max(1)
    }

    /// Run a query and copy its first rows (with a header row) into a new sheet. See
    /// [`Workbook::sheet_copy_limit`]; the result says whether rows were left out.
    ///
    /// Sheets use Excel semantics (IEEE doubles, 15 significant digits). Numbers that a
    /// double cannot hold exactly, such as integers beyond 2^53 or long decimals, are
    /// written as text so their digits are kept; text values always stay text.
    pub fn create_sheet_from_query(&mut self, name: &str, sql: &str, max_rows: usize) -> Result<SheetCopy> {
        let db = self.db();
        let select = db.expand_query(sql)?;
        let columns = db.describe(&select)?.iter().filter(|c| !is_internal(&c.name)).count();
        let limit = Self::sheet_copy_limit(columns, max_rows);
        let preview = db.run_bounded(&select, limit)?;
        self.write_sheet(name, preview, None, limit)
    }

    /// Copy the first rows of a table view (respecting its sort and filters) into a new sheet.
    pub fn sheet_from_view(&mut self, id: u64, name: &str, max_rows: usize) -> Result<SheetCopy> {
        let db = self.db();
        let (preview, total, limit) = db.with_view(id, |m| {
            let limit = Self::sheet_copy_limit(m.columns.len(), max_rows);
            let select = db.view_select(m, Some(limit))?;
            Ok((db.run_bounded(&select, limit)?, m.row_count, limit))
        })?;
        self.write_sheet(name, preview, Some(total), limit)
    }

    fn write_sheet(&mut self, name: &str, preview: QueryPreview, total: Option<u64>, limit: usize) -> Result<SheetCopy> {
        let mut inputs: Vec<(SheetInput, bool)> = preview
            .columns
            .iter()
            .enumerate()
            .map(|(c, col)| (SheetInput { row: 0, column: c as u32, value: col.name.clone() }, true))
            .collect();
        for (r, row) in preview.rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let numeric = preview.columns[c].kind.is_numeric();
                let (value, text) = match v {
                    Value::Null => continue,
                    Value::String(s) if numeric && fits_double(s) => (s.clone(), false),
                    Value::String(s) => (s.clone(), true),
                    Value::Bool(b) => (if *b { "TRUE" } else { "FALSE" }.to_string(), false),
                    other => (other.to_string(), false),
                };
                inputs.push((SheetInput { row: r as u32 + 1, column: c as u32, value }, text));
            }
        }
        let rows_copied = preview.rows.len() as u64;
        let truncated = preview.truncated || total.is_some_and(|t| t > rows_copied);
        let sheet = self.with_sheets(|s| {
            let sheet = s.create(name)?;
            s.set_values(sheet.index, &inputs)?;
            s.info(sheet.index)
        })?;
        Ok(SheetCopy { sheet, rows_copied, total_rows: total, truncated, row_limit: limit as u64 })
    }

    /// Turn a sheet's used range into a columnar table (first row = header), reading every
    /// row and keeping cell types (see [`Sheets::typed_columns`]).
    pub fn sheet_to_table(&mut self, index: u32, name: &str) -> Result<TableInfo> {
        let columns = self.sheets().typed_columns(index)?;
        let table = self.unique_table_name(name)?;
        let rows = columns.first().map(|c| c.values.len()).unwrap_or(0);
        let stage = format!("_rf_stage_{}", std::process::id());
        let stage_cols: Vec<String> = (0..columns.len()).map(|i| format!("c{i} VARCHAR")).collect();
        let casts: Vec<String> = columns
            .iter()
            .enumerate()
            .map(|(i, c)| format!("CAST(c{i} AS {}) AS {}", c.sql_type, ident(&c.name)))
            .collect();
        self.mutating("sheet_to_table", None, |wb| {
            wb.conn.execute_batch(&format!(
                "CREATE OR REPLACE TEMP TABLE {} ({ID} BIGINT, {});",
                ident(&stage),
                stage_cols.join(", ")
            ))?;
            {
                let mut appender = wb.conn.appender(&stage)?;
                for r in 0..rows {
                    let mut row: Vec<duckdb::types::Value> = vec![duckdb::types::Value::BigInt(r as i64)];
                    for c in &columns {
                        row.push(match &c.values[r] {
                            Some(v) => duckdb::types::Value::Text(v.clone()),
                            None => duckdb::types::Value::Null,
                        });
                    }
                    appender.append_row(duckdb::appender_params_from_iter(row))?;
                }
                appender.flush()?;
            }
            faults::check("sheet_to_table")?;
            wb.conn.execute_batch(&format!(
                "CREATE TABLE {} AS SELECT {ID}, {ID} * {ORD_GAP} AS {ORD}, {} FROM {} ORDER BY {ID}; DROP TABLE {};",
                ident(&table),
                casts.join(", "),
                ident(&stage),
                ident(&stage)
            ))?;
            storage::register_table(&wb.conn, &table, rows as i64)?;
            wb.table_info(&table)
        })
    }

    // ---------------------------------------------------------- SQL & export

    /// Run a read-only query and return the first `limit` rows.
    pub fn preview_query(&self, sql: &str, limit: usize) -> Result<QueryPreview> {
        self.db().preview_query(sql, limit)
    }

    /// Write a view (respecting its sort and filters) to a file.
    pub fn export_view(&self, id: u64, path: &Path, format: ExportFormat) -> Result<()> {
        self.db().export_view(id, path, format)
    }

    /// Materialize a view's current rows as a new table (a snapshot: later changes to the
    /// source do not reach it).
    pub fn table_from_view(&mut self, id: u64, name: &str) -> Result<TableInfo> {
        let name = self.unique_table_name(name)?;
        self.mutating("table_from_view", Some(id), |wb| {
            let entry = wb.shared.view(id)?;
            let m = wb.db().materialize(&entry)?;
            let select = wb.db().view_select(&m, None)?;
            wb.create_table_from_select(&name, &select)?;
            wb.table_info(&name)
        })
    }

    pub fn export_table(&self, table: &str, path: &Path, format: ExportFormat) -> Result<()> {
        self.db().export_table(table, path, format)
    }
}

impl Db<'_> {
    /// First `limit` rows of a SELECT for copying into a sheet (no reply-size limit: the
    /// rows go to a sheet, not over the wire, and `limit` already bounds them).
    pub fn run_bounded(&self, select: &str, limit: usize) -> Result<QueryPreview> {
        let described = self.describe(select)?;
        let keep: Vec<usize> = (0..described.len()).filter(|&i| !is_internal(&described[i].name)).collect();
        let aliases: Vec<String> = (0..described.len()).map(|i| format!("_c{i}")).collect();
        let columns: Vec<Column> = keep.iter().map(|&i| described[i].clone()).collect();
        let projection: Vec<String> = keep.iter().map(|&i| crate::wire::projection(&described[i], &aliases[i])).collect();
        let codecs: Vec<crate::wire::Codec> = columns.iter().map(|c| crate::wire::codec(&c.sql_type)).collect();
        let sql = format!(
            "SELECT {} FROM ({select}) AS _q({}) LIMIT {}",
            if projection.is_empty() { "NULL".to_string() } else { projection.join(", ") },
            aliases.join(", "),
            limit + 1
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let mut values = Vec::with_capacity(columns.len());
            for (i, codec) in codecs.iter().enumerate() {
                values.push(crate::wire::decode(*codec, row.get_ref(i)?));
            }
            out.push(values);
        }
        let truncated = out.len() > limit;
        out.truncate(limit);
        Ok(QueryPreview { columns, rows: out, truncated })
    }
}

fn open_view(db: &Db, spec: ViewSpec) -> Result<ViewInfo> {
    db.validate_spec(&spec)?;
    let entry = db.shared.register_view(spec)?;
    match db.view_info(entry.id) {
        Ok(info) => Ok(info),
        Err(e) => {
            db.shared.remove_view(entry.id);
            Err(e)
        }
    }
}

fn update_view(db: &Db, id: u64, spec: ViewSpec) -> Result<ViewInfo> {
    let entry = db.shared.view(id)?;
    db.validate_spec(&spec)?;
    *entry.spec.lock().unwrap_or_else(|e| e.into_inner()) = spec;
    db.view_info(id)
}

impl Reader {
    /// Apply DuckDB settings; settings such as `memory_limit` and `threads` belong to the
    /// database instance, so this changes them for the writer and every reader.
    pub fn configure(&self, settings: &[(&str, String)]) -> Result<()> {
        for (key, value) in settings {
            self.conn.execute_batch(&format!("SET {key} = {};", lit(value)))?;
        }
        Ok(())
    }

    pub fn setting(&self, key: &str) -> Result<String> {
        Ok(self.conn.query_row("SELECT current_setting(?)::VARCHAR", [key], |r| r.get(0))?)
    }

    pub fn shared(&self) -> &Shared {
        &self.shared
    }

    fn db(&self) -> Db<'_> {
        self.shared.sweep(&self.conn);
        Db { conn: &self.conn, shared: &self.shared, in_txn: false }
    }

    pub fn list_tables(&self) -> Result<Vec<TableInfo>> {
        self.db().list_tables()
    }

    pub fn open_view(&self, spec: ViewSpec) -> Result<ViewInfo> {
        open_view(&self.db(), spec)
    }

    pub fn update_view(&self, id: u64, spec: ViewSpec) -> Result<ViewInfo> {
        update_view(&self.db(), id, spec)
    }

    pub fn close_view(&self, id: u64) {
        self.shared.remove_view(id);
        self.shared.sweep(&self.conn);
    }

    pub fn view_info(&self, id: u64) -> Result<ViewInfo> {
        self.db().view_info(id)
    }

    pub fn fetch(&self, id: u64, row_start: u64, row_count: u64, column_start: u32, column_count: Option<u32>) -> Result<RowBlock> {
        self.db().fetch(id, row_start, row_count, column_start, column_count)
    }

    pub fn stats(&self, id: u64, column: &str, start: u64, end: u64) -> Result<Stats> {
        self.db().stats(id, column, start, end)
    }

    pub fn preview_query(&self, sql: &str, limit: usize) -> Result<QueryPreview> {
        self.db().preview_query(sql, limit)
    }

    pub fn saved_views(&self, table: Option<&str>) -> Result<Vec<SavedView>> {
        self.db().saved_views(table)
    }

    pub fn export_view(&self, id: u64, path: &Path, format: ExportFormat) -> Result<()> {
        self.db().export_view(id, path, format)
    }

    pub fn revision(&self) -> Result<u64> {
        storage::revision(&self.conn)
    }

    pub fn history(&self, after: u64, limit: usize) -> Result<Vec<HistoryEntry>> {
        storage::history(&self.conn, after, limit)
    }

    pub fn sheets(&self) -> MutexGuard<'_, Sheets> {
        self.shared.sheets.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn interrupt_handle(&self) -> Arc<duckdb::InterruptHandle> {
        self.conn.interrupt_handle()
    }

    pub fn progress_handle(&self) -> duckdb::progress::ProgressHandle {
        self.conn.progress_handle()
    }

    /// Copy the whole workbook database into a new file at `path`, from one snapshot,
    /// while the writer keeps working. Returns the revision the copy holds.
    pub fn backup_to(&self, path: &Path) -> Result<u64> {
        backup_to(&self.conn, path)
    }
}

/// `COPY FROM DATABASE` runs in one transaction, so the copy is one consistent revision.
fn backup_to(conn: &Connection, path: &Path) -> Result<u64> {
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    let alias = format!("rf_backup_{}", storage::now_ms());
    conn.execute_batch(&format!("ATTACH {} AS {};", lit(&path.to_string_lossy()), ident(&alias)))?;
    let result = (|| -> Result<u64> {
        conn.execute_batch("BEGIN TRANSACTION;")?;
        let out = (|| -> Result<u64> {
            let revision = storage::revision(conn)?;
            let db: String = conn.query_row("SELECT current_database()", [], |r| r.get(0))?;
            faults::check("backup")?;
            conn.execute_batch(&format!("COPY FROM DATABASE {} TO {};", ident(&db), ident(&alias)))?;
            Ok(revision)
        })();
        match out {
            Ok(r) => {
                conn.execute_batch("COMMIT;")?;
                Ok(r)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    })();
    let detached = conn.execute_batch(&format!("DETACH {};", ident(&alias)));
    let revision = result?;
    detached?;
    if let Ok(f) = std::fs::File::open(path) {
        f.sync_all()?;
    }
    Ok(revision)
}

impl Workbook {
    /// See [`Reader::backup_to`]; on the writer's connection (it waits for nothing, but the
    /// writer is busy while it runs).
    pub fn backup_to(&self, path: &Path) -> Result<u64> {
        if self.txn_depth > 0 {
            bail!("cannot back up inside a change");
        }
        backup_to(&self.conn, path)
    }
}

/// Can this exact decimal text become a spreadsheet number without losing digits?
/// Spreadsheets keep 15 significant digits.
fn fits_double(text: &str) -> bool {
    let digits: String = text.trim_start_matches(['+', '-']).chars().filter(|c| *c != '.').collect();
    let significant = digits.trim_start_matches('0').trim_end_matches('0');
    text.chars().all(|c| c.is_ascii_digit() || "+-.".contains(c)) && significant.len() <= 15
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
