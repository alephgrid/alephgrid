//! Workbook and space registry.
//!
//! ```text
//! <data-dir>/.control/control.duckdb    organizations, users, spaces, roles, sessions, audit (auth/store.rs)
//! <data-dir>/<workbook-id>/workbook.json  name, space, star, times, summary
//! <data-dir>/<workbook-id>/data.duckdb  tables, formula metadata, IronCalc sheets, revision history
//! <data-dir>/<workbook-id>/.owner.lock  flock held by the live allocation (see runtime/local.rs)
//! <data-dir>/<workbook-id>/owner.json   who holds it, under which epoch
//! <data-dir>/.control/                  runtime specs, allocation records, controller lock
//! <data-dir>/.trash/<workbook-id>/      deleted workbooks (same layout + deleted_at), restorable
//! <data-dir>/.uploads/<workbook>/<random>/<file>  uploads in progress; removed after import or failure
//! <data-dir>/.exports/<workbook>/<random>/<file>  exports being streamed or waiting for their
//!                                                 download; removed once taken or expired
//! <data-dir>/.tmp/<workbook-id>/        DuckDB spill files of a live allocation
//! ```
//!
//! A workbook's data and identity live in its directory; the compute that serves it is an
//! allocation managed by [`crate::runtime::controller::Controller`]. Deleting a workbook
//! blocks new allocations, stops the live one (draining its work) and moves the
//! directory to the trash with one rename. Nothing is removed until the trash is purged.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use alephgrid_core::{Reader, Workbook};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use ts_rs::TS;

use crate::runtime::budget::HostBudget;
use crate::sched::{Class, JobError, Key, Lane, Scheduler};
use crate::runtime::controller::{operation_id, Controller, StageHook, StopReason};
use crate::runtime::controller::Admission;
use crate::runtime::{ComputePolicy, ComputeProfile, FailureKind, IsolationClass, OwnershipAssignment, ProviderKind, Reservation, RuntimeFailure, STANDARD};
use crate::ws::Event;

// The data directory's layout: one directory per workbook, named by its id, holding its
// database and its metadata, beside dot-directories the server keeps for itself.
/// A workbook's database file, in its directory.
pub const DB_FILE: &str = "data.duckdb";
/// A workbook's metadata file, in its directory.
pub const META_FILE: &str = "workbook.json";
/// Per-workbook scratch for uploads being staged.
pub const UPLOADS: &str = ".uploads";
/// Per-workbook scratch for exports and snapshots being written.
pub const EXPORTS: &str = ".exports";
/// Online backups.
pub const BACKUPS: &str = ".backups";
/// Deleted workbooks, until purged.
pub const TRASH: &str = ".trash";

/// Whether `dir` is one of the server's own dot-directories rather than a workbook.
pub fn is_hidden(dir: &Path) -> bool {
    dir.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
}

/// DuckDB handle types, re-exported by the engine's dependency.
pub mod duckdb_handles {
    pub use alephgrid_core::duckdb_handles::{InterruptHandle, ProgressHandle};
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
pub struct Summary {
    pub tables: usize,
    pub sheets: usize,
    pub rows: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct WorkbookMeta {
    pub id: String,
    pub name: String,
    pub created_at: u64,
    /// The space that owns the workbook; its roles decide who may use it.
    #[serde(default)]
    pub space: String,
    /// The workspace label of workbooks made before spaces (migrated at startup).
    #[serde(default, rename = "workspace", skip_serializing)]
    #[ts(skip)]
    pub legacy_workspace: Option<String>,
    #[serde(default)]
    pub starred: bool,
    #[serde(default)]
    pub opened_at: u64,
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default)]
    pub summary: Summary,
    /// Set while the workbook is in the trash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub deleted_at: Option<u64>,
    /// Id of the [`crate::runtime::RuntimeSpec`] the workbook was last opened with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub runtime_spec: Option<String>,
    /// The compute profile it runs with (7.15); `None` is `standard`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub profile: Option<String>,
}

/// What the workbook list shows: metadata plus size on disk.
#[derive(Debug, Clone, Serialize, TS)]
pub struct WorkbookListing {
    #[serde(flatten)]
    pub meta: WorkbookMeta,
    pub size_bytes: u64,
    /// The asking principal's role in the workbook's space (workbook lists).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub role: Option<crate::auth::SpaceRole>,
    /// Its compute right now (sleeping when closed; normal, not a failure).
    pub compute: crate::runtime::ComputeState,
}

/// A workspace label of an older version, read once by the migration to spaces.
#[derive(Debug, Clone, Deserialize)]
struct LegacyWorkspace {
    id: String,
    name: String,
}

/// Why a handle stopped serving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closed {
    Deleted,
    Stopped,
}

impl Closed {
    fn message(self) -> &'static str {
        match self {
            Closed::Deleted => "this workbook was deleted",
            Closed::Stopped => "this workbook was closed on the server; reconnect to reopen it",
        }
    }
}

/// A live allocation of one workbook: the writer connection, reader connections that run
/// alongside it, and a control connection for settings. Owned by the controller; sessions
/// and requests hold clones while they use it.
pub struct WorkbookHandle {
    pub id: String,
    pub meta: Mutex<WorkbookMeta>,
    /// Read the progress of, or interrupt, the statement running on the writer; usable
    /// without the engine lock (which that statement holds).
    pub progress: duckdb_handles::ProgressHandle,
    pub interrupt: Arc<duckdb_handles::InterruptHandle>,
    /// Stops a running sheet evaluation (SEC.07).
    sheet_cancel: Arc<std::sync::atomic::AtomicBool>,
    /// The sheets' change count at the last memory estimate (SEC.12).
    sheet_seen: AtomicU64,
    reader_interrupts: Vec<Arc<duckdb_handles::InterruptHandle>>,
    reader_progress: Vec<duckdb_handles::ProgressHandle>,
    /// Orders and bounds the work on this workbook (see `sched.rs`).
    pub sched: Arc<Scheduler>,
    /// Client-chosen id of the long job running now (an import), for progress and cancel.
    pub job: Mutex<Option<String>>,
    /// Background recalculations of stored formula results, per table (F.09): the queue
    /// ticket and the channel that cancels it.
    recalcs: Mutex<std::collections::HashMap<String, (u64, tokio::sync::watch::Sender<bool>)>>,
    engine: Mutex<Option<Workbook>>,
    readers: Vec<Mutex<Option<Reader>>>,
    control: Mutex<Option<Reader>>,
    closed: Mutex<Option<Closed>>,
    /// Operations running now (writer or readers).
    active: AtomicUsize,
    sessions: AtomicU64,
    /// Leases held now (requests, operations, sessions setting up).
    uses: AtomicUsize,
    /// Background work besides scheduled jobs (computing worker functions); keeps the
    /// allocation from being idle.
    background: AtomicUsize,
    /// Results of worker (Python) functions and the calls still to compute (phase 8).
    pub functions: Arc<alephgrid_core::external::ResultCache>,
    fn_job: Mutex<FnJob>,
    /// Whether any session has connected to this allocation.
    had_session: AtomicBool,
    last_activity_ms: AtomicU64,
    pub events: broadcast::Sender<Event>,
    pub dir: PathBuf,
    /// The ownership lock; dropping the file releases it.
    lock: Mutex<Option<File>>,
    pub temp_dir: PathBuf,
    pub ownership: OwnershipAssignment,
    /// Where its jobs are counted (9.13); set by the controller once it opened.
    pub metrics: std::sync::OnceLock<Arc<crate::metrics::Metrics>>,
}

/// A use of a live allocation: while any lease is held the workbook is not closed as idle
/// or to make room. Requests hold one from the moment the controller hands out the handle;
/// sessions hold one while they set up and while each operation runs, so an open
/// connection alone does not keep compute running (7.16). Dropping it counts as activity.
pub struct Lease(Arc<WorkbookHandle>);

impl Lease {
    pub fn new(handle: Arc<WorkbookHandle>) -> Lease {
        handle.uses.fetch_add(1, Ordering::SeqCst);
        Lease(handle)
    }

    /// The handle, for keeping after the lease ends (a session).
    pub fn handle(&self) -> Arc<WorkbookHandle> {
        self.0.clone()
    }
}

impl std::ops::Deref for Lease {
    type Target = Arc<WorkbookHandle>;
    fn deref(&self) -> &Arc<WorkbookHandle> {
        &self.0
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.uses.fetch_sub(1, Ordering::SeqCst);
        self.0.touch_activity();
    }
}

/// How many workers a runtime runs at once (1 for one that does not say).
fn runtime_workers(runtime: &Arc<dyn alephgrid_core::external::FunctionRuntime>) -> usize {
    runtime.parallelism()
}

/// The background computation of worker functions of one allocation.
#[derive(Default)]
struct FnJob {
    running: bool,
    /// Asked again while running: look for missing calls once more when done.
    again: bool,
    cancel: Arc<AtomicBool>,
}

/// Counts an operation as active for as long as it lives.
struct Active<'a>(&'a WorkbookHandle);

impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        self.0.touch_activity();
    }
}

impl WorkbookHandle {
    pub fn new(
        meta: WorkbookMeta,
        dir: PathBuf,
        engine: Workbook,
        readers: usize,
        lock: File,
        temp_dir: PathBuf,
        ownership: OwnershipAssignment,
    ) -> Result<WorkbookHandle> {
        let readers: Vec<Reader> = (0..readers).map(|_| engine.reader()).collect::<Result<_>>()?;
        let functions = engine.function_results();
        let control = engine.reader()?;
        let (events, _) = broadcast::channel(256);
        let reader_interrupts: Vec<_> = readers.iter().map(|r| r.interrupt_handle()).collect();
        let sched = Scheduler::new(engine.interrupt_handle(), reader_interrupts.clone(), crate::sched::Limits::default());
        sched.set_sheet_cancel(engine.sheet_cancel());
        let sheet_cancel = engine.sheet_cancel();
        Ok(WorkbookHandle {
            id: meta.id.clone(),
            progress: engine.progress_handle(),
            interrupt: engine.interrupt_handle(),
            sched,
            sheet_cancel,
            sheet_seen: AtomicU64::new(u64::MAX),
            reader_interrupts,
            reader_progress: readers.iter().map(|r| r.progress_handle()).collect(),
            job: Mutex::new(None),
            recalcs: Mutex::new(Default::default()),
            meta: Mutex::new(meta),
            engine: Mutex::new(Some(engine)),
            readers: readers.into_iter().map(|r| Mutex::new(Some(r))).collect(),
            control: Mutex::new(Some(control)),
            closed: Mutex::new(None),
            active: AtomicUsize::new(0),
            sessions: AtomicU64::new(0),
            uses: AtomicUsize::new(0),
            background: AtomicUsize::new(0),
            functions,
            fn_job: Mutex::new(FnJob::default()),
            had_session: AtomicBool::new(false),
            last_activity_ms: AtomicU64::new(alephgrid_core::storage::now_ms()),
            events,
            dir,
            lock: Mutex::new(Some(lock)),
            temp_dir,
            ownership,
            metrics: std::sync::OnceLock::new(),
        })
    }

    /// Recalculate the stored formula results of `queued` tables (with their queue tickets,
    /// see `Shared::results_queued`) in the background, one bulk read job each. A newer
    /// request for a table cancels the one still running for it. When a job ends (done,
    /// failed or cancelled) every session gets `table_changed`, so views built on the old
    /// results reopen or show why they are stale.
    pub fn recalculate_later(self: &Arc<Self>, queued: Vec<(String, u64)>, revision: u64, timeout: Duration) {
        static N: AtomicU64 = AtomicU64::new(1);
        for (table, ticket) in queued {
            let (tx, rx) = tokio::sync::watch::channel(false);
            if let Some((_, older)) = self.recalcs.lock().unwrap_or_else(|e| e.into_inner()).insert(table.clone(), (ticket, tx)) {
                let _ = older.send(true);
            }
            let h = self.clone();
            tokio::spawn(async move {
                let job = Job { key: (0, N.fetch_add(1, Ordering::Relaxed)), class: Class::Bulk, deadline: Instant::now() + timeout };
                let t = table.clone();
                let out = h.run(job, Some(rx), None, Work::Read(Box::new(move |r: &Reader| r.recalculate(&t, Some(ticket))))).await;
                {
                    let mut running = h.recalcs.lock().unwrap_or_else(|e| e.into_inner());
                    if running.get(&table).is_some_and(|(k, _)| *k == ticket) {
                        running.remove(&table);
                    }
                }
                match &out {
                    Ok(false) => return,
                    Ok(true) => {}
                    Err(e) => {
                        // Cancelled or timed out before it started: say so for this ticket.
                        let why = match e.downcast_ref::<JobError>() { Some(JobError::Cancelled) => "cancelled".to_string(), _ => format!("{e:#}") };
                        let _ = h.with_control(|r| {
                            r.shared().results_failed(&table, ticket, &why);
                            Ok(())
                        });
                    }
                }
                let _ = h.events.send(Event::TableChanged { table, origin: 0, revision });
            });
        }
    }

    /// Compute the calls of worker (Python) functions this workbook is missing, in the
    /// background (docs/PYTHON_FUNCTIONS.md): batches of at most
    /// [`alephgrid_core::external::BATCH_CALLS`] calls, run by the runtime's isolated
    /// workers with no lock held, each stored only if its version is still the pinned one.
    /// One job per allocation; a request while it runs makes it look again when done.
    pub fn compute_functions_later(self: &Arc<Self>) {
        if self.functions.runtime_impl().is_none() || self.is_closing() {
            return;
        }
        let cancel = {
            let mut j = self.fn_job.lock().unwrap();
            if j.running {
                j.again = true;
                return;
            }
            j.running = true;
            j.again = false;
            j.cancel = Arc::new(AtomicBool::new(false));
            j.cancel.clone()
        };
        let h = self.clone();
        h.background.fetch_add(1, Ordering::SeqCst);
        std::thread::spawn(move || {
            let done = h.compute_functions(&cancel);
            if let Err(e) = done {
                if !h.is_closing() {
                    tracing::warn!(workbook = h.id.as_str(), "computing Python functions: {e:#}");
                }
            }
            h.fn_job.lock().unwrap().running = false;
            h.background.fetch_sub(1, Ordering::SeqCst);
            h.touch_activity();
        });
    }

    fn compute_functions(self: &Arc<Self>, cancel: &AtomicBool) -> Result<()> {
        use alephgrid_core::external::{check_result, CallContext, CallResult, PendingCall, WorkerFailureKind, WorkerJob, BATCH_BYTES, BATCH_CALLS};
        let Some(runtime) = self.functions.runtime_impl() else { return Ok(()) };
        loop {
            if cancel.load(Ordering::SeqCst) || self.is_closing() || self.functions.is_disabled() {
                return Ok(());
            }
            let mut work: Vec<(CallContext, Vec<PendingCall>)> = Vec::new();
            let sheet = self.functions.take_pending();
            if !sheet.is_empty() {
                work.push((CallContext::Sheet, sheet));
            }
            // One scan finds many batches' worth (bounded), not one batch per scan.
            for (_, calls) in self.with_reader(|r| r.missing_table_calls(BATCH_CALLS * 50))? {
                work.push((CallContext::Table, calls));
            }
            if work.is_empty() {
                let mut j = self.fn_job.lock().unwrap();
                if j.again {
                    j.again = false;
                    continue;
                }
                return Ok(());
            }
            let registry = self.with_reader(|r| r.functions())?;
            for (context, calls) in work {
                // Group by version, then cut into batches by count and size.
                let mut groups: std::collections::BTreeMap<(String, u32), Vec<PendingCall>> = Default::default();
                for c in calls {
                    groups.entry((c.function.clone(), c.version)).or_default().push(c);
                }
                for ((name, version), calls) in groups {
                    let Some(v) = registry.iter().find(|f| f.name == name).and_then(|f| f.versions.iter().find(|v| v.version == version)) else { continue };
                    let d = &v.definition;
                    let returns = d.returns.unwrap_or(alephgrid_core::external::ResultType::Text);
                    let flush = |batch: &mut Vec<PendingCall>| -> Result<bool> {
                        if batch.is_empty() {
                            return Ok(true);
                        }
                        let calls = std::mem::take(batch);
                        let job = WorkerJob {
                            function: name.clone(),
                            version,
                            source: d.source.clone(),
                            entry: alephgrid_core::functions::python_entry(&name),
                            arguments: d.arguments.clone(),
                            returns,
                            context,
                            calls: calls.iter().map(|c| c.args.clone()).collect(),
                        };
                        let results: Vec<CallResult> = match runtime.run(&job, cancel) {
                            Ok(r) => r.into_iter().map(|r| check_result(r, returns, context)).collect(),
                            Err(f) if matches!(f.kind, WorkerFailureKind::Cancelled | WorkerFailureKind::Unavailable) => return Ok(false),
                            // Out of time, memory or output, or a crash: every call of the
                            // batch says why, until it is recalculated.
                            Err(f) => vec![CallResult::error("#CALC!", f.message, true); calls.len()],
                        };
                        let pairs: Vec<(String, CallResult)> = calls.into_iter().map(|c| c.key).zip(results).collect();
                        let stored = self.with_engine(|wb| wb.store_function_results(&name, version, &pairs))?;
                        if stored.applied {
                            let revision = self.with_reader(|r| r.revision()).unwrap_or(0);
                            if stored.sheets {
                                let _ = self.events.send(Event::SheetChanged { sheet: None, origin: 0, revision });
                            }
                            for table in stored.tables {
                                let _ = self.events.send(Event::TableChanged { table, origin: 0, revision });
                            }
                        }
                        Ok(true)
                    };
                    // Cut into batches by count and size.
                    let mut batches: Vec<Vec<PendingCall>> = vec![Vec::new()];
                    let mut bytes = 0;
                    for c in calls {
                        let size = serde_json::to_vec(&c.args).map(|v| v.len()).unwrap_or(0);
                        let last = batches.last().unwrap();
                        if last.len() >= BATCH_CALLS || (bytes + size > BATCH_BYTES && !last.is_empty()) {
                            batches.push(Vec::new());
                            bytes = 0;
                        }
                        bytes += size;
                        batches.last_mut().unwrap().push(c);
                    }
                    // As many at once as the runtime has workers; each stored as it ends.
                    let parallel = (runtime_workers(&runtime)).max(1);
                    for chunk in batches.chunks_mut(parallel) {
                        let outcomes: Vec<Result<bool>> = std::thread::scope(|scope| {
                            let flush = &flush;
                            let handles: Vec<_> = chunk.iter_mut().map(|b| scope.spawn(move || flush(b))).collect();
                            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err(anyhow!("a worker thread panicked")))).collect()
                        });
                        for o in outcomes {
                            if !o? {
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
    }

    /// Stop the worker computing functions for this allocation, if one runs.
    pub fn cancel_functions(&self) {
        self.fn_job.lock().unwrap().cancel.store(true, Ordering::SeqCst);
    }

    /// Cancel the running recalculation of `table`'s stored results; false when none runs.
    pub fn cancel_recalculation(&self, table: &str) -> bool {
        let running = self.recalcs.lock().unwrap_or_else(|e| e.into_inner());
        running.iter().find(|(t, _)| t.eq_ignore_ascii_case(table)).is_some_and(|(_, (_, tx))| tx.send(true).is_ok())
    }

    fn check_open(&self) -> Result<()> {
        match *self.closed.lock().unwrap() {
            Some(c) => bail!(c.message()),
            None => Ok(()),
        }
    }

    fn enter(&self) -> Result<Active<'_>> {
        self.check_open()?;
        self.active.fetch_add(1, Ordering::SeqCst);
        let guard = Active(self);
        // Closed between the check and the increment: the drain may not have seen us.
        self.check_open()?;
        Ok(guard)
    }

    /// Run `f` with exclusive access to the workbook's writer. Fails once the workbook is
    /// closing, including for operations that were waiting for the lock.
    pub fn with_engine<T>(&self, f: impl FnOnce(&mut Workbook) -> Result<T>) -> Result<T> {
        let _active = self.enter()?;
        let mut guard = self.engine.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
            Some(wb) => f(wb),
            None => bail!(self.closed_message()),
        }
    }

    fn closed_message(&self) -> &'static str {
        self.closed.lock().unwrap().unwrap_or(Closed::Stopped).message()
    }

    /// Run `f` on a reader connection (a free one if there is one). Readers see committed
    /// data and do not wait for the writer.
    pub fn with_reader<T>(&self, f: impl FnOnce(&Reader) -> Result<T>) -> Result<T> {
        let _active = self.enter()?;
        for slot in &self.readers {
            if let Ok(guard) = slot.try_lock() {
                return match guard.as_ref() {
                    Some(r) => f(r),
                    None => bail!(self.closed_message()),
                };
            }
        }
        let guard = self.readers[0].lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(r) => f(r),
            None => bail!(self.closed_message()),
        }
    }

    /// Run `f` on reader `slot` (granted by the scheduler).
    pub fn with_reader_slot<T>(&self, slot: usize, f: impl FnOnce(&Reader) -> Result<T>) -> Result<T> {
        let _active = self.enter()?;
        let guard = self.readers[slot.min(self.readers.len() - 1)].lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(r) => f(r),
            None => bail!(self.closed_message()),
        }
    }

    /// Progress of the statement running on a slot.
    pub fn progress_of(&self, lane: Lane, slot: usize) -> alephgrid_core::duckdb_handles::QueryProgress {
        match lane {
            Lane::Write => self.progress.query_progress(),
            Lane::Read => self.reader_progress[slot.min(self.reader_progress.len() - 1)].query_progress(),
        }
    }

    /// Run a job through the scheduler: wait for a slot (until `deadline`, or `cancel`),
    /// run `work` on that slot's connection on the blocking pool, interrupt it if the
    /// deadline passes or it is cancelled while running. The slot stays taken until the
    /// work has really finished, even if the caller stops waiting. `progress` is called
    /// about twice a second while a job that reports progress runs.
    pub async fn run<T: Send + 'static>(
        self: &Arc<Self>,
        job: Job,
        cancel: Option<tokio::sync::watch::Receiver<bool>>,
        progress: Option<Box<dyn Fn(f64) + Send>>,
        work: Work<T>,
    ) -> Result<T> {
        let ticket = self.enqueue(&job, work.lane())?;
        self.run_queued(ticket, job, cancel, progress, work).await
    }

    /// Join the queue now (so requests of one connection keep their order); run later with
    /// [`WorkbookHandle::run_queued`].
    pub fn enqueue(&self, job: &Job, lane: Lane) -> Result<crate::sched::Ticket> {
        self.check_open()?;
        let ticket = self.sched.submit(job.key, lane, job.class);
        if ticket.is_err() {
            self.count_job(job.class, "rejected");
        }
        Ok(ticket?)
    }

    fn count_job(&self, class: Class, outcome: &'static str) {
        if let Some(m) = self.metrics.get() {
            m.inc("alephgrid_jobs_total", &[("class", crate::metrics::class_label(class)), ("outcome", outcome)]);
        }
    }

    pub async fn run_queued<T: Send + 'static>(
        self: &Arc<Self>,
        mut ticket: crate::sched::Ticket,
        job: Job,
        mut cancel: Option<tokio::sync::watch::Receiver<bool>>,
        progress: Option<Box<dyn Fn(f64) + Send>>,
        work: Work<T>,
    ) -> Result<T> {
        let cancelled = |c: &mut Option<tokio::sync::watch::Receiver<bool>>| {
            let mut c = c.clone();
            async move {
                match c.as_mut() {
                    Some(rx) => {
                        while !*rx.borrow() {
                            if rx.changed().await.is_err() {
                                std::future::pending::<()>().await;
                            }
                        }
                    }
                    None => std::future::pending::<()>().await,
                }
            }
        };
        let lane = work.lane();
        let labels = [("class", crate::metrics::class_label(job.class)), ("lane", crate::metrics::lane_label(lane))];
        let slot = tokio::select! {
            slot = ticket.granted() => slot,
            _ = tokio::time::sleep_until(job.deadline.into()) => {
                self.count_job(job.class, "deadline_exceeded");
                bail!(JobError::DeadlineExceeded)
            }
            _ = cancelled(&mut cancel) => {
                self.count_job(job.class, "cancelled");
                bail!(JobError::Cancelled)
            }
        };
        if let Some(m) = self.metrics.get() {
            m.observe("alephgrid_job_wait_seconds", &labels, ticket.queued_at().elapsed());
        }
        let ran = Instant::now();
        let h = self.clone();
        // The job's log span, inside the request or session that started it.
        let span = tracing::info_span!("job", workbook = self.id.as_str(), job = %format_args!("{}.{}", job.key.0, job.key.1), class = ?job.class, lane = ?lane);
        let job_span = span.clone();
        let mut task = tokio::task::spawn_blocking(move || {
            let _span = span.entered();
            // Holding the ticket here frees the slot only after the work is done.
            let _ticket = ticket;
            match work {
                Work::Write(f) => h.with_engine(f),
                Work::Read(f) => h.with_reader_slot(slot, f),
            }
        });
        let mut stopped: Option<JobError> = None;
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        tick.tick().await;
        let result = loop {
            tokio::select! {
                r = &mut task => break r,
                _ = tokio::time::sleep_until(job.deadline.into()), if stopped.is_none() => {
                    stopped = Some(JobError::DeadlineExceeded);
                    self.sched.interrupt(job.key);
                }
                _ = cancelled(&mut cancel), if stopped.is_none() => {
                    stopped = Some(JobError::Cancelled);
                    self.sched.interrupt(job.key);
                }
                _ = tick.tick(), if progress.is_some() || stopped.is_some() => {
                    if stopped.is_some() {
                        // An interrupt that lands between two statements of a job is lost
                        // (the next statement starts afresh), so repeat it until the job ends.
                        self.sched.interrupt(job.key);
                    } else if let Some(p) = self.progress_of(lane, slot).percentage() {
                        (progress.as_ref().unwrap())(p);
                    }
                }
            }
        };
        if let Some(m) = self.metrics.get() {
            m.observe("alephgrid_job_duration_seconds", &labels, ran.elapsed());
        }
        let outcome = match (&result, &stopped) {
            (Ok(Ok(_)), _) => "ok",
            (_, Some(JobError::Cancelled)) => "cancelled",
            (_, Some(_)) => "deadline_exceeded",
            _ => "failed",
        };
        self.count_job(job.class, outcome);
        let result = result.map_err(|e| anyhow!("internal error: {e}"))?;
        if let Some(why) = &stopped {
            job_span.in_scope(|| tracing::info!("job stopped: {why}"));
        }
        match (result, stopped) {
            // Stopped (cancel or deadline) and then failed: report why it was stopped. The
            // interrupt may have hit any statement, not only one that says INTERRUPT. A job
            // that finished anyway keeps its result.
            (Err(_), Some(why)) => Err(anyhow::Error::new(why)),
            (r, _) => r,
        }
    }

    fn with_control<T>(&self, f: impl FnOnce(&Reader) -> Result<T>) -> Result<T> {
        let guard = self.control.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(r) => f(r),
            None => bail!(self.closed_message()),
        }
    }

    pub fn is_deleted(&self) -> bool {
        *self.closed.lock().unwrap() == Some(Closed::Deleted)
    }

    pub fn is_closing(&self) -> bool {
        self.closed.lock().unwrap().is_some()
    }

    /// No operation running or waiting and no job (used with the controller's reference count).
    pub fn is_quiet(&self) -> bool {
        self.active.load(Ordering::SeqCst) == 0
            && self.background.load(Ordering::SeqCst) == 0
            && self.job.lock().unwrap().is_none()
            && self.sched.load() == 0
    }

    pub fn active(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    pub fn touch_activity(&self) {
        self.last_activity_ms.store(alephgrid_core::storage::now_ms(), Ordering::Relaxed);
    }

    pub fn last_activity_ms(&self) -> u64 {
        self.last_activity_ms.load(Ordering::Relaxed)
    }

    pub fn uses(&self) -> usize {
        self.uses.load(Ordering::SeqCst)
    }

    /// Take the ownership lock out, still held, before closing (a controlled restart hands
    /// it to the replacement allocation).
    pub fn take_lock(&self) -> Option<File> {
        self.lock.lock().unwrap().take()
    }

    pub fn had_session(&self) -> bool {
        self.had_session.load(Ordering::SeqCst)
    }

    pub fn session_started(&self) {
        self.had_session.store(true, Ordering::SeqCst);
        self.sessions.fetch_add(1, Ordering::SeqCst);
        self.touch_activity();
    }

    pub fn session_ended(&self) {
        self.sessions.fetch_sub(1, Ordering::SeqCst);
        self.touch_activity();
    }

    pub fn sessions(&self) -> u64 {
        self.sessions.load(Ordering::SeqCst)
    }

    /// Refuse new operations from now on.
    pub fn begin_closing(&self, reason: Closed) {
        let mut c = self.closed.lock().unwrap();
        if c.is_none() {
            *c = Some(reason);
        }
        drop(c);
        // A worker computing functions for this allocation is stopped.
        self.fn_job.lock().unwrap().cancel.store(true, Ordering::SeqCst);
    }

    /// Wait for running operations, up to `deadline`; then interrupt what still runs and
    /// wait for it to stop. Finally checkpoint, so the file holds every committed change.
    pub fn drain(&self, deadline: Duration) -> Result<()> {
        let until = std::time::Instant::now() + deadline;
        while self.active() > 0 && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        if self.active() > 0 {
            tracing::warn!(workbook = self.id.as_str(), active = self.active(), "drain deadline passed; interrupting");
            self.interrupt.interrupt();
            self.sheet_cancel.store(true, Ordering::SeqCst);
            for i in &self.reader_interrupts {
                i.interrupt();
            }
        }
        let guard = self.engine.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(wb) = guard.as_ref() {
            wb.checkpoint().context("checkpoint while draining")?;
        }
        Ok(())
    }

    /// Close every connection and release the ownership lock.
    pub fn close(&self) -> Result<()> {
        self.begin_closing(Closed::Stopped);
        let mut checkpoint = Ok(());
        for slot in &self.readers {
            slot.lock().unwrap_or_else(|e| e.into_inner()).take();
        }
        self.control.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(wb) = self.engine.lock().unwrap_or_else(|e| e.into_inner()).take() {
            checkpoint = wb.checkpoint();
        }
        self.lock.lock().unwrap().take();
        checkpoint
    }

    /// Apply this allocation's share of the host budget to its DuckDB instance.
    pub fn apply_limits(&self, memory: u64, r: &Reservation) -> Result<()> {
        self.with_control(|c| c.configure(&[
            ("memory_limit", format!("{memory}B")),
            ("threads", r.threads.max(1).to_string()),
            ("max_temp_directory_size", format!("{}B", r.temp_disk_bytes.max(1 << 20))),
        ]))
    }

    /// DuckDB's `memory_limit` and `threads` as the instance reports them.
    pub fn limits(&self) -> Result<(String, String)> {
        self.with_control(|c| Ok((c.setting("memory_limit")?, c.setting("threads")?)))
    }

    /// Rough memory the sheet model takes: 8 × its serialized size (IronCalc keeps parsed
    /// formulas, styles and indexes in memory). An estimate for the budget, not a measurement.
    pub fn sheet_memory_estimate(&self) -> u64 {
        self.with_control(|c| {
            let s = c.sheets();
            self.sheet_seen.store(s.changes(), Ordering::SeqCst);
            Ok(s.to_bytes().map(|b| b.len() as u64 * 8).unwrap_or(0))
        })
        .unwrap_or(0)
    }

    /// [`WorkbookHandle::sheet_memory_estimate`] again when the sheets changed since the
    /// last estimate (SEC.12).
    pub fn sheet_estimate_if_changed(&self) -> Option<u64> {
        let changes = self.with_control(|c| Ok(c.sheets().changes())).ok()?;
        (changes != self.sheet_seen.load(Ordering::SeqCst)).then(|| self.sheet_memory_estimate())
    }

    pub fn expire_views(&self, ttl_ms: u64) -> usize {
        self.with_control(|c| Ok(c.shared().expire_views(ttl_ms))).unwrap_or(0)
    }

    /// Recompute counts shown on the home screen and bump `updated_at`.
    pub fn refresh_summary(&self) -> Result<()> {
        let summary = self.with_reader(|r| {
            let tables = r.list_tables()?;
            let sheets = r.sheets().list().len();
            Ok(Summary { tables: tables.len(), sheets, rows: tables.iter().map(|t| t.row_count).sum() })
        })?;
        let mut meta = self.meta.lock().unwrap();
        meta.summary = summary;
        meta.updated_at = now();
        write_meta(&self.dir, &meta)
    }

    pub fn touch(&self, opened: bool) -> Result<()> {
        if self.is_closing() {
            return Ok(());
        }
        let mut meta = self.meta.lock().unwrap();
        if opened {
            meta.opened_at = now();
        } else {
            meta.updated_at = now();
        }
        write_meta(&self.dir, &meta)
    }
}

/// Which scheduler queue a job joins and when it must be done.
#[derive(Debug, Clone, Copy)]
pub struct Job {
    pub key: Key,
    pub class: Class,
    pub deadline: std::time::Instant,
}

pub type WriteFn<T> = Box<dyn FnOnce(&mut Workbook) -> Result<T> + Send>;
pub type ReadFn<T> = Box<dyn FnOnce(&Reader) -> Result<T> + Send>;

/// The work of a job, on the writer or on a reader.
pub enum Work<T> {
    Write(WriteFn<T>),
    Read(ReadFn<T>),
}

impl<T> Work<T> {
    pub fn lane(&self) -> Lane {
        match self {
            Work::Write(_) => Lane::Write,
            Work::Read(_) => Lane::Read,
        }
    }
}

pub struct Config {
    pub data_dir: PathBuf,
    /// Most rows copied from a table or query into a sheet.
    pub sheet_copy_rows: usize,
    /// Sign-in and sessions.
    pub auth: crate::auth::AuthConfig,
    /// Default limits of every space (7.07); a space's own override wins.
    pub quota: crate::quota::QuotaLimits,
    pub allow_sql: bool,
    /// Memory shared by every open workbook's DuckDB instance. Defaults to 80% of RAM (or
    /// of the container's limit).
    pub memory_budget: Option<u64>,
    /// DuckDB worker threads shared by every open workbook. Defaults to all cores.
    pub threads: Option<usize>,
    /// Temporary disk for spills and staged uploads. Defaults to 64 GiB.
    pub temp_budget: Option<u64>,
    /// Disk kept back for a full disk (3.T03); 0 turns it off.
    pub disk_reserve: u64,
    /// Smallest memory share a workbook may run with.
    pub min_workbook_memory: u64,
    /// Close workbooks without meaningful activity (requests, running work) after this long;
    /// open connections do not count.
    pub idle_timeout: Duration,
    /// Compute profiles besides `standard` (7.13).
    pub profiles: Vec<crate::runtime::ComputeProfile>,
    /// A Python runtime directory (`alephgrid python-runtime create`); none: Python
    /// functions cannot run (phase 8).
    pub python_runtime: Option<PathBuf>,
    /// `None`: the host's sandbox. `Some(Isolation::None)` only with `--auth local`.
    pub python_isolation: Option<crate::python::Isolation>,
    pub python_limits: crate::python::Limits,
    /// How long a start waits in line for host capacity.
    pub start_queue_timeout: Duration,
    /// Drop sorted/filtered view materializations unused for this long.
    pub view_ttl: Duration,
    /// Time running work gets to finish when a workbook is drained (shutdown, stop).
    pub shutdown_grace: Duration,
    /// Called at each allocation start stage (tests use it to delay or fail starts).
    pub stage_hook: Option<StageHook>,
    /// Largest WebSocket message a client may send.
    pub max_message_bytes: usize,
    /// Largest file upload.
    pub max_upload_bytes: u64,
    /// Largest a workbook's database may grow; operations that add data are refused past
    /// it (deleting stays possible). None: no limit (SEC.11).
    pub max_workbook_bytes: Option<u64>,
    /// Online backups kept under `<data-dir>/.backups` (older ones are removed), and the
    /// least time between two (SEC.11).
    pub backups_kept: usize,
    pub backup_interval: Duration,
    /// Default and longest time an interactive request may take (waiting included).
    pub request_timeout: Duration,
    /// Default and longest time a bulk job (import, query, sort, copy) may take.
    pub job_timeout: Duration,
    /// A session's unsent replies and events beyond this pause reading its requests.
    pub outbox_pause_bytes: usize,
    /// A session whose unsent data passes this is closed (the client is not reading).
    pub outbox_max_bytes: usize,
    /// A bearer token that may read `GET /metrics` besides instance admins (9.13).
    pub metrics_token: Option<String>,
}

impl Config {
    pub fn new(data_dir: impl Into<PathBuf>) -> Config {
        Config {
            data_dir: data_dir.into(),
            sheet_copy_rows: crate::ws::SHEET_COPY_ROWS,
            auth: crate::auth::AuthConfig::default(),
            quota: crate::quota::QuotaLimits::default(),
            allow_sql: true,
            memory_budget: None,
            threads: None,
            temp_budget: None,
            disk_reserve: 32 << 20,
            min_workbook_memory: 256 << 20,
            idle_timeout: Duration::from_secs(600),
            profiles: Vec::new(),
            python_runtime: None,
            python_isolation: None,
            python_limits: crate::python::Limits::default(),
            start_queue_timeout: Duration::from_secs(60),
            view_ttl: Duration::from_secs(900),
            shutdown_grace: Duration::from_secs(30),
            stage_hook: None,
            max_message_bytes: 16 << 20,
            max_upload_bytes: 100 << 30,
            max_workbook_bytes: None,
            backups_kept: 5,
            backup_interval: Duration::from_secs(60),
            request_timeout: Duration::from_secs(30),
            job_timeout: Duration::from_secs(3600),
            outbox_pause_bytes: 16 << 20,
            outbox_max_bytes: 64 << 20,
            metrics_token: None,
        }
    }
}

/// A file uploaded for preview, waiting for "Import" or "Cancel".
#[derive(Debug, Clone)]
pub struct Upload {
    pub workbook: String,
    pub path: PathBuf,
    pub dir: PathBuf,
    pub size_bytes: u64,
    pub touched: std::time::Instant,
    /// An .xlsx preview, parsed once when the file was staged (SEC.08).
    pub preview: Option<alephgrid_core::UploadPreview>,
}

/// Unclaimed uploads are removed after this long.
pub const UPLOAD_TTL: Duration = Duration::from_secs(3600);
/// Exported query results not downloaded within this long are removed.
pub const DOWNLOAD_TTL: Duration = Duration::from_secs(3600);

/// A finished export waiting to be downloaded once.
#[derive(Debug, Clone)]
pub struct Download {
    pub workbook: String,
    pub dir: PathBuf,
    pub path: PathBuf,
    pub file_name: String,
    pub created: std::time::Instant,
}

/// Exports waiting for their download. Each is taken once; the file is removed when it is
/// taken (after streaming), cancelled or expired.
#[derive(Default)]
pub struct Downloads(Mutex<HashMap<String, Download>>);

impl Downloads {
    pub fn register(&self, workbook: &str, dir: PathBuf, path: PathBuf, file_name: &str) -> String {
        let id = new_id();
        let d = Download { workbook: workbook.to_string(), dir, path, file_name: file_name.to_string(), created: std::time::Instant::now() };
        self.0.lock().unwrap().insert(id.clone(), d);
        id
    }

    /// Take a download of `workbook` (it can be taken only once).
    pub fn take(&self, workbook: &str, id: &str) -> Option<Download> {
        let mut map = self.0.lock().unwrap();
        match map.get(id) {
            Some(d) if d.workbook == workbook => map.remove(id),
            _ => None,
        }
    }

    /// Drop a download and its file.
    pub fn discard(&self, id: &str) {
        if let Some(d) = self.0.lock().unwrap().remove(id) {
            let _ = std::fs::remove_dir_all(d.dir);
        }
    }

    /// Remove downloads older than `ttl`; returns how many.
    pub fn expire(&self, ttl: Duration) -> usize {
        let mut map = self.0.lock().unwrap();
        let before = map.len();
        map.retain(|_, d| {
            let keep = d.created.elapsed() < ttl;
            if !keep {
                let _ = std::fs::remove_dir_all(&d.dir);
            }
            keep
        });
        before - map.len()
    }

    pub fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub struct AppState {
    pub config: Config,
    pub uploads: Mutex<HashMap<String, Upload>>,
    pub controller: Controller,
    /// Identities, roles and sessions.
    pub auth: crate::auth::Auth,
    /// Serializes space deletion with workbooks moving into spaces.
    spaces: Mutex<()>,
    /// Running jobs, starts and measured storage per space (quotas).
    pub quota: crate::quota::Counters,
    next_session: AtomicU64,
    /// Exclusive lock on the data directory: one server per data directory.
    _dir_lock: File,
    pub shutting_down: AtomicBool,
    /// Online backups: one at a time, and when the last one finished (SEC.11).
    pub backup_running: Mutex<()>,
    pub last_backup: Mutex<Option<std::time::Instant>>,
    /// Open WebSocket sessions (for `/api/admin/sessions`).
    pub sessions: Mutex<HashMap<u64, crate::ws::SessionInfo>>,
    /// Exported query results waiting to be downloaded.
    pub downloads: Arc<Downloads>,
    /// The Python runtime workers run in (phase 8), and why there is none when one was
    /// configured but cannot run.
    pub python: Option<Arc<crate::python::PythonRuntime>>,
    pub python_problem: Option<String>,
    /// Lifecycle counters and histograms (`GET /metrics`, 9.13); the controller's.
    pub metrics: Arc<crate::metrics::Metrics>,
    /// When this server started (ms since the epoch).
    pub started_at_ms: u64,
}

pub type Shared = Arc<AppState>;

pub fn now() -> u64 {
    alephgrid_core::storage::now_secs()
}

/// Bytes of a workbook's database (with its write-ahead log).
pub fn database_bytes(workbook_dir: &Path) -> u64 {
    let db = workbook_dir.join(DB_FILE);
    std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0) + std::fs::metadata(db.with_extension("duckdb.wal")).map(|m| m.len()).unwrap_or(0)
}

/// Directories a workbook's database may touch: its upload and export scratch
/// directories and its spill directory. Created here, so DuckDB can resolve them.
pub fn allowed_dirs(data_dir: &Path, workbook: &str, temp: &Path) -> Result<Vec<PathBuf>> {
    let dirs = vec![data_dir.join(UPLOADS).join(workbook), data_dir.join(EXPORTS).join(workbook), temp.to_path_buf()];
    for d in &dirs {
        std::fs::create_dir_all(d)?;
    }
    Ok(dirs)
}

/// A new id for a workbook, upload, export or download: 64 random bits from the operating
/// system's source, as 16 hex digits (SEC.14). Ids name directories and one-time downloads,
/// so they must not be guessable.
pub fn new_id() -> String {
    crate::auth::random_id()
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

// ---------------------------------------------------------------- faults

/// Server-side fault injection for in-process tests, shared by all threads (requests run
/// on the blocking pool). Like the engine's, it can only be armed from Rust code. An armed
/// point can be scoped to paths containing a string, so parallel tests (each with its own
/// data directory) do not trip each other's faults.
static FAULTS: Mutex<Vec<(&'static str, Option<String>, u32)>> = Mutex::new(Vec::new());

/// Arm `point` only for operations on paths containing `scope` (a data directory, say).
pub fn arm_fault_in(point: &'static str, scope: &Path, times: u32) {
    FAULTS.lock().unwrap().push((point, Some(scope.to_string_lossy().to_string()), times));
}

pub fn fault(point: &'static str) -> Result<()> {
    fault_at(point, Path::new(""))
}

/// Fails if `point` is armed for `path`, simulating a full disk.
pub fn fault_at(point: &'static str, path: &Path) -> Result<()> {
    let p = path.to_string_lossy();
    let mut f = FAULTS.lock().unwrap();
    if let Some(e) = f.iter_mut().find(|(pt, scope, n)| *pt == point && *n > 0 && scope.as_ref().is_none_or(|s| p.contains(s.as_str()))) {
        e.2 -= 1;
        bail!(std::io::Error::other(format!("injected fault at {point}: no space left on device")));
    }
    Ok(())
}

impl AppState {
    pub fn new(config: Config) -> Result<Shared> {
        std::fs::create_dir_all(&config.data_dir)
            .with_context(|| format!("creating data dir {}", config.data_dir.display()))?;
        let control = config.data_dir.join(".control");
        std::fs::create_dir_all(&control)?;
        let dir_lock = File::create(control.join("controller.lock"))?;
        match dir_lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => bail!(
                "another Alephgrid server is using {}; stop it first (one server per data directory)",
                config.data_dir.display()
            ),
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
        // Leftovers of uploads/exports interrupted by a crash; never user data.
        for scratch in [UPLOADS, EXPORTS] {
            let dir = config.data_dir.join(scratch);
            if dir.exists() {
                std::fs::remove_dir_all(&dir).with_context(|| format!("cleaning {}", dir.display()))?;
            }
        }
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) as u64;
        let budget = HostBudget {
            memory_bytes: config.memory_budget.unwrap_or_else(|| crate::runtime::budget::host_memory() / 10 * 8),
            threads: config.threads.map(|t| t as u64).unwrap_or(cores).max(1),
            temp_disk_bytes: config.temp_budget.unwrap_or(64 << 30),
        };
        let policy = ComputePolicy {
            id: "standard".into(),
            min_memory_bytes: config.min_workbook_memory,
            max_memory_bytes: None,
            isolation: IsolationClass::SharedProcess,
            idle_timeout_ms: config.idle_timeout.as_millis() as u64,
            providers: vec![ProviderKind::Local],
            profiles: std::iter::once(crate::runtime::ComputeProfile::standard()).chain(config.profiles.iter().cloned()).collect(),
            queue_timeout_ms: config.start_queue_timeout.as_millis() as u64,
            python: config.python_runtime.as_ref().map(|_| config.python_limits),
        };
        let (python, python_problem) = match &config.python_runtime {
            None => (None, None),
            Some(dir) => {
                if config.python_isolation == Some(crate::python::Isolation::None) && config.auth.mode != crate::auth::AuthMode::Local {
                    bail!("--python-isolation none runs Python without a sandbox and is allowed only with --auth local");
                }
                let scratch = config.data_dir.join(".python");
                let _ = std::fs::remove_dir_all(&scratch);
                match crate::python::PythonRuntime::load(dir, config.python_isolation.clone(), config.python_limits, &scratch) {
                    Ok(rt) => {
                        tracing::info!(runtime = rt.manifest.id(), python = rt.manifest.python, isolation = rt.isolation.name(), "Python functions run in isolated workers");
                        (Some(Arc::new(rt)), None)
                    }
                    Err(e) => {
                        tracing::warn!("Python functions cannot run: {e:#}");
                        (None, Some(format!("Python functions cannot run on this server: {e:#}")))
                    }
                }
            }
        };
        let python_id = python.as_ref().map(|p| p.manifest.id());
        let mut controller = Controller::with_spec(&config.data_dir, budget, policy, config.stage_hook.clone(), crate::runtime::RuntimeSpec::current().with_python(python_id.as_deref()))?;
        controller.function_runtime = python.clone().map(|p| p as Arc<dyn alephgrid_core::external::FunctionRuntime>);
        controller.drain_grace = config.shutdown_grace;
        let metrics = controller.metrics.clone();
        // Before reconciling, which writes records: on a full disk the reserve is given back.
        controller.disk_reserve = crate::reserve::DiskReserve::new(&control, config.disk_reserve);
        let fixed = controller.reconcile()?;
        if config.disk_reserve > 0 && !controller.disk_reserve.ensure() {
            tracing::warn!(
                "too little free disk to keep the {} MiB reserve for a full disk; it is created once there is room",
                config.disk_reserve >> 20
            );
        }
        if !fixed.is_empty() {
            tracing::info!("marked {} allocation(s) from a previous run as lost", fixed.len());
        }
        let store = crate::auth::Store::open(&control.join("control.duckdb"))?;
        migrate_to_spaces(&config.data_dir, &store)?;
        let auth = crate::auth::Auth::new(config.auth.clone(), store)?;
        Ok(Arc::new(AppState {
            config,
            uploads: open_uploads(),
            controller,
            auth,
            spaces: Mutex::new(()),
            quota: crate::quota::Counters::default(),
            next_session: AtomicU64::new(1),
            _dir_lock: dir_lock,
            shutting_down: AtomicBool::new(false),
            backup_running: Mutex::new(()),
            last_backup: Mutex::new(None),
            sessions: Mutex::new(HashMap::new()),
            downloads: Arc::new(Downloads::default()),
            python,
            python_problem,
            metrics,
            started_at_ms: alephgrid_core::storage::now_ms(),
        }))
    }

    pub fn next_session_id(&self) -> u64 {
        self.next_session.fetch_add(1, Ordering::Relaxed)
    }

    pub fn workbook_dir(&self, id: &str) -> PathBuf {
        self.config.data_dir.join(id)
    }

    fn trash_dir(&self) -> PathBuf {
        self.config.data_dir.join(TRASH)
    }

    /// Register a staged upload; also drops uploads nobody claimed within [`UPLOAD_TTL`].
    pub fn add_upload(&self, upload: Upload) -> String {
        let id = new_id();
        let mut uploads = self.uploads.lock().unwrap();
        uploads.retain(|_, u| {
            let keep = u.touched.elapsed() < UPLOAD_TTL;
            if !keep {
                let _ = std::fs::remove_dir_all(&u.dir);
                self.controller.release_upload(u.size_bytes);
            }
            keep
        });
        uploads.insert(id.clone(), upload);
        id
    }

    /// A staged upload of `workbook`, marked as used now.
    /// Refuse adding data to a workbook at its size limit (SEC.11).
    pub fn check_workbook_size(&self, id: &str) -> Result<()> {
        let Some(limit) = self.config.max_workbook_bytes else { return Ok(()) };
        let size = database_bytes(&self.workbook_dir(id));
        if size >= limit {
            bail!(alephgrid_core::limits::TooLarge(format!(
                "this workbook holds {} MiB, its limit is {} MiB; delete tables or rows first, or ask an admin to raise the limit",
                size >> 20,
                limit >> 20
            )));
        }
        Ok(())
    }

    pub fn upload(&self, workbook: &str, id: &str) -> Result<Upload> {
        let mut uploads = self.uploads.lock().unwrap();
        match uploads.get_mut(id) {
            Some(u) if u.workbook == workbook && u.path.exists() => {
                u.touched = std::time::Instant::now();
                Ok(u.clone())
            }
            _ => bail!("this upload has expired or was already imported; choose the file again"),
        }
    }

    pub fn remove_upload(&self, id: &str) {
        if let Some(u) = self.uploads.lock().unwrap().remove(id) {
            let _ = std::fs::remove_dir_all(u.dir);
            self.controller.release_upload(u.size_bytes);
        }
    }

    /// A fresh private directory for one request under `.uploads/<workbook>` or
    /// `.exports/<workbook>`: the only places that workbook's database may read or write
    /// files (see [`allowed_dirs`]).
    pub fn scratch_dir(&self, kind: &str, workbook: &str) -> Result<PathBuf> {
        let dir = self.workbook_scratch(kind, workbook).join(new_id());
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    pub fn workbook_scratch(&self, kind: &str, workbook: &str) -> PathBuf {
        self.config.data_dir.join(kind).join(workbook)
    }


    // ------------------------------------------------------------- workbooks

    pub fn list(&self) -> Result<Vec<WorkbookListing>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.config.data_dir)? {
            let dir = entry?.path();
            if is_hidden(&dir) {
                continue;
            }
            let Ok(mut meta) = read_meta(&dir) else { continue };
            if let Some(h) = self.controller.handle(&meta.id) {
                meta = h.meta.lock().unwrap().clone();
            }
            let compute = self.controller.compute_state(&meta.id);
            out.push(WorkbookListing { size_bytes: dir_size(&dir), meta, role: None, compute });
        }
        out.sort_by_key(|w| std::cmp::Reverse(w.meta.opened_at.max(w.meta.created_at)));
        Ok(out)
    }

    /// Create a workbook in `space` (the caller checked the principal may).
    pub fn create(&self, name: &str, space: &str) -> Result<Lease> {
        if self.auth.store.space(space)?.is_none() {
            bail!(crate::auth::not_found("space not found"));
        }
        self.check_new_workbook(space, 0)?;
        // A new workbook opens at once: refuse before making it rather than leave it closed.
        let limits = self.limits(space)?;
        self.controller.check_admission(&Admission {
            space: space.to_string(),
            open_workbooks: limits.open_workbooks,
            compute_memory_bytes: limits.compute_memory_bytes,
            profile: crate::runtime::ComputeProfile::standard(),
            note: None,
            python_off: None,
        })?;
        let id = new_id();
        let dir = self.workbook_dir(&id);
        std::fs::create_dir_all(&dir)?;
        let t = now();
        let meta = WorkbookMeta {
            id: id.clone(),
            name: name.trim().to_string(),
            created_at: t,
            space: space.to_string(),
            legacy_workspace: None,
            starred: false,
            opened_at: t,
            updated_at: t,
            summary: Summary::default(),
            deleted_at: None,
            runtime_spec: None,
            profile: None,
        };
        write_meta(&dir, &meta)?;
        self.get(&id)
    }

    /// The live handle of a workbook, starting an allocation for it if needed.
    pub fn get(&self, id: &str) -> Result<Lease> {
        self.get_with(id, &operation_id("open"))
    }

    /// [`AppState::get`] under a caller-chosen operation id.
    pub fn get_with(&self, id: &str, operation: &str) -> Result<Lease> {
        self.check_workbook(id)?;
        let admit = || self.admission(id, None).map_err(|e| RuntimeFailure::new(FailureKind::StartupFailed, format!("{e:#}")));
        Ok(self.controller.ensure(id, operation, &admit)?)
    }

    fn check_workbook(&self, id: &str) -> Result<()> {
        if !valid_id(id) {
            bail!("invalid workbook id");
        }
        if self.shutting_down.load(Ordering::SeqCst) {
            bail!("the server is shutting down");
        }
        if !self.workbook_dir(id).join(META_FILE).exists() {
            bail!("workbook {id} not found");
        }
        Ok(())
    }

    /// What a start of `id` is admitted under (7.13): its space's compute quotas and its
    /// profile (`profile`, or the one it keeps), if the server still defines it and the
    /// space is still granted it; otherwise `standard`, with a note saying why.
    pub fn admission(&self, id: &str, profile: Option<&str>) -> Result<Admission> {
        let meta = read_meta(&self.workbook_dir(id))?;
        let limits = self.limits(&meta.space)?;
        let wanted = profile.map(str::to_string).or(meta.profile).unwrap_or_else(|| STANDARD.to_string());
        let (profile, note) = self.entitled_profile(&meta.space, &wanted)?;
        let python_off = self.python_off(&meta.space)?;
        Ok(Admission { space: meta.space, open_workbooks: limits.open_workbooks, compute_memory_bytes: limits.compute_memory_bytes, profile, note, python_off })
    }

    /// Why Python functions may not run for workbooks of `space`, if so.
    pub fn python_off(&self, space: &str) -> Result<Option<String>> {
        if let Some(problem) = &self.python_problem {
            return Ok(Some(problem.clone()));
        }
        Ok(match self.auth.store.space(space)? {
            Some(s) if !s.python => Some("Python functions are turned off for this space".to_string()),
            _ => None,
        })
    }

    /// A space's Python setting changed: apply it to its open workbooks at once (running
    /// work stops and its results are not stored, 8.10).
    pub fn python_policy_changed(&self, space: &str) {
        let off = self.python_off(space).unwrap_or_else(|e| Some(format!("{e:#}")));
        for id in self.controller.live() {
            if self.workbook_space(&id).ok().as_deref() != Some(space) {
                continue;
            }
            if let Some(h) = self.controller.handle(&id) {
                if off.is_some() {
                    h.cancel_functions();
                }
                let runtime = self.controller.function_runtime.clone();
                let off = off.clone();
                if let Err(e) = h.with_engine(move |wb| wb.set_function_runtime(runtime, off)) {
                    tracing::warn!(workbook = id.as_str(), "applying the Python setting: {e:#}");
                }
                let _ = h.events.send(crate::ws::Event::SheetChanged { sheet: None, origin: 0, revision: 0 });
                h.compute_functions_later();
            }
        }
    }

    fn entitled_profile(&self, space: &str, wanted: &str) -> Result<(ComputeProfile, Option<String>)> {
        if wanted == STANDARD {
            return Ok((ComputeProfile::standard(), None));
        }
        Ok(match self.controller.policy.profile(wanted) {
            None => (ComputeProfile::standard(), Some(format!("the {wanted} profile is no longer offered by this server; it runs with standard"))),
            Some(_) if !self.auth.store.space_profiles(space)?.iter().any(|p| p == wanted) => {
                (ComputeProfile::standard(), Some(format!("the {wanted} profile is no longer granted to this space; it runs with standard")))
            }
            Some(p) => (p.clone(), None),
        })
    }

    /// Run `id` with `profile` from now on (7.15): a controlled restart if it is open, else
    /// recorded and opened. The caller checked the principal may choose it.
    pub fn restart(&self, id: &str, operation: &str, profile: Option<&str>) -> Result<Lease> {
        self.check_workbook(id)?;
        let previous = self.admission(id, None)?;
        let wanted = profile.map(str::to_string).unwrap_or_else(|| previous.profile.id.clone());
        let adm = self.admission(id, Some(&wanted))?;
        if adm.profile.id != wanted {
            return Err(crate::auth::refused(adm.note.clone().unwrap_or_else(|| format!("the {wanted} profile is not available"))));
        }
        Ok(self.controller.restart(id, operation, adm, previous)?)
    }

    /// Change metadata without opening the workbook's database.
    fn edit_meta(&self, id: &str, f: impl FnOnce(&mut WorkbookMeta) -> Result<()>) -> Result<WorkbookMeta> {
        if !valid_id(id) {
            bail!("invalid workbook id");
        }
        if let Some(h) = self.controller.handle(id) {
            let mut meta = h.meta.lock().unwrap();
            f(&mut meta)?;
            write_meta(&h.dir, &meta)?;
            return Ok(meta.clone());
        }
        let dir = self.workbook_dir(id);
        let mut meta = read_meta(&dir)?;
        f(&mut meta)?;
        write_meta(&dir, &meta)?;
        Ok(meta)
    }

    /// Apply a partial update (name, space, starred); the caller checked the principal may.
    pub fn update(&self, id: &str, patch: WorkbookPatch) -> Result<WorkbookMeta> {
        let _guard = self.spaces.lock().unwrap();
        if let Some(space) = &patch.space {
            if self.auth.store.space(space)?.is_none() {
                bail!(crate::auth::not_found("space not found"));
            }
            if self.workbook_space(id)? != *space {
                self.check_new_workbook(space, dir_size(&self.workbook_dir(id)))?;
            }
        }
        let moved = patch.space.is_some();
        let out = self.edit_meta(id, |meta| {
            if let Some(name) = patch.name {
                let name = name.trim();
                if name.is_empty() {
                    bail!("name cannot be empty");
                }
                meta.name = name.to_string();
            }
            if let Some(space) = patch.space {
                meta.space = space;
            }
            if let Some(starred) = patch.starred {
                meta.starred = starred;
            }
            Ok(())
        });
        if moved {
            // Connections to it now answer to the roles of its new space, and its compute
            // counts against that space's quotas.
            if let Ok(meta) = &out {
                self.controller.moved(id, &meta.space);
            }
            self.auth.bump();
        }
        out
    }

    /// Copy workbook `id`'s database (open as `handle`) to `target` from one snapshot and
    /// return its revision. The database may only write its own scratch directories (4.07):
    /// copy there, then move the file to `target`.
    pub fn snapshot_to(&self, handle: &WorkbookHandle, id: &str, target: &Path) -> Result<u64> {
        let scratch = self.scratch_dir(EXPORTS, id)?;
        let staged = scratch.join(DB_FILE);
        let copied = handle.with_reader(|r| r.backup_to(&staged)).and_then(|rev| {
            std::fs::rename(&staged, target)?;
            Ok(rev)
        });
        let _ = std::fs::remove_dir_all(&scratch);
        copied
    }

    /// Copy a workbook (tables, formulas, sheets and history) into a new one, from one
    /// snapshot while the source keeps working.
    pub fn duplicate(&self, id: &str) -> Result<WorkbookMeta> {
        let src = self.get(id)?;
        let src_meta = src.meta.lock().unwrap().clone();
        self.check_new_workbook(&src_meta.space, dir_size(&self.workbook_dir(id)))?;
        let new = new_id();
        let dir = self.workbook_dir(&new);
        std::fs::create_dir_all(&dir)?;
        let copied = (|| -> Result<()> {
            let target = dir.join(DB_FILE);
            self.snapshot_to(&src, id, &target)?;
            // The copy is a new workbook: bind it to its own id.
            Workbook::open_locked(&target, &alephgrid_core::storage::Lockdown::default())?.rebind_identity(&new)?;
            Ok(())
        })();
        if let Err(e) = copied {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
        let t = now();
        let meta = WorkbookMeta {
            id: new.clone(),
            name: format!("Copy of {}", src_meta.name),
            created_at: t,
            opened_at: t,
            updated_at: t,
            starred: false,
            deleted_at: None,
            ..src_meta
        };
        write_meta(&dir, &meta)?;
        Ok(meta)
    }

    /// Move a workbook to the trash. Connected clients receive `workbook_deleted`.
    pub fn delete(&self, id: &str) -> Result<()> {
        if !valid_id(id) {
            bail!("invalid workbook id");
        }
        let dir = self.workbook_dir(id);
        if !dir.join(META_FILE).exists() {
            bail!("workbook {id} not found");
        }
        if !self.controller.block(id) {
            bail!("workbook {id} is already being deleted");
        }
        let result = self
            .controller
            .stop(id, &operation_id("delete"), StopReason::Deleted)
            .and_then(|_| self.move_to_trash(id, &dir));
        self.controller.unblock(id);
        result
    }

    fn move_to_trash(&self, id: &str, dir: &Path) -> Result<()> {
        let trash = self.trash_dir();
        std::fs::create_dir_all(&trash)?;
        let target = trash.join(id);
        if target.exists() {
            std::fs::remove_dir_all(&target)?;
        }
        let mut meta = read_meta(dir)?;
        meta.deleted_at = Some(now());
        write_meta(dir, &meta)?;
        std::fs::rename(dir, &target).with_context(|| format!("moving workbook {id} to the trash"))?;
        Ok(())
    }

    pub fn trash(&self) -> Result<Vec<WorkbookListing>> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(self.trash_dir()) else { return Ok(out) };
        for entry in entries {
            let dir = entry?.path();
            let Ok(meta) = read_meta(&dir) else { continue };
            out.push(WorkbookListing { size_bytes: dir_size(&dir), meta, role: None, compute: crate::runtime::ComputeState::Sleeping });
        }
        out.sort_by_key(|w| std::cmp::Reverse(w.meta.deleted_at));
        Ok(out)
    }

    /// Move a workbook back out of the trash.
    pub fn restore(&self, id: &str) -> Result<WorkbookMeta> {
        if !valid_id(id) {
            bail!("invalid workbook id");
        }
        let from = self.trash_dir().join(id);
        let to = self.workbook_dir(id);
        if !from.join(META_FILE).exists() {
            bail!("workbook {id} is not in the trash");
        }
        if to.exists() {
            bail!("a workbook with id {id} already exists");
        }
        let mut meta = read_meta(&from)?;
        if self.auth.store.space(&meta.space)?.is_none() {
            bail!("the space of workbook {id} no longer exists");
        }
        let limit = self.limits(&meta.space)?.workbooks;
        let count = self.list()?.into_iter().filter(|w| w.meta.space == meta.space).count() as u64;
        crate::quota::check_workbooks(count, limit)?;
        meta.deleted_at = None;
        write_meta(&from, &meta)?;
        std::fs::rename(&from, &to)?;
        Ok(meta)
    }

    /// Permanently remove one trashed workbook, or the whole trash with `None`.
    pub fn purge(&self, id: Option<&str>) -> Result<()> {
        match id {
            Some(id) => {
                if !valid_id(id) {
                    bail!("invalid workbook id");
                }
                let dir = self.trash_dir().join(id);
                if !dir.exists() {
                    bail!("workbook {id} is not in the trash");
                }
                std::fs::remove_dir_all(dir)?;
            }
            None => {
                if self.trash_dir().exists() {
                    std::fs::remove_dir_all(self.trash_dir())?;
                }
            }
        }
        Ok(())
    }

    /// Refuse new requests from now on (shutdown has begun).
    pub fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
    }

    /// Stop every allocation: drain, checkpoint, close, release locks (shutdown).
    pub fn checkpoint_all(&self) {
        self.begin_shutdown();
        self.controller.shutdown();
        self.auth.close_audit();
    }

    // ---------------------------------------------------------------- spaces

    /// The space a workbook (live, closed or in the trash) belongs to.
    pub fn workbook_space(&self, id: &str) -> Result<String> {
        if !valid_id(id) {
            bail!("invalid workbook id");
        }
        if let Some(h) = self.controller.handle(id) {
            return Ok(h.meta.lock().unwrap().space.clone());
        }
        let dir = self.workbook_dir(id);
        let meta = if dir.join(META_FILE).exists() { read_meta(&dir) } else { read_meta(&self.trash_dir().join(id)) };
        meta.map(|m| m.space).map_err(|_| anyhow!("workbook {id} not found"))
    }

    /// The space of a workbook, if `p` may do `cap` there. A workbook in a space `p` cannot
    /// see is "not found", like one that does not exist.
    pub fn require_workbook(&self, p: &crate::auth::Principal, id: &str, cap: crate::auth::Cap) -> Result<(crate::auth::Space, crate::auth::SpaceRole)> {
        let missing = || crate::auth::not_found(format!("workbook {id} not found"));
        let space = self.workbook_space(id).map_err(|_| missing())?;
        match self.auth.require_space(p, &space, cap) {
            Err(e) if matches!(e.downcast_ref::<crate::auth::Denied>(), Some(crate::auth::Denied::NotFound(_))) => Err(missing()),
            other => other,
        }
    }

    /// Create a space in `org`; `owner` (a user id) becomes its owner.
    pub fn create_space(&self, org: &str, name: &str, owner: Option<&str>) -> Result<crate::auth::Space> {
        let name = name.trim();
        if name.is_empty() {
            bail!("name cannot be empty");
        }
        if self.auth.store.org(org)?.is_none() {
            bail!(crate::auth::not_found("organization not found"));
        }
        let space = self.auth.store.txn(|c| {
            let s = crate::auth::Store::create_space(c, &crate::auth::random_id(), org, name)?;
            if let Some(u) = owner {
                crate::auth::Store::set_space_member(c, &s.id, crate::auth::MemberKind::User, u, crate::auth::SpaceRole::Owner)?;
            }
            Ok(s)
        })?;
        self.auth.bump();
        Ok(space)
    }

    /// Delete an empty space. Refused while it holds workbooks, in the trash included.
    pub fn delete_space(&self, id: &str) -> Result<()> {
        let _guard = self.spaces.lock().unwrap();
        let live = self.list()?.into_iter().filter(|w| w.meta.space == id).count();
        let trashed = self.trash()?.into_iter().filter(|w| w.meta.space == id).count();
        if live + trashed > 0 {
            return Err(crate::auth::bad_request(format!(
                "this space still holds {live} workbook(s) and {trashed} in the trash; move or delete them first"
            )));
        }
        self.auth.store.delete_space(id)?;
        self.auth.bump();
        Ok(())
    }
}

/// Make every workbook belong to a space (7.01). Run at every start; does nothing once done.
///
/// * The first start creates the default organization and its `General` space.
/// * Each workspace of `workspaces.json` becomes a space of the default organization with
///   the same id, so workbooks keep their grouping.
/// * A workbook (live or trashed) without a known space joins its former workspace's space,
///   or `General`.
/// * `workspaces.json` is then renamed into `.control/`, so the migration is not repeated
///   and the original stays for inspection.
///
/// Every step can be repeated, so a crash midway is finished by the next start.
pub fn migrate_to_spaces(data_dir: &Path, store: &crate::auth::Store) -> Result<()> {
    use crate::auth::Store;
    let org = match store.setting("default_org")? {
        Some(o) => o,
        None => store.txn(|c| {
            let o = Store::create_org(c, &crate::auth::random_id(), "Default organization")?;
            Store::set_setting(c, "default_org", &o.id)?;
            Ok(o.id)
        })?,
    };
    let legacy_path = data_dir.join("workspaces.json");
    // A list of workspace names: anything large is not one (SEC.14).
    if std::fs::metadata(&legacy_path).is_ok_and(|m| m.len() > 1 << 20) {
        bail!("workspaces.json is larger than 1 MiB; it is not a workspace list");
    }
    let legacy: Vec<LegacyWorkspace> = match std::fs::read(&legacy_path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("reading workspaces.json")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    let unique = |c: &duckdb::Connection, base: &str| -> Result<String> {
        for n in 1.. {
            let name = if n == 1 { base.to_string() } else { format!("{base} ({n})") };
            let taken: i64 = c.query_row("SELECT count(*) FROM spaces WHERE org_id = ? AND lower(name) = lower(?)", duckdb::params![org, name], |r| r.get(0))?;
            if taken == 0 {
                return Ok(name);
            }
        }
        unreachable!()
    };
    store.txn(|c| {
        for w in &legacy {
            if valid_id(&w.id) && Store::space_in(c, &w.id)?.is_none() {
                let name = unique(c, if w.name.trim().is_empty() { "Workspace" } else { w.name.trim() })?;
                Store::create_space(c, &w.id, &org, &name)?;
            }
        }
        Ok(())
    })?;
    let general = match store.setting("default_space")?.filter(|s| store.space(s).ok().flatten().is_some()) {
        Some(s) => s,
        None => store.txn(|c| {
            let name = unique(c, "General")?;
            let s = Store::create_space(c, &crate::auth::random_id(), &org, &name)?;
            Store::set_setting(c, "default_space", &s.id)?;
            Ok(s.id)
        })?,
    };
    let mut moved = 0;
    for parent in [data_dir.to_path_buf(), data_dir.join(TRASH)] {
        let Ok(entries) = std::fs::read_dir(&parent) else { continue };
        for e in entries {
            let dir = e?.path();
            if (is_hidden(&dir) && parent == data_dir) || !dir.join(META_FILE).exists() {
                continue;
            }
            // A workbook whose workbook.json does not match its directory stays unplaced (and
            // unopenable) rather than stopping the server (SEC.02).
            let Ok(mut meta) = read_meta(&dir) else {
                tracing::warn!("skipping {}: its workbook.json does not name it", dir.display());
                continue;
            };
            if !meta.space.is_empty() && store.space(&meta.space)?.is_some() {
                continue;
            }
            let from = meta.legacy_workspace.take().filter(|w| store.space(w).ok().flatten().is_some());
            meta.space = from.unwrap_or_else(|| general.clone());
            write_meta(&dir, &meta)?;
            moved += 1;
        }
    }
    if legacy_path.exists() {
        std::fs::rename(&legacy_path, data_dir.join(".control").join("workspaces.json.migrated"))?;
        alephgrid_core::storage::sync_dir(data_dir);
        tracing::info!("migrated {} workspace(s) to spaces", legacy.len());
    }
    if moved > 0 {
        tracing::info!("placed {moved} workbook(s) into spaces");
    }
    Ok(())
}

/// Background work: close idle workbooks, drop unused view materializations. Runs until
/// the returned task is aborted.
pub fn start_janitor(state: &Shared) -> tokio::task::JoinHandle<()> {
    let state = state.clone();
    let tick = (state.config.idle_timeout.min(state.config.view_ttl) / 4).clamp(Duration::from_millis(50), Duration::from_secs(30));
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tick).await;
            if state.shutting_down.load(Ordering::SeqCst) {
                break;
            }
            let s = state.clone();
            let ttl = s.config.view_ttl.as_millis() as u64;
            let _ = tokio::task::spawn_blocking(move || {
                let evicted = s.controller.evict_idle();
                if !evicted.is_empty() {
                    tracing::info!("closed {} idle workbook(s)", evicted.len());
                }
                s.controller.expire_views(ttl);
                s.controller.disk_reserve.ensure();
                s.controller.refresh_sheet_estimates();
                s.downloads.expire(DOWNLOAD_TTL);
            })
            .await;
        }
    })
}

fn open_uploads() -> Mutex<HashMap<String, Upload>> {
    Mutex::new(HashMap::new())
}

#[derive(Debug, Default, Deserialize, TS)]
#[ts(optional_fields)]
pub struct WorkbookPatch {
    pub name: Option<String>,
    /// Move the workbook into this space.
    pub space: Option<String>,
    pub starred: Option<bool>,
}

pub fn read_meta(dir: &Path) -> Result<WorkbookMeta> {
    let id = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let bytes = std::fs::read(dir.join(META_FILE)).with_context(|| format!("workbook {id} not found"))?;
    let meta: WorkbookMeta = serde_json::from_slice(&bytes)?;
    // The id is the workbook's filesystem key: it must be the directory's own name (SEC.02).
    if !valid_id(&meta.id) || meta.id != id {
        bail!("workbook {id}'s workbook.json names a different workbook ({:?})", meta.id);
    }
    Ok(meta)
}

pub fn write_meta(dir: &Path, meta: &WorkbookMeta) -> Result<()> {
    crate::runtime::write_durably(&dir.join(META_FILE), &serde_json::to_vec_pretty(meta)?)
}
