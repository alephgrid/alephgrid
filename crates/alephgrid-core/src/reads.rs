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
use crate::sqlpolicy::{self, CheckedSelect, NotAllowed, QueryParam};
use crate::storage::{self, ID, ORD};
use crate::wire::{self, Codec};

/// Internal columns never shown to users.
pub(crate) fn is_internal(name: &str) -> bool {
    name == ID || name == ORD || name.starts_with(ERROR_PREFIX)
}

/// A column of a table, both names lowercased, as a dependency-graph node.
type Node = (String, String);

/// A table's columns in relation order, and its formulas as (name, formula, is_sheet).
type Layout = (Vec<String>, Vec<(String, String, bool)>);

/// Column-level dependencies of every formula column in the workbook, so lookups across
/// tables can tell real circular references from tables that merely look each other up.
pub(crate) struct Graph {
    deps: HashMap<Node, Vec<Node>>,
}

impl Graph {
    /// Nodes reachable from `from` (not including `from` unless a path leads back to it).
    fn closure(&self, from: &[Node]) -> std::collections::HashSet<Node> {
        let mut seen = std::collections::HashSet::new();
        let mut stack: Vec<Node> = from.iter().flat_map(|n| self.deps.get(n).cloned().unwrap_or_default()).collect();
        while let Some(n) = stack.pop() {
            if seen.insert(n.clone()) {
                stack.extend(self.deps.get(&n).cloned().unwrap_or_default());
            }
        }
        seen
    }

    /// A dependency path from `deps` (a new formula's references) to `target`, if any.
    fn path(&self, deps: &[Node], target: &Node) -> Option<Vec<Node>> {
        let mut parent: HashMap<Node, Option<Node>> = HashMap::new();
        let mut queue: std::collections::VecDeque<Node> = std::collections::VecDeque::new();
        for d in deps {
            if parent.insert(d.clone(), None).is_none() { queue.push_back(d.clone()); }
        }
        while let Some(n) = queue.pop_front() {
            if &n == target {
                let mut path = vec![n.clone()];
                let mut at = n;
                while let Some(Some(p)) = parent.get(&at) { path.push(p.clone()); at = p.clone(); }
                path.reverse();
                return Some(path);
            }
            for d in self.deps.get(&n).cloned().unwrap_or_default() {
                if !parent.contains_key(&d) { parent.insert(d.clone(), Some(n.clone())); queue.push_back(d); }
            }
        }
        None
    }
}

/// State shared while one table's relation is built, including nested lookup relations.
pub(crate) struct Build {
    /// Formula columns being compiled, outermost first. A nested relation leaves out any
    /// column that is one of them or depends on one of them: reading it would be circular.
    in_progress: std::cell::RefCell<Vec<Node>>,
    graph: std::cell::OnceCell<Graph>,
    /// Tables read by each relation being built, innermost last.
    reads: std::cell::RefCell<Vec<std::collections::BTreeSet<String>>>,
}

impl Build {
    pub(crate) fn new() -> Self {
        Build { in_progress: std::cell::RefCell::new(Vec::new()), graph: std::cell::OnceCell::new(), reads: std::cell::RefCell::new(Vec::new()) }
    }
}

/// Other tables for lookups: their full relation (with their own formulas, except any that
/// would be circular here), or for the formula's own table its stored columns only.
struct TableRelations<'a, 'b> {
    db: &'a Db<'b>,
    own: &'a str,
    own_select: String,
    build: &'a Build,
    /// Checking a formula about to be saved: sheet cells no formula read before come from
    /// the live sheet model (saving the formula links them). Reads use saved values only.
    live: bool,
}

impl crate::table_formula::Relations for TableRelations<'_, '_> {
    fn relation(&self, table: &str) -> std::result::Result<Option<crate::table_formula::Relation>, String> {
        let found = storage::user_tables(self.db.conn).map_err(|e| e.to_string())?.into_iter().find(|t| t.eq_ignore_ascii_case(table));
        let Some(actual) = found else { return Ok(None) };
        let (sql, reads) = if actual.eq_ignore_ascii_case(self.own) {
            (self.own_select.clone(), vec![actual.to_lowercase()])
        } else {
            self.db.base_select_tracked(&actual, self.build).map_err(|e| e.to_string())?
        };
        let described = self.db.describe(&sql).map_err(|e| e.to_string())?;
        let errors = described.iter().filter_map(|c| c.name.strip_prefix(ERROR_PREFIX).map(|n| (n.to_string(), c.name.clone()))).collect();
        let columns = described.into_iter().filter(|c| !is_internal(&c.name)).map(|c| (c.name, c.sql_type)).collect();
        Ok(Some(crate::table_formula::Relation { sql, columns, errors, reads }))
    }

    fn cell(&self, cell: &crate::table_formula::CellRef) -> std::result::Result<Option<crate::table_formula::CellValue>, String> {
        if let Some(v) = storage::link_value(self.db.conn, cell).map_err(|e| e.to_string())? {
            return Ok(Some(v));
        }
        if !self.live {
            return Ok(None);
        }
        Ok(self.db.shared.sheets.lock().unwrap_or_else(|e| e.into_inner()).cell_value(cell))
    }
}

/// The hidden column holding the error codes of spreadsheet-formula column `name`.
pub(crate) const ERROR_PREFIX: &str = "_rf_e_";

pub(crate) fn error_column(name: &str) -> String {
    format!("{ERROR_PREFIX}{name}")
}

/// Alephgrid's own tables (`_rf_meta`, `_rf_history`, …), in any letter case.
pub(crate) fn is_internal_table(name: &str) -> bool {
    name.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("_rf_"))
}

pub(crate) struct Db<'a> {
    pub conn: &'a Connection,
    pub shared: &'a Shared,
    /// Inside the writer's transaction: no snapshots of our own, no caching of what is
    /// read (it may be rolled back), no scratch writes (a transaction writes one database).
    pub in_txn: bool,
    /// Cached lookup tables this operation's SQL refers to, kept until it is done.
    pub keep: std::cell::RefCell<Vec<Arc<crate::catalog::Scratch>>>,
    /// Lookup tables built inside this operation's snapshot. Other connections cannot see
    /// them until it commits, so they join the shared cache only then (and are dropped if it
    /// rolls back).
    pub built: std::cell::RefCell<Vec<PendingLookup>>,
    /// Inside [`Db::snapshot`].
    pub in_snapshot: std::cell::Cell<bool>,
}

/// A lookup table built in a snapshot that has not committed yet.
pub(crate) struct PendingLookup {
    sql: String,
    revisions: Vec<(String, u64)>,
    table: Arc<crate::catalog::Scratch>,
    rows: u64,
}

impl Db<'_> {
    /// Run `f` in one snapshot (a read transaction), or in the caller's transaction.
    pub fn snapshot<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        if self.in_txn {
            return f();
        }
        self.conn.execute_batch("BEGIN TRANSACTION;")?;
        self.in_snapshot.set(true);
        let out = f().and_then(|v| {
            crate::faults::check("snapshot_commit")?;
            // An interrupt aimed at the read can land on its COMMIT instead.
            self.conn.execute_batch("COMMIT;")?;
            Ok(v)
        });
        self.in_snapshot.set(false);
        let built = std::mem::take(&mut *self.built.borrow_mut());
        if out.is_err() {
            // Whatever failed, never leave this connection inside a transaction. Lookup
            // tables built meanwhile went with it.
            let _ = self.conn.execute_batch("ROLLBACK;");
        } else {
            let mut cache = self.shared.lookups.lock().unwrap_or_else(|e| e.into_inner());
            for p in built {
                cache.put(&p.sql, p.revisions, Some(p.table), p.rows);
            }
        }
        out
    }

    // ------------------------------------------------------------- schema

    /// Whether `name` is a user table. Internal tables (`_rf_*`) are never user tables, so
    /// no table operation can reach Alephgrid's own metadata by name.
    pub fn table_exists(&self, name: &str) -> Result<bool> {
        if is_internal_table(name) {
            return Ok(false);
        }
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

    /// Calculated columns (SQL language) whose formula is one call of a worker function.
    pub(crate) fn worker_columns(&self, cache: &crate::external::ResultCache) -> Result<Vec<crate::external::ColumnCall>> {
        let mut stmt = self.conn.prepare("SELECT table_name, name, formula FROM _rf_columns WHERE language = 'sql' ORDER BY table_name, position")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (table, column, formula) = row?;
            if let Some((function, args)) = crate::external::parse_column_call(&formula, |n| cache.is_worker_function(n)) {
                out.push(crate::external::ColumnCall { table, column, function, args });
            }
        }
        Ok(out)
    }

    pub fn formulas(&self, table: &str) -> Result<Vec<(String, String)>> {
        Ok(self
            .conn
            .prepare(&format!("SELECT name, formula FROM _rf_columns WHERE table_name = {} ORDER BY position", lit(table)))?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?)
    }

    /// Formula columns written in spreadsheet syntax (`language = 'sheet'`).
    pub fn sheet_formulas(&self, table: &str) -> Result<std::collections::HashSet<String>> {
        Ok(self
            .conn
            .prepare(&format!("SELECT name FROM _rf_columns WHERE table_name = {} AND language = 'sheet'", lit(table)))?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?)
    }

    /// `SELECT _rf_id, _rf_ord, <data columns>, <formula columns> FROM table`. A spreadsheet
    /// formula column adds its value and a hidden `_rf_e_<name>` error column.
    pub fn base_select(&self, table: &str) -> Result<String> {
        self.base_select_in(table, &Build::new())
    }

    /// The columns of `table` in relation order (stored, then formula columns), and its
    /// formulas as (name, formula, is_sheet).
    pub(crate) fn layout(&self, table: &str) -> Result<Layout> {
        let sheet = self.sheet_formulas(table)?;
        let stored: Vec<String> = self.describe(&format!("SELECT * FROM main.{}", ident(table)))?.into_iter().map(|c| c.name).filter(|n| !is_internal(n)).collect();
        let formulas: Vec<(String, String, bool)> = self.formulas(table)?.into_iter().map(|(n, f)| { let s = sheet.contains(&n); (n, f, s) }).collect();
        let mut order = stored;
        order.extend(formulas.iter().map(|(n, _, _)| n.clone()));
        Ok((order, formulas))
    }

    /// Dependencies of every formula column in the workbook.
    pub(crate) fn graph(&self) -> Result<Graph> {
        let mut layouts = HashMap::new();
        for t in storage::user_tables(self.conn)? {
            layouts.insert(t.to_lowercase(), self.layout(&t)?);
        }
        let mut deps = HashMap::new();
        for (t, (order, formulas)) in &layouts {
            for (k, (name, f, sheet)) in formulas.iter().enumerate() {
                let out = if *sheet {
                    // A formula that no longer parses reads nothing (it shows #REF!).
                    self.sheet_deps(t, f, &layouts).unwrap_or_default()
                } else {
                    // A SQL formula reads only its own table; count every column before it.
                    order[..order.len() - formulas.len() + k].iter().map(|c| (t.clone(), c.to_lowercase())).collect()
                };
                deps.insert((t.clone(), name.to_lowercase()), out);
            }
        }
        Ok(Graph { deps })
    }

    /// For each table (lowercase), the other tables whose formulas read it directly. Only
    /// spreadsheet formulas read other tables; one query, no table is described.
    pub(crate) fn table_readers(&self) -> Result<HashMap<String, Vec<String>>> {
        use crate::table_formula::Reference;
        let rows: Vec<(String, String)> = self
            .conn
            .prepare("SELECT table_name, formula FROM _rf_columns WHERE language = 'sheet'")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        for (reader, formula) in rows {
            let reader = reader.to_lowercase();
            let Ok(refs) = crate::table_formula::references(&formula) else { continue };
            for r in refs {
                let (Reference::Column { table, .. } | Reference::Table { table, .. }) = r else { continue };
                let read = table.to_lowercase();
                if read != reader {
                    let list = out.entry(read).or_default();
                    if !list.contains(&reader) {
                        list.push(reader.clone());
                    }
                }
            }
        }
        Ok(out)
    }

    /// The columns a spreadsheet formula of `table` reads, as graph nodes.
    fn sheet_deps(&self, table: &str, formula: &str, layouts: &HashMap<String, Layout>) -> Result<Vec<Node>> {
        use crate::table_formula::Reference;
        let own = table.to_lowercase();
        let mut out = Vec::new();
        for r in crate::table_formula::references(formula).map_err(|e| anyhow!("{e}"))? {
            match r {
                Reference::Own { column, .. } => out.push((own.clone(), column.to_lowercase())),
                Reference::Column { table, column, .. } => out.push((table.to_lowercase(), column.to_lowercase())),
                Reference::Table { table, positions, .. } => {
                    let t = table.to_lowercase();
                    if let Some((order, _)) = layouts.get(&t) {
                        out.extend(positions.iter().filter_map(|p| order.get(p - 1)).map(|c| (t.clone(), c.to_lowercase())));
                    }
                }
                // Sheet cells read nothing from tables (sheets cannot read tables yet).
                Reference::Cell { .. } => {}
            }
        }
        Ok(out)
    }

    /// [`Self::base_select_in`], with the tables the relation reads (lowercase).
    pub(crate) fn base_select_tracked(&self, table: &str, build: &Build) -> Result<(String, Vec<String>)> {
        build.reads.borrow_mut().push(std::collections::BTreeSet::from([table.to_lowercase()]));
        let out = self.base_select_in(table, build);
        let mine = build.reads.borrow_mut().pop().unwrap_or_default();
        if let Some(parent) = build.reads.borrow_mut().last_mut() {
            parent.extend(mine.iter().cloned());
        }
        Ok((out?, mine.into_iter().collect()))
    }

    /// Read grouped lookup relations (joins and ASOF joins) from the cache when it is fresh.
    fn use_cached_stages(&self, stages: &mut [crate::table_formula::Stage]) -> Result<()> {
        use crate::table_formula::Stage;
        for stage in stages {
            match stage {
                Stage::Join { sql, reads, .. } => {
                    if let Some(cached) = self.cached_lookup(sql, reads)? {
                        *sql = format!("SELECT k, v, e FROM {cached}");
                    }
                }
                Stage::Asof { sql, reads, .. } => {
                    if let Some(cached) = self.cached_lookup(sql, reads)? {
                        *sql = format!("SELECT r, n, t, v, e FROM {cached}");
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Whether scratch table `s` exists in this connection's snapshot.
    fn scratch_visible(&self, s: &crate::catalog::Scratch) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            &format!("SELECT count(*) FROM duckdb_tables() WHERE database_name = {} AND table_name = {}", lit(crate::catalog::SCRATCH), lit(&s.name)),
            [],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// The scratch table holding grouped lookup relation `sql` as of the current data of
    /// `reads`, built now if needed. `None` inside the writer's transaction (no scratch
    /// writes there), or when the result is larger than [`limits::LOOKUP_CACHE_ROWS`].
    fn cached_lookup(&self, sql: &str, reads: &[String]) -> Result<Option<String>> {
        if self.in_txn || reads.is_empty() {
            return Ok(None);
        }
        let all = storage::data_revisions(self.conn)?;
        let revisions = |all: &HashMap<String, u64>| -> Option<Vec<(String, u64)>> {
            reads.iter().map(|r| all.iter().find(|(t, _)| t.eq_ignore_ascii_case(r)).map(|(_, v)| (r.clone(), *v))).collect()
        };
        let Some(revs) = revisions(&all) else { return Ok(None) };
        // Built earlier in this snapshot.
        if let Some(p) = self.built.borrow().iter().find(|p| p.sql == sql && p.revisions == revs) {
            return Ok(Some(p.table.qualified()));
        }
        let mut cache = self.shared.lookups.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = cache.get(sql, &revs) {
            match hit {
                None => return Ok(None),
                // A table committed after this snapshot began is not visible in it.
                Some(s) if self.scratch_visible(&s)? => {
                    let name = s.qualified();
                    self.keep.borrow_mut().push(s);
                    return Ok(Some(name));
                }
                Some(_) => {}
            }
        }
        drop(cache);
        let scratch = self.shared.new_scratch("lk");
        self.conn.execute_batch(&format!("CREATE TABLE {} AS {sql};", scratch.qualified()))?;
        let rows: i64 = self.conn.query_row(&format!("SELECT count(*) FROM {}", scratch.qualified()), [], |r| r.get(0))?;
        // Built outside a snapshot, the data may have moved on meanwhile: do not keep it.
        if revisions(&storage::data_revisions(self.conn)?).as_ref() != Some(&revs) {
            return Ok(None);
        }
        let rows = rows.max(0) as u64;
        let mut cache = self.shared.lookups.lock().unwrap_or_else(|e| e.into_inner());
        if rows > limits::LOOKUP_CACHE_ROWS {
            cache.put(sql, revs, None, rows);
            return Ok(None);
        }
        let scratch = Arc::new(scratch);
        let name = scratch.qualified();
        self.keep.borrow_mut().push(scratch.clone());
        if self.in_snapshot.get() {
            self.built.borrow_mut().push(PendingLookup { sql: sql.to_string(), revisions: revs, table: scratch, rows });
        } else {
            cache.put(sql, revs, Some(scratch), rows);
        }
        Ok(Some(name))
    }

    /// Build `table`'s relation within `build`: formula columns that would be circular
    /// here (see [`Build`]) show #REF!, keeping their place so column numbers stay valid.
    fn base_select_in(&self, table: &str, build: &Build) -> Result<String> {
        // Qualified, so a CTE that shadows the table (see `expand_query`) can read it.
        let mut select = format!("SELECT * FROM main.{}", ident(table));
        let sheet = self.sheet_formulas(table)?;
        let own = table.to_lowercase();
        for (i, (name, formula)) in self.formulas(table)?.into_iter().enumerate() {
            let node = (own.clone(), name.to_lowercase());
            let circular = {
                let in_progress = build.in_progress.borrow();
                !in_progress.is_empty() && (in_progress.contains(&node) || {
                    let graph = match build.graph.get() { Some(g) => g, None => { let g = self.graph()?; build.graph.get_or_init(|| g) } };
                    graph.closure(std::slice::from_ref(&node)).iter().any(|n| in_progress.contains(n))
                })
            };
            if circular {
                let value = if sheet.contains(&name) { "CAST(NULL AS VARCHAR)" } else { "NULL" };
                select = format!("SELECT *, {value} AS {}, '#REF!' AS {} FROM ({select})", ident(&name), ident(&error_column(&name)));
                if !sheet.contains(&name) {
                    // SQL formula columns have no error column; drop the placeholder's.
                    select = format!("SELECT * EXCLUDE ({}) FROM ({select})", ident(&error_column(&name)));
                }
                continue;
            }
            if !sheet.contains(&name) {
                select = format!("SELECT *, ({}) AS {} FROM ({select})", formula_to_sql(&formula), ident(&name));
                continue;
            }
            let described = self.describe(&select)?;
            let errors: Vec<(String, String)> = described
                .iter()
                .filter_map(|c| c.name.strip_prefix(ERROR_PREFIX).map(|n| (n.to_string(), c.name.clone())))
                .collect();
            let columns: Vec<(String, String)> = described.into_iter().filter(|c| !is_internal(&c.name)).map(|c| (c.name, c.sql_type)).collect();
            let relations = TableRelations { db: self, own: table, own_select: format!("SELECT * FROM main.{}", ident(table)), build, live: false };
            build.in_progress.borrow_mut().push(node);
            let compiled = crate::table_formula::compile_full(&formula, &columns, &errors, &relations, &format!("_rf_t{i}_"));
            build.in_progress.borrow_mut().pop();
            match compiled {
                Ok(mut c) => {
                    // Grouped lookup relations come from the cache when it is fresh.
                    self.use_cached_stages(&mut c.stages)?;
                    let temps = c.temps().iter().map(|a| ident(a)).collect::<Vec<_>>().join(", ");
                    let exclude = if temps.is_empty() { String::new() } else { format!(" EXCLUDE ({temps})") };
                    select = format!(
                        "SELECT *{exclude}, {} AS {}, {} AS {} FROM ({})",
                        c.value, ident(&name), c.error, ident(&error_column(&name)), c.wrap(&select)
                    );
                }
                // A formula that no longer compiles (a column or table it reads was renamed or
                // deleted outside Alephgrid) shows #REF! instead of making the table unreadable.
                Err(_) => {
                    select = format!("SELECT *, CAST(NULL AS VARCHAR) AS {}, '#REF!' AS {} FROM ({select})", ident(&name), ident(&error_column(&name)));
                }
            }
        }
        Ok(select)
    }

    /// Compile spreadsheet formula `formula` for column `column` of `table` as the table is
    /// now (a formula cannot read its own column), refusing circular references.
    pub fn check_sheet_formula(&self, table: &str, formula: &str, column: &str) -> Result<()> {
        let select = self.base_select(table)?;
        let described = self.describe(&select)?;
        let errors: Vec<(String, String)> = described.iter().filter_map(|c| c.name.strip_prefix(ERROR_PREFIX).map(|n| (n.to_string(), c.name.clone()))).collect();
        let columns: Vec<(String, String)> = described.into_iter().filter(|c| !is_internal(&c.name) && c.name != column).map(|c| (c.name, c.sql_type)).collect();
        let target = (table.to_lowercase(), column.to_lowercase());
        let build = Build::new();
        build.in_progress.borrow_mut().push(target.clone());
        let relations = TableRelations { db: self, own: table, own_select: format!("SELECT * FROM main.{}", ident(table)), build: &build, live: true };
        let c = crate::table_formula::compile_full(formula, &columns, &errors, &relations, "_rf_check_").map_err(|e| anyhow!("formula error: {e}"))?;
        // Circular references across tables, with the path in the message.
        let graph = match build.graph.into_inner() { Some(g) => g, None => self.graph()? };
        let mut layouts = HashMap::new();
        for t in storage::user_tables(self.conn)? {
            layouts.insert(t.to_lowercase(), self.layout(&t)?);
        }
        let deps = self.sheet_deps(table, formula, &layouts).map_err(|e| anyhow!("formula error: {e}"))?;
        if let Some(path) = graph.path(&deps, &target) {
            let names: Vec<String> = std::iter::once(&target).chain(path.iter()).map(|(t, c)| format!("{t}.{c}")).collect();
            bail!("formula error: circular reference: {}", names.join(" → "));
        }
        // Plan it, so a type mistake in generated SQL fails here rather than on read.
        self.describe(&format!("SELECT {}, {} FROM ({})", c.value, c.error, c.wrap(&select)))?;
        Ok(())
    }

    /// The value of the table formula of a sheet cell that reads tables (a table cell):
    /// the formula compiled over a relation of one row with no columns, so it can read
    /// whole table columns and sheet cells but not `[@column]`. `Ok(Err(..))` for a formula
    /// that does not compile, with where; `live` as in [`TableRelations`].
    pub fn table_cell_value(&self, formula: &str, live: bool) -> Result<std::result::Result<crate::table_formula::CellValue, crate::table_formula::FormulaError>> {
        use crate::table_formula::{CellValue, FormulaError, Reference, Ty};
        if let Ok(refs) = crate::table_formula::references(formula) {
            if let Some(Reference::Own { column, span }) = refs.into_iter().find(|r| matches!(r, Reference::Own { .. })) {
                return Ok(Err(FormulaError {
                    message: format!("[@{column}] reads a row of a table, and a sheet cell has no row; read whole columns instead, e.g. SUMIFS(orders[amount], orders[{column}], \"x\")"),
                    span,
                }));
            }
        }
        let own = "SELECT 0::BIGINT AS _rf_id, 0::BIGINT AS _rf_ord";
        let build = Build::new();
        let relations = TableRelations { db: self, own: "", own_select: own.into(), build: &build, live };
        // A table result that spills (F.10): FILTER, SORT, UNIQUE or TAKE over a table.
        match crate::table_formula::array_plan(formula, &relations) {
            Err(e) => return Ok(Err(e)),
            Ok(Some(plan)) => return self.table_cell_array(&plan, live, formula),
            Ok(None) => {}
        }
        let mut c = match crate::table_formula::compile_full(formula, &[], &[], &relations, "_rf_c_") {
            Ok(c) => c,
            Err(e) => return Ok(Err(e)),
        };
        if c.tables.is_empty() {
            return Ok(Err(FormulaError { message: "this formula reads no table; the sheet can compute it".into(), span: crate::table_formula::Span { start: 0, end: formula.chars().count() } }));
        }
        self.use_cached_stages(&mut c.stages)?;
        let value = match c.ty {
            // Dates reach the sheet as serial numbers, as the sheet engine stores them.
            Ty::Date => format!("CAST(date_diff('day', DATE '1899-12-30', {}) AS DOUBLE)", c.value),
            Ty::Bool => format!("CASE WHEN {} THEN 1.0 ELSE 0.0 END", c.value),
            Ty::Text => format!("CAST({} AS VARCHAR)", c.value),
            Ty::Number | Ty::Blank => format!("CAST({} AS DOUBLE)", c.value),
        };
        let sql = format!("SELECT {value}, {} FROM ({})", c.error, c.wrap(own));
        let (num, text, error): (Option<f64>, Option<String>, Option<String>) = self.conn.query_row(
            &format!("SELECT CASE WHEN typeof(v) = 'VARCHAR' THEN NULL ELSE TRY_CAST(v AS DOUBLE) END, CASE WHEN typeof(v) = 'VARCHAR' THEN CAST(v AS VARCHAR) END, e FROM ({sql}) t(v, e)"),
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(Ok(match (error, c.ty) {
            (Some(e), _) => CellValue::Error(e),
            (None, Ty::Text) => text.map(CellValue::Text).unwrap_or(CellValue::Blank),
            (None, Ty::Bool) => num.map(|n| CellValue::Bool(n != 0.0)).unwrap_or(CellValue::Blank),
            (None, _) => num.map(CellValue::Number).unwrap_or(CellValue::Blank),
        }))
    }

    /// Whether a sheet cell's table formula gives a table result that spills.
    pub(crate) fn table_cell_spills(&self, formula: &str) -> bool {
        let build = Build::new();
        let relations = TableRelations { db: self, own: "", own_select: "SELECT 0::BIGINT AS _rf_id, 0::BIGINT AS _rf_ord".into(), build: &build, live: true };
        matches!(crate::table_formula::array_plan(formula, &relations), Ok(Some(_)))
    }

    /// The rows of a table result, bounded by [`crate::sheets::MAX_SPILL_CELLS`]: a larger
    /// one is refused when it is entered (`live`), and shows `#SPILL!` when it grows later.
    fn table_cell_array(&self, plan: &crate::table_formula::ArrayPlan, live: bool, formula: &str) -> Result<std::result::Result<crate::table_formula::CellValue, crate::table_formula::FormulaError>> {
        use crate::table_formula::{CellValue, Ty};
        let width = plan.types.len().max(1);
        let cap = crate::sheets::MAX_SPILL_CELLS / width;
        let cols: Vec<String> = plan.types.iter().enumerate().map(|(k, t)| match t {
            Ty::Date => format!("CAST(date_diff('day', DATE '1899-12-30', c{k}) AS DOUBLE) AS c{k}"),
            Ty::Text => format!("CAST(c{k} AS VARCHAR) AS c{k}"),
            Ty::Bool => format!("c{k}"),
            _ => format!("CAST(c{k} AS DOUBLE) AS c{k}"),
        }).collect();
        let sql = format!("SELECT {} FROM ({}) LIMIT {}", cols.join(", "), plan.sql, cap + 1);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        let mut out: Vec<Vec<CellValue>> = Vec::new();
        while let Some(r) = rows.next()? {
            let mut row = Vec::with_capacity(width);
            for (k, t) in plan.types.iter().enumerate() {
                row.push(match t {
                    Ty::Text => r.get::<_, Option<String>>(k)?.map(CellValue::Text).unwrap_or(CellValue::Blank),
                    Ty::Bool => r.get::<_, Option<bool>>(k)?.map(CellValue::Bool).unwrap_or(CellValue::Blank),
                    _ => r.get::<_, Option<f64>>(k)?.map(CellValue::Number).unwrap_or(CellValue::Blank),
                });
            }
            out.push(row);
        }
        if out.len() > cap {
            if live {
                return Ok(Err(crate::table_formula::FormulaError {
                    message: format!("this table result has more than {} cells, more than a sheet takes from a table; sort and filter the table's view instead, and create a table from that view to keep a copy", crate::sheets::MAX_SPILL_CELLS),
                    span: crate::table_formula::Span { start: 0, end: formula.chars().count() },
                }));
            }
            return Ok(Ok(CellValue::Array(vec![vec![CellValue::Error("#SPILL!".into())]])));
        }
        if out.is_empty() {
            let empty = match plan.if_empty.as_ref().map(|e| &e.kind) {
                Some(crate::table_formula::ExprKind::Number(n)) => CellValue::Number(*n),
                Some(crate::table_formula::ExprKind::Text(t)) => CellValue::Text(t.clone()),
                Some(crate::table_formula::ExprKind::Bool(b)) => CellValue::Bool(*b),
                Some(crate::table_formula::ExprKind::Error(e)) => CellValue::Error(e.code().to_string()),
                _ => CellValue::Error("#CALC!".into()),
            };
            return Ok(Ok(CellValue::Array(vec![vec![empty]])));
        }
        Ok(Ok(CellValue::Array(out)))
    }

    pub fn describe(&self, select: &str) -> Result<Vec<Column>> {
        let cols: Vec<(String, String)> = self
            .conn
            .prepare(&format!("DESCRIBE {select}"))?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(cols
            .into_iter()
            .map(|(name, sql_type)| Column { kind: Kind::from_sql_type(&sql_type), name, sql_type, formula: None, formula_language: None })
            .collect())
    }

    pub fn columns(&self, table: &str) -> Result<Vec<Column>> {
        if is_internal_table(table) {
            bail!(NotAllowed(format!("{table} is internal to Alephgrid")));
        }
        let formulas: HashMap<String, String> = self.formulas(table)?.into_iter().collect();
        let sheet = self.sheet_formulas(table)?;
        let mut cols = self.describe(&self.base_select(table)?)?;
        cols.retain(|c| !is_internal(&c.name));
        for c in &mut cols {
            c.formula = formulas.get(&c.name).cloned();
            if sheet.contains(&c.name) {
                c.formula_language = Some(FormulaLanguage::Sheet);
            }
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
        // A view built on stored results is rebuilt once newer results are installed.
        let current = |m: &Materialized| match &m.results {
            None => true,
            Some(r) => self.shared.committed_results(&spec.table).is_none_or(|c| c.id == r.id),
        };
        if let Some(m) = slot.as_ref().filter(|m| m.gen == gen && m.spec == spec && current(m)) {
            return Ok(m.clone());
        }
        let all = self.validate_spec(&spec)?;
        let columns: Vec<Column> = all.iter().filter(|c| !spec.hidden.contains(&c.name)).cloned().collect();
        let built = if spec.is_identity() {
            let row_count = self.count_rows(&spec.table)?;
            Materialized { gen, spec: spec.clone(), columns, row_count, perm: None, results: None }
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
            // What the order and filters read: stored columns only need the table itself;
            // formula columns of a large table come from its stored results (see `results`).
            let used: Vec<&str> = spec.sort.iter().map(|k| k.column.as_str()).chain(spec.filters.iter().map(|f| f.column.as_str())).collect();
            let formulas: Vec<String> = self.formulas(&spec.table)?.into_iter().map(|(n, _)| n).collect();
            let stored_only = used.iter().all(|c| !formulas.iter().any(|f| f == c));
            let results = if stored_only { None } else { self.results_for_view(&spec.table)? };
            let perm = self.shared.new_scratch(&format!("v{}", entry.id));
            // Build from one snapshot and stamp it with that snapshot's generation.
            let (row_count, gen) = self.snapshot(|| {
                let gen = storage::table_meta(self.conn, &spec.table)?.gen;
                let source = match &results {
                    _ if stored_only => format!("SELECT * FROM main.{}", ident(&spec.table)),
                    // Fresh results holding every column used: the table itself is not read.
                    Some((r, None)) if Self::only_results(r, &used) => format!("SELECT * FROM {}", r.scratch.qualified()),
                    Some((r, _)) => self.results_select(&spec.table, r),
                    None => self.base_select(&spec.table)?,
                };
                self.conn.execute_batch(&format!(
                    "CREATE TABLE {} AS
                     SELECT (row_number() OVER () - 1)::BIGINT AS _rf_pos, {ID}
                     FROM (SELECT {ID} FROM ({source}) {where_sql} ORDER BY {});",
                    perm.qualified(),
                    order.join(", ")
                ))?;
                let n: i64 = self.conn.query_row(&format!("SELECT count(*) FROM {}", perm.qualified()), [], |r| r.get(0))?;
                Ok((n as u64, gen))
            })?;
            Materialized { gen, spec: spec.clone(), columns, row_count, perm: Some(Arc::new(perm)), results: results.map(|(r, _)| r) }
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

    /// The relation a view's pages read: the stored results its order was built from
    /// (`allow_stale`: even when stale), fresh stored results, or the live relation.
    pub(crate) fn view_base(&self, m: &Materialized, allow_stale: bool) -> Result<String> {
        let table = &m.spec.table;
        if let Some(r) = &m.results {
            if allow_stale || self.results_fresh(r)? {
                return Ok(self.results_select(table, r));
            }
        }
        if let Some(r) = self.shared.committed_results(table) {
            if self.results_fresh(&r)? {
                return Ok(self.results_select(table, &r));
            }
        }
        self.base_select(table)
    }

    /// Why the view's formula values are stale, if they are.
    pub(crate) fn view_staleness(&self, m: &Materialized) -> Result<Option<String>> {
        match &m.results {
            Some(r) => self.staleness(&m.spec.table, r),
            None => Ok(None),
        }
    }

    pub fn view_info(&self, id: u64) -> Result<ViewInfo> {
        self.with_view(id, |m| {
            Ok(ViewInfo {
                view_id: id,
                spec: m.spec.clone(),
                row_count: m.row_count,
                columns: m.columns.clone(),
                can_append: m.perm.is_none(),
                stale: self.view_staleness(m)?,
                generation: m.gen,
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
            let stale = self.view_staleness(m)?;
            let empty = RowBlock { row_start, rows: vec![], ids: vec![], column_start: c0, truncated: false, errors: vec![], stale: stale.clone(), generation: m.gen };
            if row_start >= end {
                return Ok(empty);
            }
            let mut projection: Vec<String> = cols.iter().map(|c| wire::projection(c, &ident(&c.name))).collect();
            let codecs: Vec<Codec> = cols.iter().map(|c| wire::codec(&c.sql_type)).collect();
            // Error codes of visible spreadsheet-formula columns, read in the same scan.
            let sheet = self.sheet_formulas(&m.spec.table)?;
            let error_cols: Vec<u32> = cols.iter().enumerate().filter(|(_, c)| sheet.contains(&c.name)).map(|(i, _)| i as u32).collect();
            projection.extend(error_cols.iter().map(|&i| ident(&error_column(&cols[i as usize].name))));
            let projection = if projection.is_empty() { String::new() } else { format!(", {}", projection.join(", ")) };
            let base = self.view_base(m, true)?;
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
            let mut errors_by_id: HashMap<i64, Vec<(u32, String)>> = HashMap::new();
            let mut in_order: Vec<i64> = Vec::new();
            while let Some(row) = rows.next()? {
                let rid: i64 = row.get(0)?;
                let mut values = Vec::with_capacity(cols.len());
                for (i, codec) in codecs.iter().enumerate() {
                    values.push(wire::decode(*codec, row.get_ref(i + 1)?));
                }
                for (k, &col) in error_cols.iter().enumerate() {
                    if let Some(code) = row.get::<_, Option<String>>(cols.len() + 1 + k)? {
                        errors_by_id.entry(rid).or_default().push((col, code));
                    }
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
            let mut errors = Vec::new();
            let mut bytes = 0usize;
            for rid in &ids {
                let values = by_id.remove(rid).ok_or_else(|| anyhow!("record {rid} vanished during the read"))?;
                let size: usize = values.iter().map(limits::json_size).sum::<usize>() + 2 * values.len() + 16;
                if !out.is_empty() && bytes + size > limits::MAX_REPLY_BYTES {
                    break;
                }
                bytes += size;
                for (column, code) in errors_by_id.remove(rid).unwrap_or_default() {
                    errors.push(CellErrorMark { row: out.len() as u32, column, code });
                }
                out.push(values);
                out_ids.push(*rid);
            }
            let truncated = out.len() < ids.len();
            Ok(RowBlock { row_start, rows: out, ids: out_ids, column_start: c0, truncated, errors, stale, generation: m.gen })
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
            let base = self.view_base(m, true)?;
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
    /// The view's rows for export and copying, with exact values: stale stored results are
    /// never written out.
    pub fn view_select(&self, m: &Materialized, limit: Option<usize>) -> Result<String> {
        let base = self.view_base(m, false)?;
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

    /// Check user SQL against the policy (see `sqlpolicy.rs`) for this workbook's tables.
    pub fn check(&self, sql: &str) -> Result<CheckedSelect> {
        sqlpolicy::check_select(self.conn, &storage::user_tables(self.conn)?, sql)
    }

    /// Expose formula columns to a checked query by shadowing each table it reads with a
    /// CTE. When the query has its own `WITH` list, the shadows are prepended to it, and a
    /// user CTE with the same name as a table takes precedence. The user's text is kept
    /// verbatim on lines of its own.
    pub fn expand_query(&self, q: &CheckedSelect) -> Result<Expanded> {
        let sql = q.sql.as_str();
        let mut ctes = Vec::new();
        // Tables that formula columns read (lookups) are sources too: the result depends on them.
        let mut sources = q.sources.clone();
        let tables = storage::user_tables(self.conn)?;
        for name in &q.sources {
            if !self.formulas(name)?.is_empty() {
                let (select, reads) = self.base_select_tracked(name, &Build::new())?;
                for r in reads {
                    if let Some(t) = tables.iter().find(|t| t.to_lowercase() == r) {
                        if !sources.contains(t) {
                            sources.push(t.clone());
                        }
                    }
                }
                ctes.push((name.clone(), format!("{} AS ({select})", ident(name))));
            }
        }
        sources.sort();
        let plain = Expanded { sql: sql.to_string(), lines_before: 0, sources };
        if ctes.is_empty() {
            return Ok(plain);
        }
        match crate::sql::leading_with(sql) {
            None => {
                let prefix = format!("WITH {}\n", ctes.into_iter().map(|(_, c)| c).collect::<Vec<_>>().join(", "));
                Ok(Expanded { lines_before: prefix.matches('\n').count(), sql: format!("{prefix}{sql}"), ..plain })
            }
            Some(after_with) => {
                let user = self.cte_names(sql)?;
                let ours: Vec<String> = ctes
                    .into_iter()
                    .filter(|(name, _)| !user.iter().any(|u| crate::formula::same_name(u, name)))
                    .map(|(_, c)| c)
                    .collect();
                if ours.is_empty() {
                    return Ok(plain);
                }
                let inserted = format!(" {},\n", ours.join(", "));
                Ok(Expanded {
                    lines_before: inserted.matches('\n').count(),
                    sql: format!("{}{inserted}{}", &sql[..after_with], &sql[after_with..]),
                    ..plain
                })
            }
        }
    }

    /// Names of the top-level CTEs of a query, from DuckDB's parser (empty if it cannot parse).
    fn cte_names(&self, sql: &str) -> Result<Vec<String>> {
        let json: String = self.conn.query_row("SELECT json_serialize_sql(?::VARCHAR)", [sql], |r| r.get(0))?;
        let tree: Value = serde_json::from_str(&json).unwrap_or(Value::Null);
        Ok(tree["statements"][0]["node"]["cte_map"]["map"]
            .as_array()
            .map(|m| m.iter().filter_map(|e| e["key"].as_str().map(String::from)).collect())
            .unwrap_or_default())
    }

    /// Columns of an expanded query, with its parameters bound.
    pub fn describe_query(&self, q: &Expanded, params: &[QueryParam]) -> Result<Vec<Column>> {
        let sql = format!("DESCRIBE SELECT * FROM (\n{}\n)", q.sql);
        let mut stmt = self.conn.prepare(&sql).map_err(|e| user_error(e.into(), q.lines_before + 1))?;
        sqlpolicy::bind(&mut stmt, params)?;
        stmt.raw_execute().map_err(|e| user_error(e.into(), q.lines_before + 1))?;
        let mut rows = stmt.raw_query();
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let (name, sql_type): (String, String) = (r.get(0)?, r.get(1)?);
            out.push(Column { kind: Kind::from_sql_type(&sql_type), name, sql_type, formula: None, formula_language: None });
        }
        Ok(out)
    }

    pub fn preview_query(&self, sql: &str, limit: usize) -> Result<QueryPreview> {
        let r = self.run_query(sql, &[], limit)?;
        Ok(QueryPreview { columns: r.columns, rows: r.rows, truncated: r.truncated })
    }

    /// Run a user query in one snapshot: at most `limit` rows (see [`Db::run_preview`]), with
    /// the time it took, the revision it read and the tables it read.
    pub fn run_query(&self, sql: &str, params: &[QueryParam], limit: usize) -> Result<QueryResult> {
        let checked = self.check(sql)?;
        sqlpolicy::validate_params(&checked.params, params)?;
        let started = std::time::Instant::now();
        self.snapshot(|| {
            let q = self.expand_query(&checked)?;
            let revision = storage::revision(self.conn)?;
            let data = storage::data_revisions(self.conn)?;
            let p = self.run_bounded_query(&q, params, limit, true)?;
            Ok(QueryResult {
                columns: p.columns,
                rows: p.rows,
                truncated: p.truncated,
                row_limit: limit.min(limits::MAX_QUERY_ROWS) as u64,
                elapsed_ms: started.elapsed().as_millis() as u64,
                revision,
                sources: q.sources.iter().map(|t| SourceRevision { table: t.clone(), data_rev: data.get(t).copied().unwrap_or(0) }).collect(),
            })
        })
    }

    /// First rows of an expanded user query with its parameters bound. `wire_bound` limits
    /// the rows to [`limits::MAX_QUERY_ROWS`] and the reply to [`limits::MAX_REPLY_BYTES`];
    /// without it only `limit` bounds them (copies into sheets).
    pub fn run_bounded_query(&self, q: &Expanded, params: &[QueryParam], limit: usize, wire_bound: bool) -> Result<QueryPreview> {
        let described = self.describe_query(q, params)?;
        let limit = if wire_bound { limit.min(limits::MAX_QUERY_ROWS) } else { limit };
        let select = format!("SELECT * FROM (\n{}\n)", q.sql);
        self.read_rows(&select, &described, params, limit, wire_bound, q.lines_before + 1)
    }

    /// First `limit` rows of an application-built SELECT (no parameters), in wire form,
    /// stopping at [`limits::MAX_QUERY_ROWS`] and [`limits::MAX_REPLY_BYTES`].
    pub fn run_preview(&self, select: &str, limit: usize) -> Result<QueryPreview> {
        let described = self.describe(select)?;
        self.read_rows(select, &described, &[], limit.min(limits::MAX_QUERY_ROWS), true, 0)
    }

    /// First `limit` rows of an application-built SELECT for copying into a sheet (no
    /// reply-size limit: the rows go to a sheet, not over the wire, and `limit` bounds them).
    pub fn run_select(&self, select: &str, limit: usize) -> Result<QueryPreview> {
        let described = self.describe(select)?;
        self.read_rows(select, &described, &[], limit, false, 0)
    }

    fn read_rows(&self, select: &str, described: &[Column], params: &[QueryParam], limit: usize, wire_bound: bool, lines_before: usize) -> Result<QueryPreview> {
        // Address result columns by position, so duplicate or odd names still work.
        let aliases: Vec<String> = (0..described.len()).map(|i| format!("_c{i}")).collect();
        let keep: Vec<usize> = (0..described.len()).filter(|&i| !is_internal(&described[i].name)).collect();
        let columns: Vec<Column> = keep.iter().map(|&i| described[i].clone()).collect();
        let projection: Vec<String> = keep.iter().map(|&i| wire::projection(&described[i], &aliases[i])).collect();
        let codecs: Vec<Codec> = columns.iter().map(|c| wire::codec(&c.sql_type)).collect();
        let sql = format!(
            "SELECT {} FROM (\n{select}\n) AS _q({}) LIMIT {}",
            if projection.is_empty() { "NULL".to_string() } else { projection.join(", ") },
            aliases.join(", "),
            limit + 1
        );
        let mut stmt = self.conn.prepare(&sql).map_err(|e| user_error(e.into(), lines_before + 1))?;
        sqlpolicy::bind(&mut stmt, params)?;
        stmt.raw_execute().map_err(|e| user_error(e.into(), lines_before + 1))?;
        let mut rows = stmt.raw_query();
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
            if wire_bound {
                bytes += values.iter().map(limits::json_size).sum::<usize>() + 2 * values.len() + 2;
                if !out.is_empty() && bytes > limits::MAX_REPLY_BYTES {
                    truncated = true;
                    break;
                }
            }
            out.push(values);
        }
        Ok(QueryPreview { columns, rows: out, truncated })
    }

    /// Write the complete result of a user query to `path`, from one snapshot, streamed by
    /// DuckDB. Returns the rows written and the revision read.
    pub fn export_query(&self, sql: &str, params: &[QueryParam], path: &Path, format: ExportFormat) -> Result<(u64, u64)> {
        let checked = self.check(sql)?;
        sqlpolicy::validate_params(&checked.params, params)?;
        self.snapshot(|| {
            let q = self.expand_query(&checked)?;
            let described = self.describe_query(&q, params)?;
            let keep: Vec<String> = described
                .iter()
                .enumerate()
                .filter(|(_, c)| !is_internal(&c.name))
                .map(|(i, c)| format!("_c{i} AS {}", ident(&c.name)))
                .collect();
            let aliases: Vec<String> = (0..described.len()).map(|i| format!("_c{i}")).collect();
            let select = format!("SELECT {} FROM (\n{}\n) AS _q({})", keep.join(", "), q.sql, aliases.join(", "));
            let revision = storage::revision(self.conn)?;
            let options = copy_options(format);
            let copy = format!("COPY ({select}) TO {} ({options})", lit(&path.to_string_lossy()));
            let mut stmt = self.conn.prepare(&copy).map_err(|e| user_error(e.into(), q.lines_before + 1))?;
            sqlpolicy::bind(&mut stmt, params)?;
            let rows = stmt.raw_execute().map_err(|e| user_error(e.into(), q.lines_before + 1))?;
            Ok((rows as u64, revision))
        })
    }

    /// Every user table with its columns, from one snapshot.
    pub fn schema(&self) -> Result<Vec<TableSchema>> {
        self.snapshot(|| {
            storage::user_tables(self.conn)?
                .into_iter()
                .map(|name| Ok(TableSchema { row_count: self.count_rows(&name)?, columns: self.columns(&name)?, name }))
                .collect()
        })
    }

    pub fn export_view(&self, id: u64, path: &Path, format: ExportFormat) -> Result<()> {
        self.with_view(id, |m| self.copy_to(&self.view_select(m, None)?, path, format))
    }

    pub fn export_table(&self, table: &str, path: &Path, format: ExportFormat) -> Result<()> {
        if is_internal_table(table) {
            bail!(NotAllowed(format!("{table} is internal to Alephgrid")));
        }
        self.snapshot(|| {
            let select = format!("SELECT * EXCLUDE ({ID}, {ORD}) FROM ({}) ORDER BY {ORD}", self.base_select(table)?);
            self.copy_to(&select, path, format)
        })
    }

    pub fn copy_to(&self, select: &str, path: &Path, format: ExportFormat) -> Result<()> {
        let options = copy_options(format);
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

fn copy_options(format: ExportFormat) -> &'static str {
    match format {
        ExportFormat::Csv => "FORMAT csv, HEADER",
        ExportFormat::Parquet => "FORMAT parquet, COMPRESSION zstd",
        ExportFormat::Json => "FORMAT json",
    }
}

/// A checked user query with formula columns exposed, ready to run.
#[derive(Debug, Clone)]
pub(crate) struct Expanded {
    pub sql: String,
    /// Lines added before the user's text, so error positions can be reported in it.
    pub lines_before: usize,
    pub sources: Vec<String>,
}

/// DuckDB's error for user SQL, with `LINE n:` counted in the user's own text.
pub(crate) fn user_error(e: anyhow::Error, lines_before: usize) -> anyhow::Error {
    let text = format!("{e:#}");
    if lines_before == 0 || !text.contains("LINE ") {
        return e;
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(i) = rest.find("LINE ") {
        out.push_str(&rest[..i + 5]);
        rest = &rest[i + 5..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        match digits.parse::<usize>() {
            Ok(n) if n > lines_before && rest[digits.len()..].starts_with(':') => {
                out.push_str(&(n - lines_before).to_string());
                rest = &rest[digits.len()..];
            }
            _ => {}
        }
    }
    out.push_str(rest);
    anyhow!(out)
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
