//! Workbook and workspace registry.
//!
//! ```text
//! <data-dir>/workspaces.json            [{id, name, created_at}]
//! <data-dir>/<workbook-id>/workbook.json
//! <data-dir>/<workbook-id>/data.duckdb  tables, formula metadata, IronCalc sheets, revision history
//! <data-dir>/<workbook-id>/.owner.lock  flock held by the live allocation (see runtime/local.rs)
//! <data-dir>/<workbook-id>/owner.json   who holds it, under which epoch
//! <data-dir>/.control/                  runtime specs, allocation records, controller lock
//! <data-dir>/.trash/<workbook-id>/      deleted workbooks (same layout + deleted_at), restorable
//! <data-dir>/.uploads/<random>/<file>   uploads in progress; removed after import or failure
//! <data-dir>/.exports/<random>/<file>   exports being streamed; removed once opened
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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use rowfinity_engine::{Reader, Workbook};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use ts_rs::TS;

use crate::runtime::budget::HostBudget;
use crate::sched::{Class, JobError, Key, Lane, Scheduler};
use crate::runtime::controller::{operation_id, Controller, StageHook, StopReason};
use crate::runtime::{ComputePolicy, IsolationClass, OwnershipAssignment, ProviderKind, Reservation};
use crate::ws::Event;

/// DuckDB handle types, re-exported by the engine's dependency.
pub mod duckdb_handles {
    pub use rowfinity_engine::duckdb_handles::{InterruptHandle, ProgressHandle};
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
    #[serde(default)]
    pub workspace: Option<String>,
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
}

/// What the workbook list shows: metadata plus size on disk.
#[derive(Debug, Clone, Serialize, TS)]
pub struct WorkbookListing {
    #[serde(flatten)]
    pub meta: WorkbookMeta,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub created_at: u64,
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
    reader_interrupts: Vec<Arc<duckdb_handles::InterruptHandle>>,
    reader_progress: Vec<duckdb_handles::ProgressHandle>,
    /// Orders and bounds the work on this workbook (see `sched.rs`).
    pub sched: Arc<Scheduler>,
    /// Client-chosen id of the long job running now (an import), for progress and cancel.
    pub job: Mutex<Option<String>>,
    engine: Mutex<Option<Workbook>>,
    readers: Vec<Mutex<Option<Reader>>>,
    control: Mutex<Option<Reader>>,
    closed: Mutex<Option<Closed>>,
    /// Operations running now (writer or readers).
    active: AtomicUsize,
    sessions: AtomicU64,
    last_activity_ms: AtomicU64,
    pub events: broadcast::Sender<Event>,
    pub dir: PathBuf,
    /// The ownership lock; dropping the file releases it.
    lock: Mutex<Option<File>>,
    pub temp_dir: PathBuf,
    pub ownership: OwnershipAssignment,
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
        let control = engine.reader()?;
        let (events, _) = broadcast::channel(256);
        let reader_interrupts: Vec<_> = readers.iter().map(|r| r.interrupt_handle()).collect();
        Ok(WorkbookHandle {
            id: meta.id.clone(),
            progress: engine.progress_handle(),
            interrupt: engine.interrupt_handle(),
            sched: Scheduler::new(engine.interrupt_handle(), reader_interrupts.clone(), crate::sched::Limits::default()),
            reader_interrupts,
            reader_progress: readers.iter().map(|r| r.progress_handle()).collect(),
            job: Mutex::new(None),
            meta: Mutex::new(meta),
            engine: Mutex::new(Some(engine)),
            readers: readers.into_iter().map(|r| Mutex::new(Some(r))).collect(),
            control: Mutex::new(Some(control)),
            closed: Mutex::new(None),
            active: AtomicUsize::new(0),
            sessions: AtomicU64::new(0),
            last_activity_ms: AtomicU64::new(rowfinity_engine::storage::now_ms()),
            events,
            dir,
            lock: Mutex::new(Some(lock)),
            temp_dir,
            ownership,
        })
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
    pub fn progress_of(&self, lane: Lane, slot: usize) -> rowfinity_engine::duckdb_handles::QueryProgress {
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
        Ok(self.sched.submit(job.key, lane, job.class)?)
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
        let slot = tokio::select! {
            slot = ticket.granted() => slot,
            _ = tokio::time::sleep_until(job.deadline.into()) => bail!(JobError::DeadlineExceeded),
            _ = cancelled(&mut cancel) => bail!(JobError::Cancelled),
        };
        let lane = work.lane();
        let h = self.clone();
        let mut task = tokio::task::spawn_blocking(move || {
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
                _ = tick.tick(), if progress.is_some() => {
                    if let Some(p) = self.progress_of(lane, slot).percentage() {
                        (progress.as_ref().unwrap())(p);
                    }
                }
            }
        };
        let result = result.map_err(|e| anyhow!("internal error: {e}"))?;
        match (result, stopped) {
            // Interrupted: report why. A job that finished anyway keeps its result.
            (Err(e), Some(why)) if format!("{e:#}").contains("INTERRUPT") => Err(anyhow::Error::new(why)),
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
        self.active.load(Ordering::SeqCst) == 0 && self.job.lock().unwrap().is_none() && self.sched.load() == 0
    }

    pub fn active(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    pub fn touch_activity(&self) {
        self.last_activity_ms.store(rowfinity_engine::storage::now_ms(), Ordering::Relaxed);
    }

    pub fn last_activity_ms(&self) -> u64 {
        self.last_activity_ms.load(Ordering::Relaxed)
    }

    pub fn session_started(&self) {
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
        self.with_control(|c| Ok(c.sheets().to_bytes().map(|b| b.len() as u64 * 8).unwrap_or(0))).unwrap_or(0)
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
    pub token: Option<String>,
    pub allow_sql: bool,
    /// Memory shared by every open workbook's DuckDB instance. Defaults to 80% of RAM (or
    /// of the container's limit).
    pub memory_budget: Option<u64>,
    /// DuckDB worker threads shared by every open workbook. Defaults to all cores.
    pub threads: Option<usize>,
    /// Temporary disk for spills and staged uploads. Defaults to 64 GiB.
    pub temp_budget: Option<u64>,
    /// Smallest memory share a workbook may run with.
    pub min_workbook_memory: u64,
    /// Close workbooks without sessions or work after this long.
    pub idle_timeout: Duration,
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
    /// Default and longest time an interactive request may take (waiting included).
    pub request_timeout: Duration,
    /// Default and longest time a bulk job (import, query, sort, copy) may take.
    pub job_timeout: Duration,
    /// A session's unsent replies and events beyond this pause reading its requests.
    pub outbox_pause_bytes: usize,
    /// A session whose unsent data passes this is closed (the client is not reading).
    pub outbox_max_bytes: usize,
}

impl Config {
    pub fn new(data_dir: impl Into<PathBuf>) -> Config {
        Config {
            data_dir: data_dir.into(),
            sheet_copy_rows: crate::ws::SHEET_COPY_ROWS,
            token: None,
            allow_sql: true,
            memory_budget: None,
            threads: None,
            temp_budget: None,
            min_workbook_memory: 256 << 20,
            idle_timeout: Duration::from_secs(600),
            view_ttl: Duration::from_secs(900),
            shutdown_grace: Duration::from_secs(30),
            stage_hook: None,
            max_message_bytes: 16 << 20,
            max_upload_bytes: 100 << 30,
            request_timeout: Duration::from_secs(30),
            job_timeout: Duration::from_secs(3600),
            outbox_pause_bytes: 16 << 20,
            outbox_max_bytes: 64 << 20,
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
}

/// Unclaimed uploads are removed after this long.
pub const UPLOAD_TTL: Duration = Duration::from_secs(3600);

pub struct AppState {
    pub config: Config,
    pub uploads: Mutex<HashMap<String, Upload>>,
    pub controller: Controller,
    workspaces: Mutex<()>,
    next_session: AtomicU64,
    /// Exclusive lock on the data directory: one server per data directory.
    _dir_lock: File,
    pub shutting_down: AtomicBool,
    /// Open WebSocket sessions (for `/api/admin/sessions`).
    pub sessions: Mutex<HashMap<u64, crate::ws::SessionInfo>>,
}

pub type Shared = Arc<AppState>;

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn new_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mixed = (nanos as u64)
        ^ (std::process::id() as u64).rotate_left(32)
        ^ COUNTER.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    format!("{:012x}", mixed & 0xffff_ffff_ffff)
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn dir_size(dir: &Path) -> u64 {
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

pub fn arm_fault(point: &'static str, times: u32) {
    FAULTS.lock().unwrap().push((point, None, times));
}

/// Arm `point` only for operations on paths containing `scope` (a data directory, say).
pub fn arm_fault_in(point: &'static str, scope: &Path, times: u32) {
    FAULTS.lock().unwrap().push((point, Some(scope.to_string_lossy().to_string()), times));
}

pub fn disarm_faults() {
    FAULTS.lock().unwrap().clear();
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
                "another Rowfinity server is using {}; stop it first (one server per data directory)",
                config.data_dir.display()
            ),
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
        // Leftovers of uploads/exports interrupted by a crash; never user data.
        for scratch in [".uploads", ".exports"] {
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
        };
        let mut controller = Controller::new(&config.data_dir, budget, policy, config.stage_hook.clone())?;
        controller.drain_grace = config.shutdown_grace;
        let fixed = controller.reconcile()?;
        if !fixed.is_empty() {
            tracing::info!("marked {} allocation(s) from a previous run as lost", fixed.len());
        }
        Ok(Arc::new(AppState {
            config,
            uploads: open_uploads(),
            controller,
            workspaces: Mutex::new(()),
            next_session: AtomicU64::new(1),
            _dir_lock: dir_lock,
            shutting_down: AtomicBool::new(false),
            sessions: Mutex::new(HashMap::new()),
        }))
    }

    pub fn next_session_id(&self) -> u64 {
        self.next_session.fetch_add(1, Ordering::Relaxed)
    }

    fn workbook_dir(&self, id: &str) -> PathBuf {
        self.config.data_dir.join(id)
    }

    fn trash_dir(&self) -> PathBuf {
        self.config.data_dir.join(".trash")
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

    /// A fresh private directory under `.uploads` or `.exports` for one request.
    pub fn scratch_dir(&self, kind: &str) -> Result<PathBuf> {
        let dir = self.config.data_dir.join(kind).join(new_id());
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    // ------------------------------------------------------------- workbooks

    pub fn list(&self) -> Result<Vec<WorkbookListing>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.config.data_dir)? {
            let dir = entry?.path();
            if dir.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
                continue;
            }
            let Ok(mut meta) = read_meta(&dir) else { continue };
            if let Some(h) = self.controller.handle(&meta.id) {
                meta = h.meta.lock().unwrap().clone();
            }
            out.push(WorkbookListing { size_bytes: dir_size(&dir), meta });
        }
        out.sort_by_key(|w| std::cmp::Reverse(w.meta.opened_at.max(w.meta.created_at)));
        Ok(out)
    }

    pub fn create(&self, name: &str, workspace: Option<String>) -> Result<Arc<WorkbookHandle>> {
        let workspace = self.check_workspace(workspace)?;
        let id = new_id();
        let dir = self.workbook_dir(&id);
        std::fs::create_dir_all(&dir)?;
        let t = now();
        let meta = WorkbookMeta {
            id: id.clone(),
            name: name.trim().to_string(),
            created_at: t,
            workspace,
            starred: false,
            opened_at: t,
            updated_at: t,
            summary: Summary::default(),
            deleted_at: None,
            runtime_spec: None,
        };
        write_meta(&dir, &meta)?;
        self.get(&id)
    }

    /// The live handle of a workbook, starting an allocation for it if needed.
    pub fn get(&self, id: &str) -> Result<Arc<WorkbookHandle>> {
        self.get_with(id, &operation_id("open"))
    }

    /// [`AppState::get`] under a caller-chosen operation id.
    pub fn get_with(&self, id: &str, operation: &str) -> Result<Arc<WorkbookHandle>> {
        if !valid_id(id) {
            bail!("invalid workbook id");
        }
        if self.shutting_down.load(Ordering::SeqCst) {
            bail!("the server is shutting down");
        }
        if !self.workbook_dir(id).join("workbook.json").exists() {
            bail!("workbook {id} not found");
        }
        Ok(self.controller.ensure(id, operation)?)
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

    /// Apply a partial update (name, workspace, starred).
    pub fn update(&self, id: &str, patch: WorkbookPatch) -> Result<WorkbookMeta> {
        let workspace = match patch.workspace {
            Some(ws) => Some(self.check_workspace(Some(ws).filter(|w| !w.is_empty()))?),
            None => None,
        };
        self.edit_meta(id, |meta| {
            if let Some(name) = patch.name {
                let name = name.trim();
                if name.is_empty() {
                    bail!("name cannot be empty");
                }
                meta.name = name.to_string();
            }
            if let Some(ws) = workspace {
                meta.workspace = ws;
            }
            if let Some(starred) = patch.starred {
                meta.starred = starred;
            }
            Ok(())
        })
    }

    /// Copy a workbook (tables, formulas, sheets and history) into a new one, from one
    /// snapshot while the source keeps working.
    pub fn duplicate(&self, id: &str) -> Result<WorkbookMeta> {
        let src = self.get(id)?;
        let src_meta = src.meta.lock().unwrap().clone();
        let new = new_id();
        let dir = self.workbook_dir(&new);
        std::fs::create_dir_all(&dir)?;
        let copied = (|| -> Result<()> {
            let target = dir.join("data.duckdb");
            src.with_reader(|r| r.backup_to(&target))?;
            // The copy is a new workbook: bind it to its own id.
            Workbook::open(&target)?.rebind_identity(&new)?;
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
        if !dir.join("workbook.json").exists() {
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
            out.push(WorkbookListing { size_bytes: dir_size(&dir), meta });
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
        if !from.join("workbook.json").exists() {
            bail!("workbook {id} is not in the trash");
        }
        if to.exists() {
            bail!("a workbook with id {id} already exists");
        }
        let mut meta = read_meta(&from)?;
        if let Some(ws) = &meta.workspace {
            if !self.workspaces()?.iter().any(|w| &w.id == ws) {
                meta.workspace = None;
            }
        }
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
    }

    // ------------------------------------------------------------ workspaces

    fn workspaces_path(&self) -> PathBuf {
        self.config.data_dir.join("workspaces.json")
    }

    pub fn workspaces(&self) -> Result<Vec<Workspace>> {
        match std::fs::read(self.workspaces_path()) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
            Err(e) => Err(e.into()),
        }
    }

    fn save_workspaces(&self, list: &[Workspace]) -> Result<()> {
        crate::runtime::write_durably(&self.workspaces_path(), &serde_json::to_vec_pretty(list)?)
    }

    fn check_workspace(&self, id: Option<String>) -> Result<Option<String>> {
        match id {
            Some(id) if !self.workspaces()?.iter().any(|w| w.id == id) => bail!("unknown workspace {id}"),
            other => Ok(other),
        }
    }

    pub fn create_workspace(&self, name: &str) -> Result<Workspace> {
        let _guard = self.workspaces.lock().unwrap();
        let name = name.trim();
        if name.is_empty() {
            bail!("name cannot be empty");
        }
        let mut list = self.workspaces()?;
        if list.iter().any(|w| w.name.eq_ignore_ascii_case(name)) {
            bail!("a workspace named {name:?} already exists");
        }
        let ws = Workspace { id: new_id(), name: name.to_string(), created_at: now() };
        list.push(ws.clone());
        self.save_workspaces(&list)?;
        Ok(ws)
    }

    pub fn rename_workspace(&self, id: &str, name: &str) -> Result<Workspace> {
        let _guard = self.workspaces.lock().unwrap();
        let mut list = self.workspaces()?;
        let ws = list.iter_mut().find(|w| w.id == id).ok_or_else(|| anyhow!("unknown workspace"))?;
        ws.name = name.trim().to_string();
        let out = ws.clone();
        self.save_workspaces(&list)?;
        Ok(out)
    }

    /// Remove a workspace. Its workbooks are kept and become unfiled.
    pub fn delete_workspace(&self, id: &str) -> Result<()> {
        let _guard = self.workspaces.lock().unwrap();
        let mut list = self.workspaces()?;
        list.retain(|w| w.id != id);
        self.save_workspaces(&list)?;
        drop(_guard);
        for w in self.list()? {
            if w.meta.workspace.as_deref() == Some(id) {
                self.edit_meta(&w.meta.id, |meta| {
                    meta.workspace = None;
                    Ok(())
                })?;
            }
        }
        Ok(())
    }
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
    /// Workspace id; an empty string moves the workbook out of any workspace.
    pub workspace: Option<String>,
    pub starred: Option<bool>,
}

pub fn read_meta(dir: &Path) -> Result<WorkbookMeta> {
    let id = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let bytes = std::fs::read(dir.join("workbook.json")).with_context(|| format!("workbook {id} not found"))?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn write_meta(dir: &Path, meta: &WorkbookMeta) -> Result<()> {
    crate::runtime::write_durably(&dir.join("workbook.json"), &serde_json::to_vec_pretty(meta)?)
}
