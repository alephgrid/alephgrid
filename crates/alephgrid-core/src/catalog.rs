//! In-memory state shared by a workbook's writer and its readers: open views, their
//! materialized permutations, and per-table position indexes.
//!
//! Every cached structure records the table *generation* (`_rf_tables.gen`) it was built
//! from. The generation is read in the same DuckDB snapshot that a reader or writer
//! queries, and a structural change bumps it in the same transaction as the change, so a
//! cache is used only by snapshots it matches. Caches are therefore never "invalidated";
//! they become stale and are rebuilt on next use. Nothing here holds a lock while a query
//! runs, except the per-view lock that stops two readers from sorting the same view twice.
//!
//! ## Positions
//!
//! Display order is the order of `_rf_ord`. A table whose rows were only ever appended is
//! *dense*: row `p` has `_rf_ord = p × ORD_GAP`, so a position is arithmetic and a window
//! is a zone-map-pruned range scan. Otherwise a [`PosIndex`] splits the order into chunks
//! of about [`CHUNK`] rows, each identified by its lowest `_rf_ord`; finding position `p`
//! is a binary search over chunk counts plus one `LIMIT 1 OFFSET k` query inside one
//! chunk. Inserts and deletes update chunk counts; nothing renumbers the table.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Result};
use duckdb::Connection;

use crate::model::{Column, ViewSpec};
use crate::sheets::Sheets;
use crate::sql::ident;
use crate::storage::{self, ORD, ORD_GAP};

/// Target rows per position-index chunk.
pub const CHUNK: u64 = 1 << 16;
/// Most open views per workbook (each may hold a permutation).
pub const MAX_VIEWS: usize = 256;
/// Database attached to every connection of a workbook for permutations. It lives in
/// memory, is shared by the writer and readers, and is never persisted.
pub const SCRATCH: &str = "rf_scratch";

/// State shared by [`crate::Workbook`] and its [`crate::Reader`]s.
pub struct Shared {
    pub(crate) catalog: Mutex<Catalog>,
    pub(crate) sheets: Mutex<Sheets>,
    /// Scratch tables whose last user is gone; dropped by the next operation.
    graveyard: Arc<Mutex<Vec<String>>>,
    scratch_seq: AtomicU64,
    pub(crate) lookups: Mutex<LookupCache>,
    pub(crate) results: crate::results::ResultsStore,
    /// Results of worker (Python) functions (phase 8).
    pub(crate) functions: Arc<crate::external::ResultCache>,
}

/// Grouped lookup relations built into scratch tables, keyed by their SQL. An entry is valid
/// for the data revisions of the tables its SQL reads.
#[derive(Default)]
pub struct LookupCache {
    entries: HashMap<String, LookupEntry>,
    tick: u64,
}

struct LookupEntry {
    revisions: Vec<(String, u64)>,
    /// None when the result was too large to keep.
    table: Option<Arc<Scratch>>,
    rows: u64,
    used: u64,
}

impl LookupCache {
    /// The scratch table holding `sql`'s rows at `revisions`; `Some(None)` when that result
    /// is known to be too large to keep; `None` when it is not cached.
    pub(crate) fn get(&mut self, sql: &str, revisions: &[(String, u64)]) -> Option<Option<Arc<Scratch>>> {
        self.tick += 1;
        let tick = self.tick;
        let e = self.entries.get_mut(sql).filter(|e| e.revisions == revisions)?;
        e.used = tick;
        Some(e.table.clone())
    }

    /// Keep `table` (`rows` rows of `sql` at `revisions`), dropping least recently used
    /// entries beyond [`crate::limits::LOOKUP_CACHE_ROWS`].
    pub(crate) fn put(&mut self, sql: &str, revisions: Vec<(String, u64)>, table: Option<Arc<Scratch>>, rows: u64) {
        self.tick += 1;
        self.entries.insert(sql.to_string(), LookupEntry { revisions, table, rows, used: self.tick });
        while self.rows() > crate::limits::LOOKUP_CACHE_ROWS {
            let Some(oldest) = self.entries.iter().filter(|(_, e)| e.table.is_some()).min_by_key(|(_, e)| e.used).map(|(k, _)| k.clone()) else { break };
            self.entries.remove(&oldest);
        }
    }

    /// Rows held by cached lookups.
    pub fn rows(&self) -> u64 {
        self.entries.values().filter(|e| e.table.is_some()).map(|e| e.rows).sum()
    }

    /// Cached lookups holding rows (for tests and diagnostics).
    pub fn len(&self) -> usize {
        self.entries.values().filter(|e| e.table.is_some()).count()
    }

    /// Whether no lookup holds rows.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Scratch tables of the cached lookups, sorted (for tests).
    pub fn tables(&self) -> Vec<String> {
        let mut out: Vec<String> = self.entries.values().filter_map(|e| e.table.as_ref().map(|t| t.qualified())).collect();
        out.sort();
        out
    }
}

#[derive(Default)]
pub struct Catalog {
    views: HashMap<u64, Arc<ViewEntry>>,
    next_view: u64,
    indexes: HashMap<String, Arc<PosIndex>>,
}

pub struct ViewEntry {
    pub id: u64,
    pub(crate) spec: Mutex<ViewSpec>,
    /// Built on demand, rebuilt when stale or expired. Locked while building.
    pub(crate) mat: Mutex<Option<Arc<Materialized>>>,
    last_used_ms: AtomicU64,
}

/// A view as of one table generation.
pub struct Materialized {
    pub gen: i64,
    /// Spec the materialization was built for; a spec change makes it stale too.
    pub spec: ViewSpec,
    pub columns: Vec<Column>,
    pub row_count: u64,
    /// `rf_scratch` table `(_rf_pos, _rf_id)` for sorted or filtered views.
    pub perm: Option<Arc<Scratch>>,
    /// The stored formula results the permutation was built from; its pages read them too,
    /// so order and values agree (flagged when stale).
    pub results: Option<Arc<crate::results::StoredResults>>,
}

/// A scratch table, dropped (by [`Shared::sweep`]) when its last reference goes.
pub struct Scratch {
    pub name: String,
    graveyard: Arc<Mutex<Vec<String>>>,
}

impl Scratch {
    pub fn qualified(&self) -> String {
        format!("{SCRATCH}.main.{}", ident(&self.name))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        self.graveyard.lock().unwrap_or_else(|e| e.into_inner()).push(std::mem::take(&mut self.name));
    }
}

impl Shared {
    /// Results of this workbook's worker (Python) functions.
    pub fn function_results(&self) -> Arc<crate::external::ResultCache> {
        self.functions.clone()
    }

    /// The sheet hook of this workbook's worker functions, for a sheet model loaded again.
    pub(crate) fn external_hook(&self) -> ironcalc::base::external::ExternalHook {
        Arc::new(crate::external::SheetHook(self.functions.clone()))
    }

    pub(crate) fn new(mut sheets: Sheets) -> Shared {
        let functions = Arc::new(crate::external::ResultCache::default());
        sheets.set_external(Some(Arc::new(crate::external::SheetHook(functions.clone()))));
        Shared {
            functions,
            catalog: Mutex::new(Catalog { next_view: 1, ..Default::default() }),
            sheets: Mutex::new(sheets),
            graveyard: Arc::new(Mutex::new(Vec::new())),
            scratch_seq: AtomicU64::new(0),
            lookups: Mutex::new(LookupCache::default()),
            results: crate::results::ResultsStore::new(),
        }
    }

    fn catalog(&self) -> std::sync::MutexGuard<'_, Catalog> {
        self.catalog.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn new_scratch(&self, prefix: &str) -> Scratch {
        let n = self.scratch_seq.fetch_add(1, Ordering::Relaxed);
        Scratch { name: format!("{prefix}_{n}"), graveyard: self.graveyard.clone() }
    }

    /// Drop scratch tables nobody uses any more. Must run outside a transaction that
    /// writes the workbook (a transaction writes to one database only).
    pub(crate) fn sweep(&self, conn: &Connection) {
        let names: Vec<String> = std::mem::take(&mut *self.graveyard.lock().unwrap_or_else(|e| e.into_inner()));
        for name in names {
            let _ = conn.execute_batch(&format!("DROP TABLE IF EXISTS {SCRATCH}.main.{};", ident(&name)));
        }
    }

    /// Scratch tables waiting to be dropped (for tests).
    pub fn pending_drops(&self) -> usize {
        self.graveyard.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub(crate) fn register_view(&self, spec: ViewSpec) -> Result<Arc<ViewEntry>> {
        let mut c = self.catalog();
        if c.views.len() >= MAX_VIEWS {
            bail!("too many open views in this workbook (max {MAX_VIEWS}); close some tabs and try again");
        }
        let id = c.next_view;
        c.next_view += 1;
        let entry = Arc::new(ViewEntry {
            id,
            spec: Mutex::new(spec),
            mat: Mutex::new(None),
            last_used_ms: AtomicU64::new(storage::now_ms()),
        });
        c.views.insert(id, entry.clone());
        Ok(entry)
    }

    pub(crate) fn view(&self, id: u64) -> Result<Arc<ViewEntry>> {
        let v = self.catalog().views.get(&id).cloned().ok_or_else(|| anyhow!("unknown view {id}"))?;
        v.last_used_ms.store(storage::now_ms(), Ordering::Relaxed);
        Ok(v)
    }

    pub(crate) fn remove_view(&self, id: u64) -> Option<Arc<ViewEntry>> {
        self.catalog().views.remove(&id)
    }

    pub(crate) fn views(&self) -> Vec<Arc<ViewEntry>> {
        self.catalog().views.values().cloned().collect()
    }

    pub fn open_view_count(&self) -> usize {
        self.catalog().views.len()
    }

    /// Drop the materializations of views not used for `idle_ms`; they are rebuilt from
    /// their spec on next use. Returns how many were dropped.
    pub fn expire_views(&self, idle_ms: u64) -> usize {
        let now = storage::now_ms();
        let mut n = 0;
        for v in self.views() {
            if now.saturating_sub(v.last_used_ms.load(Ordering::Relaxed)) >= idle_ms {
                if let Ok(mut m) = v.mat.try_lock() {
                    if m.take().is_some() {
                        n += 1;
                    }
                }
            }
        }
        n
    }

    /// Rows held by materialized permutations (an estimate of scratch memory / 16 bytes).
    pub fn materialized_rows(&self) -> u64 {
        self.views()
            .iter()
            .filter_map(|v| v.mat.try_lock().ok().and_then(|m| m.as_ref().filter(|m| m.perm.is_some()).map(|m| m.row_count)))
            .sum()
    }

    pub(crate) fn cached_index(&self, table: &str, gen: i64) -> Option<Arc<PosIndex>> {
        self.catalog().indexes.get(table).filter(|i| i.gen == gen).cloned()
    }

    pub(crate) fn store_index(&self, table: &str, index: PosIndex) {
        self.catalog().indexes.insert(table.to_string(), Arc::new(index));
    }

    pub(crate) fn forget_table(&self, table: &str) {
        self.forget_results(table);
        let mut c = self.catalog();
        c.indexes.remove(table);
        c.views.retain(|_, v| v.spec.lock().unwrap_or_else(|e| e.into_inner()).table != table);
    }

    /// Apply `f` to the spec of every open view of `table` (after a rename).
    pub(crate) fn rewrite_specs(&self, table: &str, mut f: impl FnMut(&mut ViewSpec)) {
        for v in self.views() {
            let mut spec = v.spec.lock().unwrap_or_else(|e| e.into_inner());
            if spec.table == table {
                f(&mut spec);
            }
        }
        if let Some(i) = self.catalog().indexes.remove(table) {
            drop(i);
        }
    }
}

impl ViewEntry {
    pub fn spec(&self) -> ViewSpec {
        self.spec.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

// ---------------------------------------------------------------- PosIndex

/// Positions → `_rf_ord` for one table generation.
#[derive(Debug, Clone)]
pub struct PosIndex {
    pub gen: i64,
    pub rows: u64,
    kind: IndexKind,
}

#[derive(Debug, Clone)]
enum IndexKind {
    Dense,
    /// `lo` ascending; chunk `i` holds the rows with `lo[i] <= _rf_ord < lo[i + 1]`.
    /// The first `lo` is `i64::MIN`, so every possible order key falls in some chunk.
    Chunks { lo: Vec<i64>, count: Vec<u64>, start: Vec<u64> },
}

impl PosIndex {
    pub fn dense(gen: i64, rows: u64) -> PosIndex {
        PosIndex { gen, rows, kind: IndexKind::Dense }
    }

    pub fn is_dense(&self) -> bool {
        matches!(self.kind, IndexKind::Dense)
    }

    pub fn chunk_count(&self) -> usize {
        match &self.kind {
            IndexKind::Dense => 0,
            IndexKind::Chunks { lo, .. } => lo.len(),
        }
    }

    /// Read the chunk boundaries of `table` with one ordered scan of `_rf_ord`.
    pub fn build(conn: &Connection, table: &str, gen: i64, dense: bool) -> Result<PosIndex> {
        let rows: i64 = conn.query_row(&format!("SELECT count(*) FROM main.{}", ident(table)), [], |r| r.get(0))?;
        let rows = rows as u64;
        if dense {
            return Ok(PosIndex::dense(gen, rows));
        }
        let mut lo: Vec<i64> = conn
            .prepare(&format!(
                "SELECT {ORD} FROM (SELECT {ORD}, row_number() OVER (ORDER BY {ORD}) - 1 AS rn FROM main.{})
                 WHERE rn % {CHUNK} = 0 ORDER BY {ORD}",
                ident(table)
            ))?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        if lo.is_empty() {
            lo.push(i64::MIN);
        }
        lo[0] = i64::MIN;
        let n = lo.len() as u64;
        let mut count = vec![CHUNK; lo.len()];
        if let Some(last) = count.last_mut() {
            *last = rows - CHUNK * (n - 1);
        }
        let mut index = PosIndex { gen, rows, kind: IndexKind::Chunks { lo, count, start: vec![] } };
        index.recount();
        Ok(index)
    }

    fn recount(&mut self) {
        if let IndexKind::Chunks { count, start, lo } = &mut self.kind {
            // Drop empty chunks, keeping the first boundary at i64::MIN.
            let mut i = 0;
            while i < count.len() {
                if count[i] == 0 && count.len() > 1 {
                    count.remove(i);
                    lo.remove(i);
                } else {
                    i += 1;
                }
            }
            lo[0] = i64::MIN;
            start.clear();
            let mut s = 0;
            for c in count.iter() {
                start.push(s);
                s += c;
            }
            self.rows = s;
        }
    }

    /// Chunk boundaries for a dense table, computed without reading it.
    fn to_chunks(&self) -> PosIndex {
        match &self.kind {
            IndexKind::Chunks { .. } => self.clone(),
            IndexKind::Dense => {
                let n = self.rows.div_ceil(CHUNK).max(1);
                let lo: Vec<i64> = (0..n).map(|i| if i == 0 { i64::MIN } else { (i * CHUNK) as i64 * ORD_GAP }).collect();
                let count: Vec<u64> = (0..n).map(|i| CHUNK.min(self.rows.saturating_sub(i * CHUNK))).collect();
                let mut out = PosIndex { gen: self.gen, rows: self.rows, kind: IndexKind::Chunks { lo, count, start: vec![] } };
                out.recount();
                out
            }
        }
    }

    /// `_rf_ord` of the row at `pos` (< rows).
    pub fn ord_at(&self, conn: &Connection, table: &str, pos: u64) -> Result<i64> {
        if pos >= self.rows {
            bail!("row {pos} is outside the table ({} rows)", self.rows);
        }
        match &self.kind {
            IndexKind::Dense => Ok(pos as i64 * ORD_GAP),
            IndexKind::Chunks { lo, start, .. } => {
                let i = start.partition_point(|&s| s <= pos) - 1;
                let offset = pos - start[i];
                let upper = lo.get(i + 1).map(|hi| format!(" AND {ORD} < {hi}")).unwrap_or_default();
                let ord: Option<i64> = conn
                    .query_row(
                        &format!(
                            "SELECT {ORD} FROM main.{} WHERE {ORD} >= {}{upper} ORDER BY {ORD} LIMIT 1 OFFSET {offset}",
                            ident(table),
                            lo[i]
                        ),
                        [],
                        |r| r.get(0),
                    )
                    .ok();
                ord.ok_or_else(|| anyhow!("position index of {table:?} is out of date"))
            }
        }
    }

    /// Inclusive `_rf_ord` bounds of positions `[start, end)`, or `None` if empty.
    pub fn ord_bounds(&self, conn: &Connection, table: &str, start: u64, end: u64) -> Result<Option<(i64, i64)>> {
        let end = end.min(self.rows);
        if start >= end {
            return Ok(None);
        }
        Ok(Some((self.ord_at(conn, table, start)?, self.ord_at(conn, table, end - 1)?)))
    }

    /// SQL condition selecting positions `[start, end)`.
    pub fn range_condition(&self, conn: &Connection, table: &str, start: u64, end: u64) -> Result<String> {
        Ok(match self.ord_bounds(conn, table, start, end)? {
            Some((lo, hi)) => format!("{ORD} BETWEEN {lo} AND {hi}"),
            None => "FALSE".into(),
        })
    }

    /// The index after appending `n` rows at the end.
    pub fn appended(&self, n: u64) -> PosIndex {
        let mut out = self.clone();
        match &mut out.kind {
            IndexKind::Dense => out.rows += n,
            IndexKind::Chunks { count, .. } => {
                *count.last_mut().unwrap() += n;
                out.recount();
            }
        }
        out
    }

    /// The index after inserting rows with order keys `ords` (no other row's key changed).
    /// Each key is counted in the chunk whose range holds it: a boundary can lie between
    /// two neighbours when the row that defined it was deleted.
    pub fn inserted(&self, ords: &[i64]) -> PosIndex {
        let mut out = self.to_chunks();
        if let IndexKind::Chunks { lo, count, .. } = &mut out.kind {
            for &o in ords {
                let i = lo.partition_point(|&l| l <= o) - 1;
                count[i] += 1;
            }
        }
        out.recount();
        out
    }

    /// The index after deleting positions `[start, end)`. A dense table stays dense when
    /// the deleted rows were its last ones.
    pub fn deleted(&self, start: u64, end: u64) -> PosIndex {
        let end = end.min(self.rows);
        if self.is_dense() && end == self.rows {
            return PosIndex::dense(self.gen, start);
        }
        let mut out = self.to_chunks();
        if let IndexKind::Chunks { count, start: starts, .. } = &mut out.kind {
            for i in 0..count.len() {
                let (s, e) = (starts[i], starts[i] + count[i]);
                let overlap = e.min(end).saturating_sub(s.max(start));
                count[i] -= overlap;
            }
        }
        out.recount();
        out
    }

    /// Split chunks that grew beyond `4 × CHUNK` rows, so `OFFSET` scans stay short.
    pub fn split_large(mut self, conn: &Connection, table: &str) -> Result<PosIndex> {
        loop {
            let IndexKind::Chunks { count, .. } = &self.kind else { return Ok(self) };
            let Some(i) = count.iter().position(|&c| c > 4 * CHUNK) else { return Ok(self) };
            let (pos, half) = match &self.kind {
                IndexKind::Chunks { start, count, .. } => (start[i] + count[i] / 2, count[i] / 2),
                _ => unreachable!(),
            };
            let ord = self.ord_at(conn, table, pos)?;
            if let IndexKind::Chunks { lo, count, .. } = &mut self.kind {
                let rest = count[i] - half;
                count[i] = half;
                lo.insert(i + 1, ord);
                count.insert(i + 1, rest);
            }
            self.recount();
        }
    }

    /// Check the index against the table (tests only): every chunk count, and that the
    /// dense invariant holds.
    pub fn verify(&self, conn: &Connection, table: &str) -> Result<()> {
        let t = ident(table);
        match &self.kind {
            IndexKind::Dense => {
                let bad: i64 = conn.query_row(
                    &format!("SELECT count(*) FROM (SELECT {ORD}, row_number() OVER (ORDER BY {ORD}) - 1 AS rn FROM main.{t}) WHERE {ORD} <> rn * {ORD_GAP}"),
                    [],
                    |r| r.get(0),
                )?;
                if bad != 0 {
                    bail!("{bad} rows break the dense order of {table}");
                }
            }
            IndexKind::Chunks { lo, count, .. } => {
                for i in 0..lo.len() {
                    let upper = lo.get(i + 1).map(|hi| format!(" AND {ORD} < {hi}")).unwrap_or_default();
                    let n: i64 = conn.query_row(&format!("SELECT count(*) FROM main.{t} WHERE {ORD} >= {}{upper}", lo[i]), [], |r| r.get(0))?;
                    if n as u64 != count[i] {
                        bail!("chunk {i} of {table} holds {n} rows, index says {}", count[i]);
                    }
                }
            }
        }
        Ok(())
    }
}
