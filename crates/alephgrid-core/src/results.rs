//! Stored formula results (F.09).
//!
//! Spreadsheet formula columns that read other tables (lookups, aggregates) are computed when
//! rows are read. A page costs little, but sorting, filtering or summarizing by such a column
//! evaluates it for every row of the table. Large tables therefore keep their formula results
//! in the scratch database:
//!
//! * **Snapshot-consistent.** Results are built in one read snapshot and stamped with the
//!   generation and data revision, in that snapshot, of every table they read (the table
//!   itself, the tables its formulas look up, and so on).
//! * **Generation-based invalidation.** Results are fresh while the stamp matches the
//!   snapshot reading them; nothing is invalidated in place. A stale result set stays until a
//!   newer one replaces it.
//! * **Atomic.** A new result set is installed in one step, after it is complete, and only
//!   when its snapshot is newer than the installed one's, so readers see either the old or
//!   the new results, never a mix.
//! * **Cancellable.** Building runs as an interruptible statement; an interrupted or failed
//!   build installs nothing and leaves the last committed results as they were.
//! * **Visibly stale.** A sorted or filtered view built on stale results says so (`stale` on
//!   its rows and info: recalculating, failed with why, or cancelled) instead of evaluating
//!   every row while it waits. Unsorted views read pages live, which is cheap and exact.
//!
//! Small tables are always computed live.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};

use crate::catalog::{Scratch, Shared};
use crate::reads::{error_column, Build, Db};
use crate::sql::ident;
use crate::storage::{self, ID, ORD};

/// Tables with fewer rows than this compute their formula columns live.
pub const RESULTS_MIN_ROWS: u64 = 200_000;

/// (table, generation, data revision) of every table a result set reads, sorted.
pub(crate) type Stamp = Vec<(String, i64, u64)>;

/// A table's formula results as of one snapshot.
pub struct StoredResults {
    pub id: u64,
    /// `(_rf_id, _rf_ord, <formula columns and their error columns, in relation order>)`.
    pub(crate) scratch: Arc<Scratch>,
    pub(crate) stamp: Stamp,
    /// The workbook revision of the snapshot they were built in.
    pub revision: u64,
    pub rows: u64,
    /// Formula columns stored, in relation order (their error columns follow each one).
    pub(crate) formulas: Vec<String>,
}

/// What a table's results are doing, for its views and for the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum ResultsActivity {
    Idle,
    /// A build is queued or running.
    Recalculating,
    /// The last build failed (or was cancelled): why.
    Failed(String),
}

#[derive(Default)]
pub(crate) struct ResultsSlot {
    committed: Option<Arc<StoredResults>>,
    activity: Option<ResultsActivity>,
    /// Ticket of the latest queued build; an older build finishing does not change
    /// `activity` (it was superseded, and its cancellation is not a failure to show).
    ticket: u64,
}

#[derive(Default)]
pub(crate) struct ResultsStore {
    tables: Mutex<HashMap<String, ResultsSlot>>,
    seq: AtomicU64,
    min_rows: AtomicU64,
    builds: AtomicU64,
}

impl ResultsStore {
    pub(crate) fn new() -> ResultsStore {
        ResultsStore { min_rows: AtomicU64::new(RESULTS_MIN_ROWS), ..Default::default() }
    }
}

impl Shared {
    fn results_slots(&self) -> std::sync::MutexGuard<'_, HashMap<String, ResultsSlot>> {
        self.results.tables.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Store results for tables of at least `rows` rows (tests use small tables).
    pub fn set_results_min_rows(&self, rows: u64) {
        self.results.min_rows.store(rows, Ordering::Relaxed);
    }

    /// How many result sets have been built (for tests and diagnostics).
    pub fn results_builds(&self) -> u64 {
        self.results.builds.load(Ordering::Relaxed)
    }

    pub(crate) fn committed_results(&self, table: &str) -> Option<Arc<StoredResults>> {
        self.results_slots().get(&table.to_lowercase()).and_then(|s| s.committed.clone())
    }

    pub fn results_activity(&self, table: &str) -> ResultsActivity {
        self.results_slots().get(&table.to_lowercase()).and_then(|s| s.activity.clone()).unwrap_or(ResultsActivity::Idle)
    }

    /// Mark a build as queued and return its ticket for [`crate::Reader::recalculate`]. The
    /// server does this as soon as a change commits, so views opened meanwhile use the last
    /// results, flagged, instead of building their own.
    pub fn results_queued(&self, table: &str) -> u64 {
        let ticket = self.results.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let mut slots = self.results_slots();
        let slot = slots.entry(table.to_lowercase()).or_default();
        slot.activity = Some(ResultsActivity::Recalculating);
        slot.ticket = ticket;
        ticket
    }

    fn results_done(&self, table: &str, ticket: Option<u64>, outcome: std::result::Result<Option<Arc<StoredResults>>, String>) {
        let mut slots = self.results_slots();
        let slot = slots.entry(table.to_lowercase()).or_default();
        let latest = ticket.is_none_or(|t| t >= slot.ticket);
        match outcome {
            Ok(Some(r)) => {
                if slot.committed.as_ref().is_none_or(|c| c.revision <= r.revision) {
                    slot.committed = Some(r);
                }
                if latest {
                    slot.activity = None;
                }
            }
            Ok(None) if latest => slot.activity = None,
            Ok(None) => {}
            Err(why) if latest => slot.activity = Some(ResultsActivity::Failed(why)),
            Err(_) => {}
        }
    }

    /// Record that queued build `ticket` of `table` ended without running (cancelled while
    /// it waited, for instance); ignored once a newer build was queued.
    pub fn results_failed(&self, table: &str, ticket: u64, why: &str) {
        let mut slots = self.results_slots();
        let slot = slots.entry(table.to_lowercase()).or_default();
        if ticket >= slot.ticket && slot.activity == Some(ResultsActivity::Recalculating) {
            slot.activity = Some(ResultsActivity::Failed(why.to_string()));
        }
    }

    /// Tables whose stored results no longer match the committed data (to recalculate).
    pub(crate) fn tables_with_results(&self) -> Vec<(String, Arc<StoredResults>)> {
        self.results_slots().iter().filter_map(|(t, s)| s.committed.clone().map(|r| (t.clone(), r))).collect()
    }

    /// Forget a table's results (dropped or renamed table).
    pub(crate) fn forget_results(&self, table: &str) {
        self.results_slots().remove(&table.to_lowercase());
    }
}

impl Db<'_> {
    /// Whether `table` keeps stored results: it is large enough and has spreadsheet
    /// formulas that read whole tables.
    pub(crate) fn uses_results(&self, table: &str) -> Result<bool> {
        if self.count_rows(table)? < self.shared.results.min_rows.load(Ordering::Relaxed) {
            return Ok(false);
        }
        use crate::table_formula::Reference;
        let sheet = self.sheet_formulas(table)?;
        Ok(self.formulas(table)?.iter().any(|(name, f)| {
            sheet.contains(name)
                && crate::table_formula::references(f).is_ok_and(|refs| refs.iter().any(|r| matches!(r, Reference::Column { .. } | Reference::Table { .. })))
        }))
    }

    fn stamp_of(&self, reads: &[String]) -> Result<Stamp> {
        let revs = storage::data_revisions(self.conn)?;
        let mut out = Vec::new();
        for t in storage::user_tables(self.conn)? {
            let lower = t.to_lowercase();
            if reads.contains(&lower) {
                let gen = storage::table_meta(self.conn, &t)?.gen;
                out.push((lower, gen, revs.get(&t).copied().unwrap_or(0)));
            }
        }
        out.sort();
        Ok(out)
    }

    /// Whether `r` matches the current snapshot.
    pub(crate) fn results_fresh(&self, r: &StoredResults) -> Result<bool> {
        let reads: Vec<String> = r.stamp.iter().map(|(t, _, _)| t.clone()).collect();
        Ok(self.stamp_of(&reads)? == r.stamp)
    }

    /// Bring `table`'s stored results up to date: build them in one snapshot and install
    /// them when complete. `Ok(false)` when they were already fresh. A failure (an interrupt
    /// included) installs nothing and is recorded for the table's views.
    pub fn recalculate(&self, table: &str, ticket: Option<u64>) -> Result<bool> {
        if self.in_txn {
            bail!("stored results are built outside changes");
        }
        let built = self.snapshot(|| {
            if let Some(r) = self.shared.committed_results(table) {
                if self.results_fresh(&r)? {
                    return Ok(None);
                }
            }
            let build = Build::new();
            let (select, mut reads) = self.base_select_tracked(table, &build)?;
            reads.push(table.to_lowercase());
            reads.sort();
            reads.dedup();
            let stamp = self.stamp_of(&reads)?;
            let revision = storage::revision(self.conn)?;
            let sheet = self.sheet_formulas(table)?;
            let formulas: Vec<String> = self.formulas(table)?.into_iter().map(|(n, _)| n).collect();
            let mut cols = vec![ID.to_string(), ORD.to_string()];
            for f in &formulas {
                cols.push(ident(f));
                if sheet.contains(f) {
                    cols.push(ident(&error_column(f)));
                }
            }
            let scratch = self.shared.new_scratch("rs");
            self.conn.execute_batch(&format!("CREATE TABLE {} AS SELECT {} FROM ({select}) ORDER BY {ID};", scratch.qualified(), cols.join(", ")))?;
            crate::faults::check("recalculate")?;
            let rows: i64 = self.conn.query_row(&format!("SELECT count(*) FROM {}", scratch.qualified()), [], |r| r.get(0))?;
            let id = self.shared.results.seq.fetch_add(1, Ordering::Relaxed) + 1;
            Ok(Some(Arc::new(StoredResults { id, scratch: Arc::new(scratch), stamp, revision, rows: rows.max(0) as u64, formulas })))
        });
        match built {
            Ok(r) => {
                let changed = r.is_some();
                if changed {
                    self.shared.results.builds.fetch_add(1, Ordering::Relaxed);
                }
                self.shared.results_done(table, ticket, Ok(r));
                Ok(changed)
            }
            Err(e) => {
                let text = format!("{e:#}");
                // An interrupt fails the running statement; one that arrives between two
                // statements fails the next one as an aborted transaction instead. A build
                // runs only its own statements, so either means it was cancelled.
                let lower = text.to_lowercase();
                let cancelled = text.contains("INTERRUPT") || lower.contains("interrupted") || lower.contains("current transaction is aborted");
                let why = if cancelled { "cancelled".to_string() } else { text };
                self.shared.results_done(table, ticket, Err(why));
                Err(e)
            }
        }
    }

    /// The results a sorted or filtered view of `table` reads, with why they are stale
    /// (`None`: fresh). Without usable results they are built now, unless a build is already
    /// queued or failed and older results exist, which are then used, flagged. `None` for
    /// tables that compute live.
    pub(crate) fn results_for_view(&self, table: &str) -> Result<Option<(Arc<StoredResults>, Option<String>)>> {
        if self.in_txn || !self.uses_results(table)? {
            return Ok(None);
        }
        if let Some(r) = self.shared.committed_results(table) {
            if self.results_fresh(&r)? {
                return Ok(Some((r, None)));
            }
            match self.shared.results_activity(table) {
                ResultsActivity::Recalculating => return Ok(Some((r, Some("recalculating".into())))),
                ResultsActivity::Failed(why) => return Ok(Some((r, Some(format!("recalculation failed: {why}"))))),
                ResultsActivity::Idle => {}
            }
        }
        self.recalculate(table, None)?;
        Ok(self.shared.committed_results(table).map(|r| (r, None)))
    }

    /// Tables whose stored results are stale in this snapshot (changed by a commit).
    pub(crate) fn stale_results(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for (t, r) in self.shared.tables_with_results() {
            if self.table_exists(&t)? && !self.results_fresh(&r)? {
                out.push(t);
            }
        }
        out.sort();
        Ok(out)
    }

    /// Why results `r` of `table` are stale now, or `None` while fresh.
    pub(crate) fn staleness(&self, table: &str, r: &StoredResults) -> Result<Option<String>> {
        if self.results_fresh(r)? {
            return Ok(None);
        }
        Ok(Some(match self.shared.results_activity(table) {
            ResultsActivity::Failed(why) => format!("recalculation failed: {why}"),
            ResultsActivity::Recalculating => "recalculating".into(),
            ResultsActivity::Idle if self.shared.committed_results(table).is_some_and(|c| c.id != r.id) => "newer results are ready; reopen the view".into(),
            ResultsActivity::Idle => "out of date".into(),
        }))
    }

    /// The table's relation with formula columns read from `r`: stored columns as they are
    /// now, formula values as of `r`. Rows added since `r` was built have no formula values.
    pub(crate) fn results_select(&self, table: &str, r: &StoredResults) -> String {
        format!(
            "SELECT t.*, r.* EXCLUDE ({ID}, {ORD}) FROM main.{} t LEFT JOIN {} r ON r.{ID} = t.{ID}",
            ident(table),
            r.scratch.qualified()
        )
    }

    /// Whether every column in `columns` is a formula column stored in `r`.
    pub(crate) fn only_results(r: &StoredResults, columns: &[&str]) -> bool {
        !columns.is_empty() && columns.iter().all(|c| r.formulas.iter().any(|f| f == c))
    }
}
