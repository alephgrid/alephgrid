//! A workbook is one DuckDB database file. Every user table carries a hidden,
//! dense `_rf_id` column (0..n) that doubles as the row position, so reading any
//! window of an unsorted table is a zone-map-pruned range scan.
//!
//! Sorted or filtered views materialize only a permutation `(_rf_pos, _rf_id)`
//! into a temp table; rows are then fetched by id. Formula columns are plain
//! expressions in the projection, so they are computed only for rows that are
//! actually read (the visible window, an aggregate, or an export).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use duckdb::Connection;
use serde_json::Value;

use crate::faults;
use crate::formula;
use crate::import::{self, ColumnType, FilePreview, PreviewColumn};
use crate::model::*;
use crate::sheets::{SheetInfo, SheetInput, Sheets};
use crate::sql::{formula_to_sql, ident, lit, sanitize_table_name};
use crate::wire::{self, Codec};

const ID: &str = "_rf_id";
const MAX_FETCH: u64 = 5_000;

struct View {
    spec: ViewSpec,
    columns: Vec<Column>,
    row_count: u64,
    /// Temp table holding the permutation, when the view is not the identity.
    permutation: Option<String>,
}

pub struct Workbook {
    conn: Connection,
    views: HashMap<u64, View>,
    next_view: u64,
    sheets: Sheets,
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

impl Workbook {
    pub fn open(path: &Path) -> Result<Workbook> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening workbook {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Workbook> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Workbook> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS _rf_columns (
                table_name VARCHAR, name VARCHAR, formula VARCHAR, position INTEGER
             );
             CREATE TABLE IF NOT EXISTS _rf_sheets (data BLOB);
             CREATE TABLE IF NOT EXISTS _rf_views (
                id VARCHAR PRIMARY KEY, name VARCHAR, table_name VARCHAR, spec VARCHAR, widths VARCHAR, updated_at BIGINT
             );",
        )?;
        // Show TIMESTAMPTZ values in UTC so results do not depend on the server's zone.
        let _ = conn.execute_batch("SET TimeZone = 'UTC';");
        // Lets long statements (imports, sorts) report progress to the UI.
        conn.set_query_progress_tracking(true)?;
        let bytes: Option<Vec<u8>> = conn.query_row("SELECT data FROM _rf_sheets LIMIT 1", [], |r| r.get(0)).ok();
        let sheets = Sheets::from_bytes(bytes)?;
        Ok(Workbook { conn, views: HashMap::new(), next_view: 1, sheets })
    }

    /// Apply DuckDB settings such as `memory_limit` or `threads`.
    pub fn configure(&self, settings: &[(&str, String)]) -> Result<()> {
        for (key, value) in settings {
            self.conn.execute_batch(&format!("SET {key} = {};", lit(value)))?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------- tables

    pub fn list_tables(&self) -> Result<Vec<TableInfo>> {
        let names: Vec<String> = self
            .conn
            .prepare(
                "SELECT table_name FROM duckdb_tables()
                 WHERE schema_name = 'main' AND NOT temporary AND NOT starts_with(table_name, '_rf_')
                 ORDER BY table_name",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        names
            .into_iter()
            .map(|name| {
                let row_count = self.count_rows(&name)?;
                let column_count = self.columns(&name)?.len();
                Ok(TableInfo { name, row_count, column_count })
            })
            .collect()
    }

    fn table_exists(&self, name: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            &format!(
                "SELECT count(*) FROM duckdb_tables() WHERE schema_name = 'main' AND table_name = {}",
                lit(name)
            ),
            [],
            |r| r.get(0),
        )?;
        Ok(n > 0)
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

    fn count_rows(&self, table: &str) -> Result<u64> {
        let n: i64 =
            self.conn.query_row(&format!("SELECT count(*) FROM {}", ident(table)), [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// Import a CSV/TSV/Parquet/JSON(L)/XLSX file as a new table.
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
        faults::check("import")?;
        self.create_table_from_select(&name, &select).map_err(import::import_error)?;
        self.table_info(&name)
    }

    /// Detected columns and the first rows of a file, as they would be imported.
    pub fn preview_file(&self, path: &Path, types: &[ColumnType]) -> Result<FilePreview> {
        let detected = self.describe(&import::reader_select(path, &[])?).map_err(import::import_error)?;
        self.check_type_overrides(path, types)?;
        let select = import::reader_select(path, types)?;
        let rows = self.run_preview(&select, import::PREVIEW_ROWS).map_err(import::import_error)?;
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
        let detected = self.describe(&import::reader_select(path, &[])?).map_err(import::import_error)?;
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

    /// Handle for reading the progress of the statement running on this workbook.
    pub fn progress_handle(&self) -> duckdb::progress::ProgressHandle {
        self.conn.progress_handle()
    }

    /// Handle that interrupts the statement running on this workbook.
    pub fn interrupt_handle(&self) -> std::sync::Arc<duckdb::InterruptHandle> {
        self.conn.interrupt_handle()
    }

    /// Materialize any SELECT as a new table (e.g. a group-by or a join).
    pub fn create_table_from_query(&mut self, name: &str, sql: &str) -> Result<TableInfo> {
        let name = self.unique_table_name(name)?;
        self.create_table_from_select(&name, &self.expand_query(sql)?)?;
        self.table_info(&name)
    }

    fn create_table_from_select(&self, name: &str, select: &str) -> Result<()> {
        let cols = self.describe(select)?;
        let rest = if cols.iter().any(|c| c.name == ID) { format!("* EXCLUDE ({ID})") } else { "*".into() };
        // A streaming row_number() keeps the source order.
        self.conn.execute_batch(&format!(
            "CREATE TABLE {} AS SELECT (row_number() OVER () - 1)::BIGINT AS {ID}, {rest} FROM ({select});",
            ident(name)
        ))?;
        Ok(())
    }

    /// Generate a synthetic flights table, handy for trying big-data behaviour.
    pub fn create_demo_table(&mut self, rows: u64) -> Result<TableInfo> {
        let name = self.unique_table_name("flights")?;
        let airports = "['ATL','DFW','DEN','ORD','LAX','JFK','LAS','MCO','MIA','CLT','SEA','PHX','EWR','SFO','IAH','BOS','FLL','MSP','LGA','DTW']";
        self.conn.execute_batch(&format!(
            "CREATE TABLE {table} AS
             SELECT i::BIGINT AS {ID},
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
        self.table_info(&name)
    }

    pub fn drop_table(&mut self, table: &str) -> Result<()> {
        self.conn.execute("DELETE FROM _rf_views WHERE table_name = ?", duckdb::params![table])?;
        self.views.retain(|_, v| v.spec.table != table);
        self.conn.execute_batch(&format!(
            "DROP TABLE {}; DELETE FROM _rf_columns WHERE table_name = {};",
            ident(table),
            lit(table)
        ))?;
        Ok(())
    }

    pub fn rename_table(&mut self, table: &str, new_name: &str) -> Result<String> {
        let new_name = self.unique_table_name(new_name)?;
        self.conn.execute_batch(&format!(
            "ALTER TABLE {} RENAME TO {}; UPDATE _rf_columns SET table_name = {} WHERE table_name = {};",
            ident(table),
            ident(&new_name),
            lit(&new_name),
            lit(table)
        ))?;
        for v in self.views.values_mut().filter(|v| v.spec.table == table) {
            v.spec.table = new_name.clone();
        }
        self.rewrite_saved_views(table, |spec, _| {
            spec.table = new_name.clone();
            Ok(())
        })?;
        Ok(new_name)
    }

    pub fn table_info(&self, table: &str) -> Result<TableInfo> {
        Ok(TableInfo {
            name: table.to_string(),
            row_count: self.count_rows(table)?,
            column_count: self.columns(table)?.len(),
        })
    }

    // --------------------------------------------------------------- columns

    fn formulas(&self, table: &str) -> Result<Vec<(String, String)>> {
        Ok(self
            .conn
            .prepare(&format!(
                "SELECT name, formula FROM _rf_columns WHERE table_name = {} ORDER BY position",
                lit(table)
            ))?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?)
    }

    /// `SELECT _rf_id, <data columns>, <formula columns> FROM table`.
    fn base_select(&self, table: &str) -> Result<String> {
        // Qualified, so a CTE that shadows the table (see `expand_query`) can read it.
        let mut select = format!("SELECT * FROM main.{}", ident(table));
        for (name, formula) in self.formulas(table)? {
            select = format!("SELECT *, ({}) AS {} FROM ({select})", formula_to_sql(&formula), ident(&name));
        }
        Ok(select)
    }

    fn describe(&self, select: &str) -> Result<Vec<Column>> {
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
        cols.retain(|c| c.name != ID);
        for c in &mut cols {
            c.formula = formulas.get(&c.name).cloned();
        }
        Ok(cols)
    }

    pub fn add_column(&mut self, table: &str, name: &str, sql_type: &str) -> Result<()> {
        self.ensure_new_column(table, name, None)?;
        let ty = sql_type.trim();
        if ty.is_empty() || !ty.chars().all(|c| c.is_ascii_alphanumeric() || " (),".contains(c)) {
            bail!("invalid column type {sql_type:?}");
        }
        self.conn.execute_batch(&format!("ALTER TABLE {} ADD COLUMN {} {ty};", ident(table), ident(name)))?;
        self.refresh_views_for(table)
    }

    /// Add a formula column. The formula is a DuckDB expression; `[Col]` refers to a column.
    pub fn add_formula_column(&mut self, table: &str, name: &str, formula: &str) -> Result<()> {
        self.ensure_new_column(table, name, None)?;
        formula::analyze(&self.conn, formula)?;
        let probe = format!(
            "SELECT ({}) AS {} FROM ({}) LIMIT 1",
            formula_to_sql(formula),
            ident(name),
            self.base_select(table)?
        );
        self.conn.execute_batch(&probe).map_err(|e| anyhow!("formula error: {e}"))?;
        self.conn.execute(
            "INSERT INTO _rf_columns VALUES (?, ?, ?, (SELECT coalesce(max(position) + 1, 0) FROM _rf_columns WHERE table_name = ?))",
            duckdb::params![table, name, formula, table],
        )?;
        self.refresh_views_for(table)
    }

    pub fn update_formula(&mut self, table: &str, name: &str, formula: &str) -> Result<()> {
        if !self.formulas(table)?.iter().any(|(n, _)| n == name) {
            bail!("{name:?} is not a formula column");
        }
        formula::analyze(&self.conn, formula)?;
        self.transaction(|wb| {
            wb.conn.execute(
                "UPDATE _rf_columns SET formula = ? WHERE table_name = ? AND name = ?",
                duckdb::params![formula, table, name],
            )?;
            wb.describe(&wb.base_select(table)?).map_err(|e| anyhow!("formula error: {e}"))?;
            Ok(())
        })?;
        self.refresh_views_for(table)
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
        self.transaction(|wb| {
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
            wb.describe(&wb.base_select(table)?)?;
            Ok(())
        })?;
        self.refresh_views_for(table)
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
        self.transaction(|wb| {
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
            wb.describe(&wb.base_select(table)?).map_err(|e| anyhow!("rename would break a formula: {e}"))?;
            Ok(())
        })?;
        for v in self.views.values_mut().filter(|v| v.spec.table == table) {
            for k in &mut v.spec.sort {
                if k.column == name {
                    k.column = new_name.to_string();
                }
            }
            for f in &mut v.spec.filters {
                if f.column == name {
                    f.column = new_name.to_string();
                }
            }
            for h in &mut v.spec.hidden {
                if h == name {
                    *h = new_name.to_string();
                }
            }
        }
        self.refresh_views_for(table)
    }

    fn ensure_new_column(&self, table: &str, name: &str, renaming: Option<&str>) -> Result<()> {
        if name.trim().is_empty() || name.eq_ignore_ascii_case(ID) {
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

    /// Run `f` in a database transaction; roll back on any error.
    fn transaction<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.conn.execute_batch("BEGIN TRANSACTION;")?;
        match f(self) {
            Ok(v) => {
                self.conn.execute_batch("COMMIT;")?;
                Ok(v)
            }
            Err(e) => {
                self.conn.execute_batch("ROLLBACK;")?;
                Err(e)
            }
        }
    }

    // ----------------------------------------------------------- saved views

    /// Saved views, of one table or of all tables, by name.
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

    /// Create a saved view, or update the one with `id`. The spec must open (columns exist)
    /// and names are unique per table.
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
        // Validate by building it once.
        let probe = self.open_view(spec.clone())?;
        self.close_view(probe.view_id);
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
        self.conn.execute(
            "INSERT OR REPLACE INTO _rf_views VALUES (?, ?, ?, ?, ?, ?)",
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
    }

    pub fn rename_saved_view(&mut self, id: &str, name: &str) -> Result<SavedView> {
        let v = self.saved_views(None)?.into_iter().find(|v| v.id == id).ok_or_else(|| anyhow!("unknown saved view {id}"))?;
        self.save_view(Some(id), name, v.spec, v.widths)
    }

    pub fn delete_saved_view(&mut self, id: &str) -> Result<()> {
        let n = self.conn.execute("DELETE FROM _rf_views WHERE id = ?", duckdb::params![id])?;
        if n == 0 {
            bail!("unknown saved view {id}");
        }
        Ok(())
    }

    /// Apply `f` to every saved view of `table` (inside the caller's transaction, if any).
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
        let id = self.next_view;
        self.next_view += 1;
        let view = self.build_view(id, spec)?;
        self.views.insert(id, view);
        self.view_info(id)
    }

    pub fn update_view(&mut self, id: u64, spec: ViewSpec) -> Result<ViewInfo> {
        if !self.views.contains_key(&id) {
            bail!("unknown view {id}");
        }
        let view = self.build_view(id, spec)?;
        self.views.insert(id, view);
        self.view_info(id)
    }

    pub fn close_view(&mut self, id: u64) {
        if let Some(View { permutation: Some(t), .. }) = self.views.remove(&id) {
            let _ = self.conn.execute_batch(&format!("DROP TABLE IF EXISTS {};", ident(&t)));
        }
    }

    pub fn view_info(&self, id: u64) -> Result<ViewInfo> {
        let v = self.view(id)?;
        Ok(ViewInfo {
            view_id: id,
            spec: v.spec.clone(),
            row_count: v.row_count,
            columns: v.columns.clone(),
            can_append: v.permutation.is_none(),
        })
    }

    /// Views over `table` whose ids changed (row inserts/deletes) or whose schema changed.
    pub fn views_on(&self, table: &str) -> Vec<u64> {
        self.views.iter().filter(|(_, v)| v.spec.table == table).map(|(id, _)| *id).collect()
    }

    fn view(&self, id: u64) -> Result<&View> {
        self.views.get(&id).ok_or_else(|| anyhow!("unknown view {id}"))
    }

    fn build_view(&mut self, id: u64, spec: ViewSpec) -> Result<View> {
        let all = self.columns(&spec.table)?;
        for h in &spec.hidden {
            if !all.iter().any(|c| &c.name == h) {
                bail!("unknown column {h:?}");
            }
        }
        if all.iter().all(|c| spec.hidden.contains(&c.name)) && !all.is_empty() {
            bail!("a view must show at least one column");
        }
        let columns: Vec<Column> = all.iter().filter(|c| !spec.hidden.contains(&c.name)).cloned().collect();
        let by_name: HashMap<&str, &Column> = all.iter().map(|c| (c.name.as_str(), c)).collect();
        let total = self.count_rows(&spec.table)?;
        let dense = {
            let max: Option<i64> = self.conn.query_row(
                &format!("SELECT max({ID}) FROM {}", ident(&spec.table)),
                [],
                |r| r.get(0),
            )?;
            max.map(|m| m + 1).unwrap_or(0) as u64 == total
        };
        if dense && spec.is_identity() {
            if let Some(View { permutation: Some(t), .. }) = self.views.get(&id) {
                self.conn.execute_batch(&format!("DROP TABLE IF EXISTS {};", ident(t)))?;
            }
            return Ok(View { spec, columns, row_count: total, permutation: None });
        }

        let mut conditions = Vec::new();
        for f in &spec.filters {
            let col = by_name.get(f.column.as_str()).ok_or_else(|| anyhow!("unknown column {:?}", f.column))?;
            conditions.push(filter_sql(col, f)?);
        }
        let mut order = Vec::new();
        for k in &spec.sort {
            if !by_name.contains_key(k.column.as_str()) {
                bail!("unknown column {:?}", k.column);
            }
            order.push(format!("{} {} NULLS LAST", ident(&k.column), if k.desc { "DESC" } else { "ASC" }));
        }
        order.push(ID.to_string());
        let where_sql = if conditions.is_empty() { String::new() } else { format!("WHERE {}", conditions.join(" AND ")) };
        let perm = format!("_rf_view_{id}");
        self.conn.execute_batch(&format!(
            "CREATE OR REPLACE TEMP TABLE {} AS
             SELECT (row_number() OVER () - 1)::BIGINT AS _rf_pos, {ID}
             FROM (SELECT {ID} FROM ({}) {where_sql} ORDER BY {});",
            ident(&perm),
            self.base_select(&spec.table)?,
            order.join(", ")
        ))?;
        let row_count = self.count_rows(&perm)?;
        Ok(View { spec, columns, row_count, permutation: Some(perm) })
    }

    /// Rebuild every view on `table` (after its rows were renumbered or its schema changed).
    /// References to columns that no longer exist are dropped from open views; clients
    /// learn the new spec from `view_info` after the `table_changed` event.
    fn refresh_views_for(&mut self, table: &str) -> Result<()> {
        let names: Vec<String> = self.columns(table)?.into_iter().map(|c| c.name).collect();
        for id in self.views_on(table) {
            let mut spec = self.view(id)?.spec.clone();
            spec.hidden.retain(|h| names.contains(h));
            spec.sort.retain(|k| names.contains(&k.column));
            spec.filters.retain(|f| names.contains(&f.column));
            let v = self.build_view(id, spec)?;
            self.views.insert(id, v);
        }
        Ok(())
    }

    /// Ids for view positions `[start, end)`, in view order.
    fn ids_for_range(&self, v: &View, start: u64, end: u64) -> Result<Vec<i64>> {
        match &v.permutation {
            None => Ok((start as i64..end.min(v.row_count) as i64).collect()),
            Some(perm) => Ok(self
                .conn
                .prepare(&format!(
                    "SELECT {ID} FROM {} WHERE _rf_pos >= {start} AND _rf_pos < {end} ORDER BY _rf_pos",
                    ident(perm)
                ))?
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?),
        }
    }

    /// SQL fragment restricting `_rf_id` to view positions `[start, end)`.
    fn range_condition(&self, v: &View, start: u64, end: u64) -> String {
        match &v.permutation {
            None => format!("{ID} >= {start} AND {ID} < {end}"),
            Some(perm) => format!(
                "{ID} IN (SELECT {ID} FROM {} WHERE _rf_pos >= {start} AND _rf_pos < {end})",
                ident(perm)
            ),
        }
    }

    pub fn fetch(&self, id: u64, row_start: u64, row_count: u64) -> Result<RowBlock> {
        let v = self.view(id)?;
        let end = (row_start + row_count.min(MAX_FETCH)).min(v.row_count);
        if row_start >= end {
            return Ok(RowBlock { row_start, rows: vec![] });
        }
        let ids = self.ids_for_range(v, row_start, end)?;
        let projection: Vec<String> = v.columns.iter().map(|c| wire::projection(c, &ident(&c.name))).collect();
        let codecs: Vec<Codec> = v.columns.iter().map(|c| wire::codec(&c.sql_type)).collect();
        let condition = match &v.permutation {
            None => format!("{ID} >= {row_start} AND {ID} < {end}"),
            Some(_) => format!(
                "{ID} IN ({})",
                ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")
            ),
        };
        let sql = format!(
            "SELECT {ID}, {} FROM ({}) WHERE {condition}",
            projection.join(", "),
            self.base_select(&v.spec.table)?
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        let mut by_id: HashMap<i64, Vec<Value>> = HashMap::with_capacity(ids.len());
        while let Some(row) = rows.next()? {
            let rid: i64 = row.get(0)?;
            let mut values = Vec::with_capacity(v.columns.len());
            for (i, codec) in codecs.iter().enumerate() {
                values.push(wire::decode(*codec, row.get_ref(i + 1)?));
            }
            by_id.insert(rid, values);
        }
        let rows = ids
            .iter()
            .map(|rid| by_id.remove(rid).unwrap_or_else(|| vec![Value::Null; v.columns.len()]))
            .collect();
        Ok(RowBlock { row_start, rows })
    }

    // ----------------------------------------------------------------- edits

    /// Apply cell edits as one transaction. Typing below the last row appends rows (in the
    /// same transaction). Every value is validated before anything changes; repeated
    /// cells in one batch keep the last value; an empty batch changes nothing.
    pub fn edit(&mut self, id: u64, edits: &[CellEdit]) -> Result<EditOutcome> {
        let v = self.view(id)?;
        let table = v.spec.table.clone();
        if edits.is_empty() {
            return Ok(EditOutcome { table, cells_changed: 0, rows_appended: 0 });
        }
        let by_name: HashMap<String, Column> = v.columns.iter().map(|c| (c.name.clone(), c.clone())).collect();
        for e in edits {
            let col = by_name.get(&e.column).ok_or_else(|| anyhow!("unknown column {:?}", e.column))?;
            if col.is_computed() {
                bail!("{:?} is a formula column; edit its formula instead", col.name);
            }
        }
        // Last write wins for repeated cells; keep first-seen order otherwise.
        let mut cells: Vec<(u64, &str, Option<&str>)> = Vec::new();
        let mut seen: HashMap<(u64, &str), usize> = HashMap::new();
        for e in edits {
            let value = e.value.as_deref().filter(|s| !s.is_empty());
            match seen.get(&(e.row, e.column.as_str())) {
                Some(&i) => cells[i].2 = value,
                None => {
                    seen.insert((e.row, e.column.as_str()), cells.len());
                    cells.push((e.row, e.column.as_str(), value));
                }
            }
        }
        let max_row = cells.iter().map(|c| c.0).max().unwrap_or(0);
        let appended = if max_row >= v.row_count {
            if v.permutation.is_some() {
                bail!("clear sorting and filters to add rows");
            }
            max_row + 1 - v.row_count
        } else {
            0
        };

        // Validate every value before touching the table.
        let mut by_column: HashMap<&str, Vec<(u64, Option<&str>)>> = HashMap::new();
        for (row, column, value) in &cells {
            by_column.entry(column).or_default().push((*row, *value));
        }
        for (column, values) in &by_column {
            let col = &by_name[*column];
            for value in values.iter().filter_map(|(_, v)| *v) {
                wire::check_exact_input(col, value).map_err(|e| anyhow!(e))?;
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

        // View positions -> record ids.
        let mut pos_to_id: HashMap<u64, i64> = HashMap::new();
        match &v.permutation {
            None => {
                for (row, _, _) in &cells {
                    pos_to_id.insert(*row, *row as i64);
                }
            }
            Some(perm) => {
                let positions: Vec<String> = cells.iter().map(|c| c.0.to_string()).collect();
                let pairs: Vec<(i64, i64)> = self
                    .conn
                    .prepare(&format!(
                        "SELECT _rf_pos, {ID} FROM {} WHERE _rf_pos IN ({})",
                        ident(perm),
                        positions.join(",")
                    ))?
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<_, _>>()?;
                for (p, rid) in pairs {
                    pos_to_id.insert(p as u64, rid);
                }
            }
        }
        let row_count = v.row_count;
        let mut updates: Vec<(String, String)> = Vec::new();
        for (column, values) in &by_column {
            let col = &by_name[*column];
            let mut rows = Vec::with_capacity(values.len());
            for (row, val) in values {
                let rid = *pos_to_id.get(row).ok_or_else(|| anyhow!("row {row} is outside the view"))?;
                rows.push(format!("({rid}, {})", val.map(lit).unwrap_or_else(|| "NULL::VARCHAR".into())));
            }
            updates.push((col.name.clone(), format!(
                "UPDATE {t} SET {c} = CAST(v.val AS {ty}) FROM (VALUES {}) v(id, val) WHERE {t}.{ID} = v.id;",
                rows.join(","),
                t = ident(&table),
                c = ident(&col.name),
                ty = col.sql_type,
            )));
        }
        let cells_changed = cells.len();
        self.transaction(|wb| {
            if appended > 0 {
                wb.conn.execute_batch(&format!(
                    "INSERT INTO {} ({ID}) SELECT i FROM range({row_count}, {}) t(i);",
                    ident(&table),
                    max_row + 1
                ))?;
            }
            for (_, sql) in &updates {
                faults::check("edit")?;
                wb.conn.execute_batch(sql)?;
            }
            Ok(())
        })?;
        if appended > 0 {
            self.refresh_views_for(&table)?;
        }
        Ok(EditOutcome { table, cells_changed, rows_appended: appended })
    }

    /// Insert empty rows before view position `at`. Only on unsorted, unfiltered views.
    pub fn insert_rows(&mut self, id: u64, at: u64, count: u64) -> Result<String> {
        let v = self.view(id)?;
        if v.permutation.is_some() {
            bail!("clear sorting and filters to insert rows");
        }
        let table = v.spec.table.clone();
        let t = ident(&table);
        self.conn.execute_batch(&format!(
            "BEGIN TRANSACTION;
             UPDATE {t} SET {ID} = {ID} + {count} WHERE {ID} >= {at};
             INSERT INTO {t} ({ID}) SELECT i FROM range({at}, {}) t(i);
             COMMIT;",
            at + count
        ))?;
        self.refresh_views_for(&table)?;
        Ok(table)
    }

    /// Delete view rows `[start, start + count)` and renumber so ids stay dense.
    pub fn delete_rows(&mut self, id: u64, start: u64, count: u64) -> Result<String> {
        let v = self.view(id)?;
        let table = v.spec.table.clone();
        let t = ident(&table);
        if v.permutation.is_none() {
            // Contiguous ids: shift the tail down instead of renumbering everything.
            let count = count.min(v.row_count.saturating_sub(start));
            self.conn.execute_batch(&format!(
                "BEGIN TRANSACTION;
                 DELETE FROM {t} WHERE {ID} >= {start} AND {ID} < {end};
                 UPDATE {t} SET {ID} = {ID} - {count} WHERE {ID} >= {end};
                 COMMIT;",
                end = start + count
            ))?;
            self.refresh_views_for(&table)?;
            return Ok(table);
        }
        let condition = self.range_condition(v, start, start + count);
        self.conn.execute_batch(&format!(
            "BEGIN TRANSACTION;
             DELETE FROM {t} WHERE {condition};
             CREATE OR REPLACE TEMP TABLE _rf_renumber AS
               SELECT {ID} AS old_id, (row_number() OVER (ORDER BY {ID}) - 1)::BIGINT AS new_id FROM {t};
             UPDATE {t} SET {ID} = r.new_id FROM _rf_renumber r WHERE {t}.{ID} = r.old_id AND r.old_id <> r.new_id;
             DROP TABLE _rf_renumber;
             COMMIT;"
        ))?;
        self.refresh_views_for(&table)?;
        Ok(table)
    }

    // ----------------------------------------------------------------- stats

    /// Count/sum/avg/min/max of a column over view rows `[start, end)`.
    pub fn stats(&self, id: u64, column: &str, start: u64, end: u64) -> Result<Stats> {
        let v = self.view(id)?;
        let col = v.columns.iter().find(|c| c.name == column).ok_or_else(|| anyhow!("unknown column {column:?}"))?;
        let end = end.min(v.row_count);
        if start >= end {
            return Ok(Stats::default());
        }
        let condition = if start == 0 && end == v.row_count && v.permutation.is_none() {
            "TRUE".to_string()
        } else if start == 0 && end == v.row_count {
            format!("{ID} IN (SELECT {ID} FROM {})", ident(v.permutation.as_ref().unwrap()))
        } else {
            self.range_condition(v, start, end)
        };
        let c = ident(column);
        let base = self.base_select(&v.spec.table)?;
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

    pub fn sheets(&self) -> &Sheets {
        &self.sheets
    }

    /// Run a mutation on the sheets model and persist it in the same DuckDB file. If the
    /// mutation fails part-way or the save fails, the in-memory model is reloaded from the
    /// file, so memory never runs ahead of what is durable (undo history is lost in that
    /// case). Requests rejected before any change keep the model and its history.
    pub fn with_sheets<T>(&mut self, f: impl FnOnce(&mut Sheets) -> Result<T>) -> Result<T> {
        let before = self.sheets.changes();
        let result = f(&mut self.sheets);
        let changed = self.sheets.changes() != before;
        let result = match result {
            // Rejected before anything changed: memory still matches disk, keep undo history.
            Err(e) if !changed => return Err(e),
            Err(e) => Err(e),
            Ok(out) if !changed => return Ok(out),
            Ok(out) => self.save_sheets().map(|_| out),
        };
        if result.is_err() {
            match self.load_sheets() {
                Ok(sheets) => self.sheets = sheets,
                Err(e) => tracing_like_warn(&format!("could not reload sheets after a failed change: {e:#}")),
            }
        }
        result
    }

    fn load_sheets(&self) -> Result<Sheets> {
        let bytes: Option<Vec<u8>> = self.conn.query_row("SELECT data FROM _rf_sheets LIMIT 1", [], |r| r.get(0)).ok();
        Sheets::from_bytes(bytes)
    }

    fn save_sheets(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN TRANSACTION; DELETE FROM _rf_sheets;")?;
        let result = match self.sheets.to_bytes() {
            Some(bytes) => faults::check("save_sheets").and_then(|_| {
                self.conn.execute("INSERT INTO _rf_sheets VALUES (?)", duckdb::params![bytes]).map(|_| ()).map_err(Into::into)
            }),
            None => Ok(()),
        };
        match result {
            Ok(()) => self.conn.execute_batch("COMMIT;")?,
            Err(e) => {
                self.conn.execute_batch("ROLLBACK;")?;
                return Err(e);
            }
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
        let select = self.expand_query(sql)?;
        let columns = self.describe(&select)?.iter().filter(|c| c.name != ID).count();
        let limit = Self::sheet_copy_limit(columns, max_rows);
        let preview = self.run_preview(&select, limit)?;
        self.write_sheet(name, preview, None, limit)
    }

    /// Copy the first rows of a table view (respecting its sort and filters) into a new sheet.
    pub fn sheet_from_view(&mut self, id: u64, name: &str, max_rows: usize) -> Result<SheetCopy> {
        let v = self.view(id)?;
        let total = v.row_count;
        let limit = Self::sheet_copy_limit(v.columns.len(), max_rows);
        let select = self.view_select(id, Some(limit))?;
        let preview = self.run_preview(&select, limit)?;
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
        let columns = self.sheets.typed_columns(index)?;
        let table = self.unique_table_name(name)?;
        let rows = columns.first().map(|c| c.values.len()).unwrap_or(0);
        let stage = format!("_rf_stage_{}", std::process::id());
        let stage_cols: Vec<String> = (0..columns.len()).map(|i| format!("c{i} VARCHAR")).collect();
        let casts: Vec<String> = columns
            .iter()
            .enumerate()
            .map(|(i, c)| format!("CAST(c{i} AS {}) AS {}", c.sql_type, ident(&c.name)))
            .collect();
        self.transaction(|wb| {
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
                "CREATE TABLE {} AS SELECT {ID}, {} FROM {} ORDER BY {ID}; DROP TABLE {};",
                ident(&table),
                casts.join(", "),
                ident(&stage),
                ident(&stage)
            ))?;
            Ok(())
        })?;
        self.table_info(&table)
    }

    // ---------------------------------------------------------- SQL & export

    /// Expose formula columns to ad-hoc SQL by shadowing each table with a CTE. When the
    /// query has its own `WITH` list, the shadows are prepended to it, and a user CTE with
    /// the same name as a table takes precedence.
    fn expand_query(&self, sql: &str) -> Result<String> {
        let sql = sql.trim().trim_end_matches(';').trim_end();
        let mut ctes = Vec::new();
        for t in self.list_tables()? {
            if !self.formulas(&t.name)?.is_empty() {
                ctes.push((t.name.clone(), format!("{} AS ({})", ident(&t.name), self.base_select(&t.name)?)));
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
                    .filter(|(name, _)| !user.iter().any(|u| formula::same_name(u, name)))
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

    /// Run a read-only query and return the first `limit` rows.
    pub fn preview_query(&self, sql: &str, limit: usize) -> Result<QueryPreview> {
        let select = self.expand_query(sql)?;
        self.run_preview(&select, limit)
    }

    /// First `limit` rows of a SELECT, in wire form, with truncation reported.
    fn run_preview(&self, select: &str, limit: usize) -> Result<QueryPreview> {
        let described = self.describe(select)?;
        // Address result columns by position, so duplicate or odd names still work.
        let aliases: Vec<String> = (0..described.len()).map(|i| format!("_c{i}")).collect();
        let keep: Vec<usize> = (0..described.len()).filter(|&i| described[i].name != ID).collect();
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
        while let Some(row) = rows.next()? {
            let mut values = Vec::with_capacity(columns.len());
            for (i, codec) in codecs.iter().enumerate() {
                values.push(wire::decode(*codec, row.get_ref(i)?));
            }
            out.push(values);
        }
        let truncated = out.len() > limit;
        out.truncate(limit);
        Ok(QueryPreview { columns, rows: out, truncated })
    }

    /// Write a view (respecting its sort and filters) to a file.
    pub fn export_view(&self, id: u64, path: &Path, format: ExportFormat) -> Result<()> {
        let select = self.view_select(id, None)?;
        self.copy_to(&select, path, format)
    }

    /// The view's visible columns, in view order, optionally only the first `limit` rows.
    fn view_select(&self, id: u64, limit: Option<usize>) -> Result<String> {
        let v = self.view(id)?;
        let base = self.base_select(&v.spec.table)?;
        let cols: Vec<String> = v.columns.iter().map(|c| format!("b.{}", ident(&c.name))).collect();
        let cols = cols.join(", ");
        Ok(match &v.permutation {
            None => {
                let limit = limit.map(|l| format!("WHERE b.{ID} < {l}")).unwrap_or_default();
                format!("SELECT {cols} FROM ({base}) b {limit} ORDER BY b.{ID}")
            }
            Some(perm) => {
                let limit = limit.map(|l| format!("WHERE p._rf_pos < {l}")).unwrap_or_default();
                format!("SELECT {cols} FROM ({base}) b JOIN {} p USING ({ID}) {limit} ORDER BY p._rf_pos", ident(perm))
            }
        })
    }

    /// Materialize a view's current rows as a new table (a snapshot: later changes to the
    /// source do not reach it).
    pub fn table_from_view(&mut self, id: u64, name: &str) -> Result<TableInfo> {
        let select = self.view_select(id, None)?;
        let name = self.unique_table_name(name)?;
        self.create_table_from_select(&name, &select)?;
        self.table_info(&name)
    }

    pub fn export_table(&self, table: &str, path: &Path, format: ExportFormat) -> Result<()> {
        let select = format!("SELECT * EXCLUDE ({ID}) FROM ({}) ORDER BY {ID}", self.base_select(table)?);
        self.copy_to(&select, path, format)
    }

    fn copy_to(&self, select: &str, path: &Path, format: ExportFormat) -> Result<()> {
        let options = match format {
            ExportFormat::Csv => "FORMAT csv, HEADER",
            ExportFormat::Parquet => "FORMAT parquet, COMPRESSION zstd",
            ExportFormat::Json => "FORMAT json",
        };
        self.conn
            .execute_batch(&format!("COPY ({select}) TO {} ({options});", lit(&path.to_string_lossy())))?;
        Ok(())
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.conn.execute_batch("CHECKPOINT;")?;
        Ok(())
    }
}

fn filter_sql(col: &Column, f: &Filter) -> Result<String> {
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

/// Can this exact decimal text become a spreadsheet number without losing digits?
/// Spreadsheets keep 15 significant digits.
fn fits_double(text: &str) -> bool {
    let digits: String = text.trim_start_matches(['+', '-']).chars().filter(|c| *c != '.').collect();
    let significant = digits.trim_start_matches('0').trim_end_matches('0');
    text.chars().all(|c| c.is_ascii_digit() || "+-.".contains(c)) && significant.len() <= 15
}

fn tracing_like_warn(msg: &str) {
    eprintln!("rowfinity-engine: {msg}");
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
