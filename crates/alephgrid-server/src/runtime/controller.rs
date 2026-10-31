//! Lifecycle controller (3.13–3.14, 7.13–7.16): decides when allocations start and stop,
//! keeps their records in `<data>/.control/allocations/<workbook>.json`, and reconciles
//! those records after a restart.
//!
//! Rules:
//! * One live allocation per workbook. Concurrent requests to open the same workbook wait
//!   for the one start in progress (single flight), so duplicates never allocate twice.
//! * A start is admitted under its space's quotas and reserved in the host ledger under one
//!   lock, with its record: concurrent starts never pass a space's or the host's budget.
//! * When the host is full, a start waits in a first-come queue for capacity (7.14).
//! * Every transition is persisted before it takes effect for callers.
//! * Operations carry ids. Repeating an operation id that already moved this workbook's
//!   allocation returns the current status without doing anything again.
//! * A failed or cancelled start releases its lock, reservation and scratch files before
//!   the failure is reported.
//! * Ownership is uncertain only if another process holds the lock; then the workbook is
//!   refused (`owned_elsewhere`) rather than opened twice. A controlled restart hands the
//!   lock from the old allocation to the new one.
//! * Idle means no meaningful activity (requests, running work) for the idle timeout; open
//!   connections do not count (7.16).

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::Serialize;
use ts_rs::TS;

use super::budget::{AllocationUsage, HostBudget, Ledger, ResourceUsage};
use super::local::LocalProvider;
use super::{
    AllocationState, ComputePolicy, ComputeProfile, EnsureRequest, FailureKind, OwnershipAssignment, Reservation,
    RuntimeAllocation, RuntimeFailure, RuntimeProvider, RuntimeSpec,
};
use crate::state::{read_meta, Lease, WorkbookHandle, WorkbookMeta};

/// Called at each start stage with the workbook id; an error fails or cancels the start.
pub type StageHook = Arc<dyn Fn(&str, AllocationState) -> std::result::Result<(), RuntimeFailure> + Send + Sync>;

/// What a start is admitted under: its space's compute quotas and its profile (7.13).
#[derive(Debug, Clone)]
pub struct Admission {
    /// Why Python functions may not run for it (phase 8), if so.
    pub python_off: Option<String>,
    pub space: String,
    /// Most live allocations of the space.
    pub open_workbooks: Option<u64>,
    /// Most memory the space's live allocations are guaranteed, summed.
    pub compute_memory_bytes: Option<u64>,
    pub profile: ComputeProfile,
    /// Why the workbook runs with another profile than its own, if it does.
    pub note: Option<String>,
}

struct Slot {
    record: RuntimeAllocation,
    handle: Option<Arc<WorkbookHandle>>,
    cancel: Arc<AtomicBool>,
    space: String,
    profile: ComputeProfile,
    note: Option<String>,
    /// Waiting for host capacity since this sequence number; `None` once reserved.
    queued: Option<u64>,
}

#[derive(Default)]
struct Slots {
    map: HashMap<String, Slot>,
    /// Workbooks that may not start (being deleted, server stopping).
    blocked: HashSet<String>,
    stopping: bool,
    /// Next queue ticket.
    tickets: u64,
}

impl Slots {
    /// Position (1-based) of `workbook` in the capacity queue.
    fn queue_position(&self, workbook: &str) -> Option<u64> {
        let mine = self.map.get(workbook)?.queued?;
        Some(1 + self.map.values().filter(|s| s.queued.is_some_and(|t| t < mine)).count() as u64)
    }
}

/// The slots lock, still held, and the ownership lock taken out of a closed allocation (a
/// controlled restart starts the replacement with both).
type Held<'a> = (MutexGuard<'a, Slots>, Option<File>);

/// What `GET /api/workbooks/{id}/runtime` reports.
#[derive(Debug, Clone, Serialize, TS)]
pub struct RuntimeStatus {
    pub workbook: String,
    pub spec: RuntimeSpec,
    pub spec_id: String,
    pub policy: ComputePolicy,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub allocation: Option<RuntimeAllocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ownership: Option<OwnershipAssignment>,
    pub sessions: u64,
    /// The profile the workbook asks for (`workbook.json`).
    pub profile: String,
    /// Why the live allocation runs with another profile, if it does.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
    /// Place in the capacity queue (1 is next) while waiting for the host to have room.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub queue_position: Option<u64>,
    /// Last meaningful activity of the live allocation (ms since the epoch).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_activity_ms: Option<u64>,
    /// Bytes of the workbook on disk: kept while its compute is stopped.
    pub storage_bytes: u64,
}

pub struct Controller {
    provider: LocalProvider,
    control: PathBuf,
    data: PathBuf,
    pub budget: HostBudget,
    pub policy: ComputePolicy,
    spec: RuntimeSpec,
    spec_id: String,
    ledger: Mutex<Ledger>,
    slots: Mutex<Slots>,
    changed: Condvar,
    hook: Option<StageHook>,
    /// Grace period for draining before an allocation is stopped.
    pub drain_grace: Duration,
    /// Where worker (Python) functions run (phase 8).
    pub function_runtime: Option<Arc<dyn alephgrid_core::external::FunctionRuntime>>,
}

fn new_id(prefix: &str) -> String {
    use std::sync::atomic::AtomicU64;
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{:x}{:04x}{n:x}", alephgrid_core::storage::now_ms(), std::process::id() & 0xffff)
}

pub fn operation_id(kind: &str) -> String {
    new_id(kind)
}

/// How long a workbook started for someone who has not connected yet is spared from being
/// closed to make room.
pub const START_GRACE_MS: u64 = 10_000;

fn quota(message: String) -> RuntimeFailure {
    RuntimeFailure::new(FailureKind::Quota, message)
}

impl Controller {
    pub fn new(data: &Path, budget: HostBudget, policy: ComputePolicy, hook: Option<StageHook>) -> Result<Controller> {
        let control = data.join(".control");
        std::fs::create_dir_all(control.join("allocations"))?;
        let spec = RuntimeSpec::current();
        let spec_id = super::store_spec(&control, &spec)?;
        Ok(Controller {
            provider: LocalProvider { temp_root: data.join(".tmp") },
            control,
            data: data.to_path_buf(),
            budget,
            policy,
            spec,
            spec_id,
            ledger: Mutex::new(Ledger::default()),
            slots: Mutex::new(Slots::default()),
            changed: Condvar::new(),
            hook,
            drain_grace: Duration::from_secs(30),
            function_runtime: None,
        })
    }

    fn slots(&self) -> MutexGuard<'_, Slots> {
        self.slots.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wait<'a>(&self, slots: MutexGuard<'a, Slots>, ms: u64) -> MutexGuard<'a, Slots> {
        self.changed.wait_timeout(slots, Duration::from_millis(ms)).unwrap_or_else(|e| e.into_inner()).0
    }

    fn record_path(&self, workbook: &str) -> PathBuf {
        self.control.join("allocations").join(format!("{workbook}.json"))
    }

    pub fn load_record(&self, workbook: &str) -> Option<RuntimeAllocation> {
        std::fs::read(self.record_path(workbook)).ok().and_then(|b| serde_json::from_slice(&b).ok())
    }

    fn persist(&self, record: &RuntimeAllocation) -> std::result::Result<(), RuntimeFailure> {
        super::write_durably(&self.record_path(&record.workbook), &serde_json::to_vec_pretty(record).unwrap())
            .map_err(|e| RuntimeFailure::new(FailureKind::StartupFailed, format!("could not record the allocation: {e:#}")))
    }

    /// After a restart, no allocation of the previous process is live. Mark their records
    /// lost and clean up what they left, unless another process still holds the workbook.
    pub fn reconcile(&self) -> Result<Vec<String>> {
        let mut fixed = Vec::new();
        let mut held = HashSet::new();
        for entry in std::fs::read_dir(self.control.join("allocations"))? {
            let path = entry?.path();
            if path.extension().is_none_or(|e| e != "json") {
                let _ = std::fs::remove_file(&path); // a temporary file from an interrupted write
                continue;
            }
            let Ok(mut record) = serde_json::from_slice::<RuntimeAllocation>(&std::fs::read(&path)?) else { continue };
            if !record.state.is_live() {
                continue;
            }
            let dir = self.data.join(&record.workbook);
            if self.provider.reconcile(&record, &dir)? {
                tracing::warn!(workbook = record.workbook, "workbook is held by another process; leaving its allocation record");
                held.insert(record.workbook.clone());
                continue;
            }
            let op = operation_id("reconcile");
            if record.state.can_become(AllocationState::Failed) {
                record.transition(AllocationState::Failed, &op)?;
            } else {
                record.transition(AllocationState::Stopped, &op)?;
            }
            record.failure = Some(RuntimeFailure::new(FailureKind::Lost, "the server restarted while this allocation was live"));
            self.persist(&record).map_err(|e| anyhow!(e))?;
            fixed.push(record.workbook.clone());
        }
        // Scratch directories of allocations that are gone (not of ones held elsewhere).
        if let Ok(entries) = std::fs::read_dir(&self.provider.temp_root) {
            for e in entries.flatten() {
                if !held.contains(&e.file_name().to_string_lossy().to_string()) {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        Ok(fixed)
    }

    /// The live handle of a workbook, starting an allocation if there is none. `admit` is
    /// asked for the start's admission only when one is needed.
    pub fn ensure(
        &self,
        workbook: &str,
        operation: &str,
        admit: &dyn Fn() -> std::result::Result<Admission, RuntimeFailure>,
    ) -> std::result::Result<Lease, RuntimeFailure> {
        let mut admission: Option<Admission> = None;
        loop {
            let mut slots = self.slots();
            if slots.stopping {
                return Err(RuntimeFailure::new(FailureKind::Capacity, "the server is shutting down"));
            }
            if slots.blocked.contains(workbook) {
                return Err(RuntimeFailure::new(FailureKind::StartupFailed, format!("workbook {workbook} is being deleted")));
            }
            match slots.map.get(workbook) {
                Some(s) if s.record.state == AllocationState::Ready => {
                    let h = s.handle.clone().expect("ready slots have a handle");
                    h.touch_activity();
                    return Ok(Lease::new(h));
                }
                Some(s) if s.record.state.is_live() => {
                    // Someone else is starting or stopping it: wait for the outcome.
                    drop(self.wait(slots, 200));
                    continue;
                }
                _ => {}
            }
            // Nobody is: this caller starts it, admitted outside the lock.
            let Some(adm) = admission.take() else {
                drop(slots);
                admission = Some(admit()?);
                continue;
            };
            slots.map.remove(workbook);
            return self.start(slots, workbook, operation, adm, None);
        }
    }

    /// Refuse `adm` if the space has no room for one more allocation (not counting
    /// `except`, which is being replaced).
    fn check_space(&self, slots: &Slots, adm: &Admission, except: Option<&str>) -> std::result::Result<(), RuntimeFailure> {
        let live: Vec<&Slot> = slots
            .map
            .iter()
            .filter(|(k, s)| Some(k.as_str()) != except && s.space == adm.space && s.record.state.is_live())
            .map(|(_, s)| s)
            .collect();
        if let Some(max) = adm.open_workbooks {
            if live.len() as u64 >= max {
                return Err(quota(format!(
                    "this space already has {} workbook(s) open, its limit; close one (it closes by itself when idle) or ask an admin to raise it",
                    live.len()
                )));
            }
        }
        if let Some(max) = adm.compute_memory_bytes {
            let min = self.policy.min_memory_bytes;
            let used: u64 = live.iter().map(|s| s.profile.guaranteed_memory(min)).sum();
            let want = adm.profile.guaranteed_memory(min);
            if used + want > max {
                return Err(quota(format!(
                    "this space's open workbooks are guaranteed {} MiB of its {} MiB compute budget; the {} profile needs {} MiB more. Close a workbook or ask an admin to raise the budget",
                    used >> 20,
                    max >> 20,
                    adm.profile.id,
                    want >> 20
                )));
            }
        }
        Ok(())
    }

    /// Admit and run a start. `slots` holds no live slot for `workbook`. With `handoff`,
    /// the new allocation takes over this ownership lock instead of acquiring one.
    fn start<'a>(
        &'a self,
        mut slots: MutexGuard<'a, Slots>,
        workbook: &str,
        operation: &str,
        adm: Admission,
        handoff: Option<File>,
    ) -> std::result::Result<Lease, RuntimeFailure> {
        let dir = self.data.join(workbook);
        let meta = read_meta(&dir).map_err(|e| RuntimeFailure::new(FailureKind::StartupFailed, format!("{e:#}")))?;
        let previous = self.load_record(workbook);
        let epoch = previous.as_ref().map(|r| r.epoch).unwrap_or(0) + 1;
        // The spec the workbook is pinned to (or this build's, the first time).
        let spec_id = meta.runtime_spec.clone().unwrap_or_else(|| self.spec_id.clone());
        let spec = if spec_id == self.spec_id { self.spec.clone() } else { super::load_spec(&self.control, &spec_id).map_err(|e| RuntimeFailure::new(FailureKind::Incompatible, format!("{e:#}")))? };
        let record = RuntimeAllocation {
            id: new_id("al"),
            workbook: workbook.to_string(),
            spec: spec_id,
            policy: self.policy.id.clone(),
            profile: adm.profile.id.clone(),
            provider: super::ProviderKind::Local,
            state: AllocationState::Requested,
            epoch,
            reservation: Reservation::default(),
            holder: LocalProvider::holder(),
            requested_at_ms: alephgrid_core::storage::now_ms(),
            ready_at_ms: None,
            stopped_at_ms: None,
            failure: None,
            transitions: vec![],
        };
        if let Some(why) = self.provider.capabilities().incompatibility(&spec, &self.policy) {
            return Err(self.fail_unstarted(record, operation, RuntimeFailure::new(FailureKind::Incompatible, why)));
        }
        if let Err(e) = self.check_space(&slots, &adm, None) {
            return Err(self.fail_unstarted(record, operation, e));
        }
        // From here the slot exists: concurrent opens wait for it, and it counts against its
        // space while it waits for capacity.
        let cancel = Arc::new(AtomicBool::new(false));
        self.persist(&record)?;
        let fixed = adm.profile.fixed();
        slots.map.insert(
            workbook.to_string(),
            Slot { record: record.clone(), handle: None, cancel: cancel.clone(), space: adm.space.clone(), profile: adm.profile.clone(), note: adm.note.clone(), queued: None },
        );
        self.changed.notify_all();
        // Reserve a share: evict idle workbooks if the budget is full, else wait in line.
        let deadline = std::time::Instant::now() + Duration::from_millis(self.policy.queue_timeout_ms);
        let plan = loop {
            if cancel.load(Ordering::SeqCst) {
                drop(slots);
                return Err(self.finish_failed(workbook, operation, RuntimeFailure::new(FailureKind::Cancelled, "the start was cancelled")));
            }
            let queued = slots.map.get(workbook).and_then(|s| s.queued);
            let first = match queued {
                None => !slots.map.values().any(|s| s.queued.is_some()),
                Some(mine) => !slots.map.values().any(|s| s.queued.is_some_and(|t| t < mine)),
            };
            // Only the first in line may take capacity that frees up.
            let plan = if first { self.ledger().plan(&self.budget, Some((workbook, fixed)), self.policy.min_memory_bytes) } else { Err(RuntimeFailure::new(FailureKind::Capacity, String::new())) };
            let err = match plan {
                Ok(p) => break p,
                Err(e) => e,
            };
            if first {
                if let Some(victim) = self.capacity_victim(&slots, workbook) {
                    drop(slots);
                    self.stop_if_evictable(&victim);
                    slots = self.slots();
                    continue;
                }
            }
            if std::time::Instant::now() >= deadline {
                drop(slots);
                let waited = self.policy.queue_timeout_ms / 1000;
                let failure = if queued.is_some() && waited > 0 && !err.message.is_empty() {
                    RuntimeFailure::new(FailureKind::Capacity, format!("{} (waited {waited} s in line)", err.message))
                } else if err.message.is_empty() {
                    RuntimeFailure::new(FailureKind::Capacity, "the server is at capacity; try again in a few minutes or close a workbook")
                } else {
                    err
                };
                return Err(self.finish_failed(workbook, operation, failure));
            }
            if queued.is_none() {
                let ticket = slots.tickets;
                slots.tickets += 1;
                if let Some(s) = slots.map.get_mut(workbook) {
                    s.queued = Some(ticket);
                }
                tracing::info!(workbook, "the server is at capacity; waiting in line");
            }
            slots = self.wait(slots, 100);
        };
        {
            let slot = slots.map.get_mut(workbook).expect("starting slot");
            slot.queued = None;
            slot.record.reservation = plan[workbook];
        }
        self.ledger().set_fixed(workbook, fixed);
        // Everyone's new share at once: shrink the others first, so the sum stays within the budget.
        self.apply_plan(&slots, &plan);
        let reservation = plan[workbook];
        self.changed.notify_all();
        drop(slots);

        let advance = |to: AllocationState| -> std::result::Result<(), RuntimeFailure> {
            if cancel.load(Ordering::SeqCst) {
                return Err(RuntimeFailure::new(FailureKind::Cancelled, "the start was cancelled"));
            }
            if let Some(hook) = &self.hook {
                hook(workbook, to)?;
            }
            if cancel.load(Ordering::SeqCst) {
                return Err(RuntimeFailure::new(FailureKind::Cancelled, "the start was cancelled"));
            }
            let mut slots = self.slots();
            let slot = slots.map.get_mut(workbook).expect("starting slot");
            slot.record.transition(to, operation).map_err(|e| RuntimeFailure::new(FailureKind::StartupFailed, e.to_string()))?;
            self.persist(&slot.record)?;
            self.changed.notify_all();
            Ok(())
        };
        let engine_memory = self.ledger().duckdb_memory(workbook, &reservation);
        let allocation = RuntimeAllocation { reservation, ..record };
        let req = EnsureRequest {
            workbook,
            workbook_dir: &dir,
            meta,
            allocation: &allocation,
            spec: &spec,
            policy: &self.policy,
            reservation,
            engine_memory_bytes: engine_memory,
            checkpoint: &advance,
            handoff: Mutex::new(handoff),
        };
        let started = self.provider.ensure(&req).and_then(|h| {
            // A cancel that arrived while the engine was opening still wins.
            if cancel.load(Ordering::SeqCst) {
                let _ = self.provider.stop(&h);
                return Err(RuntimeFailure::new(FailureKind::Cancelled, "the start was cancelled"));
            }
            Ok(h)
        });
        let mut slots = self.slots();
        let slot = slots.map.get_mut(workbook).expect("starting slot");
        match started {
            Ok(handle) => {
                let ready = slot.record.transition(AllocationState::Ready, operation).map_err(|e| RuntimeFailure::new(FailureKind::StartupFailed, e.to_string()));
                // Its share may have shrunk while it opened (others started): apply the current one.
                let current = self.ledger().get(workbook).unwrap_or(reservation);
                slot.record.reservation = current;
                let persisted = ready.and_then(|_| self.persist(&slot.record));
                if let Err(e) = persisted {
                    drop(slots);
                    let _ = self.provider.stop(&handle);
                    return Err(self.finish_failed(workbook, operation, e));
                }
                slot.handle = Some(handle.clone());
                // Sheets live outside DuckDB's buffer manager: take their estimate out of its share.
                let sheets = handle.sheet_memory_estimate();
                {
                    let mut ledger = self.ledger();
                    if sheets > 0 {
                        ledger.set_sheet_estimate(workbook, sheets);
                    }
                    let mem = ledger.duckdb_memory(workbook, &current);
                    if let Err(e) = handle.apply_limits(mem, &current) {
                        tracing::warn!(workbook, "could not apply resource limits: {e:#}");
                    }
                }
                let lease = Lease::new(handle.clone());
                self.changed.notify_all();
                drop(slots);
                self.pin_spec(&handle);
                // Worker functions run in the server's runtime, unless off for this space;
                // calls the open found missing are computed in the background.
                let (runtime, off) = (self.function_runtime.clone(), adm.python_off.clone());
                if let Err(e) = handle.with_engine(move |wb| wb.set_function_runtime(runtime, off)) {
                    tracing::warn!(workbook, "could not set up Python functions: {e:#}");
                }
                handle.compute_functions_later();
                Ok(lease)
            }
            Err(e) => {
                drop(slots);
                Err(self.finish_failed(workbook, operation, e))
            }
        }
    }

    /// Record a failure for a start that never got past `Requested`.
    fn fail_unstarted(&self, mut record: RuntimeAllocation, operation: &str, failure: RuntimeFailure) -> RuntimeFailure {
        let _ = record.transition(AllocationState::Failed, operation);
        record.failure = Some(failure.clone());
        let _ = self.persist(&record);
        failure
    }

    /// A start failed after its slot was inserted: record it, release the share, regrow
    /// the others, wake waiters.
    fn finish_failed(&self, workbook: &str, operation: &str, failure: RuntimeFailure) -> RuntimeFailure {
        let mut slots = self.slots();
        if let Some(mut slot) = slots.map.remove(workbook) {
            let to = if failure.kind == FailureKind::Cancelled { AllocationState::Stopped } else { AllocationState::Failed };
            if slot.record.state.can_become(to) {
                let _ = slot.record.transition(to, operation);
            }
            slot.record.failure = Some(failure.clone());
            let _ = self.persist(&slot.record);
        }
        self.ledger().release(workbook);
        self.replan(&slots);
        self.changed.notify_all();
        failure
    }

    /// Record the spec the workbook now runs with (first open, or after an upgrade).
    fn pin_spec(&self, handle: &WorkbookHandle) {
        let mut meta = handle.meta.lock().unwrap();
        if meta.runtime_spec.as_deref() != Some(&self.spec_id) {
            meta.runtime_spec = Some(self.spec_id.clone());
            if let Err(e) = crate::state::write_meta(&handle.dir, &meta) {
                tracing::warn!("could not pin the runtime spec: {e:#}");
            }
        }
    }

    fn ledger(&self) -> MutexGuard<'_, Ledger> {
        self.ledger.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record a plan in the ledger and apply it to the engines that are open (one still
    /// starting gets its share once ready).
    fn apply_plan(&self, slots: &Slots, plan: &HashMap<String, Reservation>) {
        let mut ledger = self.ledger();
        for (wb, r) in plan {
            ledger.set(wb, *r);
            if let Some(slot) = slots.map.get(wb) {
                if let Some(h) = &slot.handle {
                    let mem = ledger.duckdb_memory(wb, r);
                    if let Err(e) = h.apply_limits(mem, r) {
                        tracing::warn!(workbook = wb.as_str(), "could not apply resource limits: {e:#}");
                    }
                }
            }
        }
    }

    /// Recompute everyone's share after one left.
    fn replan(&self, slots: &Slots) {
        // Bound first: a guard in an `if let` scrutinee would live through the block.
        let plan = self.ledger().plan(&self.budget, None, self.policy.min_memory_bytes);
        if let Ok(plan) = plan {
            self.apply_plan(slots, &plan);
        }
    }

    /// Ready and not in use: no request holds it, nothing runs or waits.
    fn is_quiet(slot: &Slot) -> bool {
        slot.record.state == AllocationState::Ready && slot.handle.as_ref().is_some_and(|h| h.uses() == 0 && h.is_quiet())
    }

    /// Quiet with no meaningful activity for the idle timeout. Open sessions do not count.
    fn is_idle(&self, slot: &Slot, now: u64) -> bool {
        Self::is_quiet(slot) && now.saturating_sub(slot.handle.as_ref().unwrap().last_activity_ms()) >= self.policy.idle_timeout_ms
    }

    /// May be closed at once to make room: quiet and either nobody has it open or it is
    /// idle anyway. A workbook started for someone who has not connected yet (the page
    /// follows its start, then connects) is spared for [`START_GRACE_MS`]. Dedicated
    /// allocations never are closed to make room.
    fn is_evictable(&self, slot: &Slot, now: u64) -> bool {
        let h = slot.handle.as_ref();
        let unused = h.is_some_and(|h| {
            h.sessions() == 0 && (h.had_session() || now.saturating_sub(slot.record.ready_at_ms.unwrap_or(0)) >= START_GRACE_MS)
        });
        !slot.profile.dedicated && Self::is_quiet(slot) && (unused || self.is_idle(slot, now))
    }

    /// The least recently used workbook that may be closed to make room for `for_workbook`.
    fn capacity_victim(&self, slots: &Slots, for_workbook: &str) -> Option<String> {
        let now = alephgrid_core::storage::now_ms();
        slots
            .map
            .iter()
            .filter(|(k, s)| k.as_str() != for_workbook && self.is_evictable(s, now))
            .map(|(k, s)| (k.clone(), s.handle.as_ref().unwrap().last_activity_ms()))
            .min_by_key(|(_, t)| *t)
            .map(|(k, _)| k)
    }

    /// Whether a cancellation of `workbook`'s start has been requested and not yet seen.
    pub fn cancel_pending(&self, workbook: &str) -> bool {
        self.slots().map.get(workbook).is_some_and(|s| s.cancel.load(Ordering::SeqCst) && s.record.state.is_live())
    }

    /// Cancel a start in progress (or waiting in line). Does nothing to a ready or stopped
    /// allocation.
    pub fn cancel(&self, workbook: &str) -> Option<AllocationState> {
        let mut slots = self.slots();
        let slot = slots.map.get(workbook)?;
        if matches!(slot.record.state, AllocationState::Requested | AllocationState::Starting | AllocationState::Running) {
            slot.cancel.store(true, Ordering::SeqCst);
            self.changed.notify_all();
            // Wait for the starter to observe it.
            while slots.map.get(workbook).is_some_and(|s| s.record.state.is_live() && s.record.state != AllocationState::Ready) {
                slots = self.wait(slots, 100);
            }
        }
        slots.map.get(workbook).map(|s| s.record.state).or_else(|| self.load_record(workbook).map(|r| r.state))
    }

    /// Drain and stop a workbook's allocation. Idempotent; waits for a start in progress
    /// (cancelling it) and for a stop in progress.
    pub fn stop(&self, workbook: &str, operation: &str, reason: StopReason) -> Result<()> {
        let mut slots = self.slots();
        loop {
            let Some(slot) = slots.map.get(workbook) else { return Ok(()) };
            match slot.record.state {
                AllocationState::Ready => break,
                AllocationState::Requested | AllocationState::Starting | AllocationState::Running => {
                    slot.cancel.store(true, Ordering::SeqCst);
                    self.changed.notify_all();
                }
                _ => {}
            }
            slots = self.wait(slots, 100);
        }
        let handle = self.begin_stop(&mut slots, workbook, operation)?;
        drop(slots);
        let (result, _) = self.close_allocation(handle, workbook, operation, reason, false);
        result
    }

    /// Ready → Draining, under the slots lock: from here on nobody gets this handle.
    fn begin_stop(&self, slots: &mut Slots, workbook: &str, operation: &str) -> Result<Arc<WorkbookHandle>> {
        let slot = slots.map.get_mut(workbook).ok_or_else(|| anyhow!("workbook {workbook} is not open"))?;
        slot.record.transition(AllocationState::Draining, operation)?;
        self.persist(&slot.record).map_err(|e| anyhow!(e))?;
        self.changed.notify_all();
        Ok(slot.handle.clone().expect("ready slots have a handle"))
    }

    /// Drain, checkpoint and close a draining allocation, then record it stopped. With
    /// `keep_lock`, the ownership lock is taken out first and returned, still held, and the
    /// slots lock is returned held too, with the slot gone, so the caller can start the
    /// replacement before anyone else can.
    fn close_allocation(
        &self,
        handle: Arc<WorkbookHandle>,
        workbook: &str,
        operation: &str,
        reason: StopReason,
        keep_lock: bool,
    ) -> (Result<()>, Option<Held<'_>>) {
        handle.begin_closing(reason.closed());
        let _ = handle.events.send(reason.event());
        let drained = self.provider.drain(&handle, self.drain_grace);
        let lock = if keep_lock { handle.take_lock() } else { None };
        let stopped = self.provider.stop(&handle);
        drop(handle);
        let mut slots = self.slots();
        let mut result = drained.and(stopped);
        if let Some(mut slot) = slots.map.remove(workbook) {
            let recorded = match &result {
                Ok(_) => slot.record.transition(AllocationState::Stopped, operation),
                Err(e) => {
                    let t = slot.record.transition(AllocationState::Failed, operation);
                    slot.record.failure = Some(RuntimeFailure::new(FailureKind::StartupFailed, format!("stopping failed: {e:#}")));
                    t
                }
            };
            let persisted = recorded.and_then(|_| self.persist(&slot.record).map_err(|e| anyhow!(e)));
            result = result.and(persisted);
        }
        self.ledger().release(workbook);
        self.replan(&slots);
        self.changed.notify_all();
        if keep_lock {
            return (result, Some((slots, lock)));
        }
        drop(slots);
        (result, None)
    }

    /// Stop every allocation without meaningful activity for the policy's idle timeout,
    /// open sessions or not. Returns the workbooks stopped.
    pub fn evict_idle(&self) -> Vec<String> {
        let now = alephgrid_core::storage::now_ms();
        let candidates: Vec<String> = {
            let slots = self.slots();
            slots.map.iter().filter(|(_, s)| self.is_idle(s, now)).map(|(k, _)| k.clone()).collect()
        };
        candidates.into_iter().filter(|wb| self.stop_when(wb, StopReason::Evicted, |c, s| c.is_idle(s, now))).collect()
    }

    /// Stop `workbook` if `still` holds, deciding and moving it to Draining under one lock,
    /// so a request arriving at the same moment either got the handle first (and keeps it
    /// open) or waits for the new allocation.
    fn stop_when(&self, workbook: &str, reason: StopReason, still: impl Fn(&Self, &Slot) -> bool) -> bool {
        let operation = operation_id("evict");
        let handle = {
            let mut slots = self.slots();
            if !slots.map.get(workbook).is_some_and(|s| still(self, s)) {
                return false;
            }
            match self.begin_stop(&mut slots, workbook, &operation) {
                Ok(h) => h,
                Err(_) => return false,
            }
        };
        self.close_allocation(handle, workbook, &operation, reason, false).0.is_ok()
    }

    /// Close to make room, if it still may be.
    fn stop_if_evictable(&self, workbook: &str) -> bool {
        let now = alephgrid_core::storage::now_ms();
        self.stop_when(workbook, StopReason::Evicted, |c, s| c.is_evictable(s, now))
    }

    /// Controlled restart (7.15): check that `adm` (the new profile) fits, counting the
    /// current allocation as released, before touching anything; then drain and close the
    /// allocation and start its replacement with the ownership lock handed over, so no other
    /// process can take the workbook in between. Collaborators are told
    /// (`workbook_closed: restarting`) and reconnect. If the replacement fails to start,
    /// the workbook is started again as it was (`previous`), and the failure is reported.
    /// A stopped workbook is simply started.
    pub fn restart(&self, workbook: &str, operation: &str, adm: Admission, previous: Admission) -> std::result::Result<Lease, RuntimeFailure> {
        let set_profile = |profile: &str| -> std::result::Result<(), RuntimeFailure> {
            let dir = self.data.join(workbook);
            let mut meta = read_meta(&dir).map_err(|e| RuntimeFailure::new(FailureKind::StartupFailed, format!("{e:#}")))?;
            meta.profile = (profile != super::STANDARD).then(|| profile.to_string());
            crate::state::write_meta(&dir, &meta).map_err(|e| RuntimeFailure::new(FailureKind::StartupFailed, format!("{e:#}")))
        };
        let mut slots = self.slots();
        loop {
            if slots.stopping {
                return Err(RuntimeFailure::new(FailureKind::Capacity, "the server is shutting down"));
            }
            match slots.map.get(workbook).map(|s| s.record.state) {
                Some(AllocationState::Ready) => break,
                Some(s) if s.is_live() => slots = self.wait(slots, 100),
                _ => {
                    // Not running: record the profile and open it with it.
                    self.check_space(&slots, &adm, None)?;
                    drop(slots);
                    set_profile(&adm.profile.id)?;
                    return self.ensure(workbook, operation, &|| Ok(adm.clone()));
                }
            }
        }
        self.check_space(&slots, &adm, Some(workbook))?;
        self.ledger().plan(&self.budget, Some((workbook, adm.profile.fixed())), self.policy.min_memory_bytes)?;
        let handle = self.begin_stop(&mut slots, workbook, operation).map_err(|e| RuntimeFailure::new(FailureKind::StartupFailed, format!("{e:#}")))?;
        drop(slots);
        let (closed, held) = self.close_allocation(handle, workbook, operation, StopReason::Restarting, true);
        let (slots, lock) = held.expect("kept");
        if let Err(e) = closed {
            drop(slots);
            return Err(RuntimeFailure::new(FailureKind::StartupFailed, format!("{e:#}")));
        }
        if let Err(e) = set_profile(&adm.profile.id) {
            drop(slots);
            drop(lock);
            return Err(e);
        }
        match self.start(slots, workbook, operation, adm, lock) {
            Ok(lease) => Ok(lease),
            Err(failure) => {
                tracing::warn!(workbook, "restart failed ({failure}); starting it again as it was");
                let _ = set_profile(&previous.profile.id);
                let again = operation_id("restore");
                let _ = self.ensure(workbook, &again, &|| Ok(previous.clone()));
                Err(failure)
            }
        }
    }

    /// Expire unused view materializations in every open workbook.
    pub fn expire_views(&self, ttl_ms: u64) -> usize {
        let handles: Vec<Arc<WorkbookHandle>> = self.slots().map.values().filter_map(|s| s.handle.clone()).collect();
        handles.iter().map(|h| h.expire_views(ttl_ms)).sum()
    }

    /// Refuse new starts of `workbook` (it is being deleted) until [`Controller::unblock`].
    pub fn block(&self, workbook: &str) -> bool {
        self.slots().blocked.insert(workbook.to_string())
    }

    pub fn unblock(&self, workbook: &str) {
        self.slots().blocked.remove(workbook);
    }

    pub fn handle(&self, workbook: &str) -> Option<Arc<WorkbookHandle>> {
        self.slots().map.get(workbook).and_then(|s| s.handle.clone())
    }

    pub fn live(&self) -> Vec<String> {
        self.slots().map.keys().cloned().collect()
    }

    /// Whether a refusal is still possible for one more allocation in `adm`'s space.
    pub fn check_admission(&self, adm: &Admission) -> std::result::Result<(), RuntimeFailure> {
        self.check_space(&self.slots(), adm, None)
    }

    /// Live allocations of `space` and the memory they are guaranteed.
    pub fn space_usage(&self, space: &str) -> (u64, u64) {
        let slots = self.slots();
        let live: Vec<&Slot> = slots.map.values().filter(|s| s.space == space && s.record.state.is_live()).collect();
        (live.len() as u64, live.iter().map(|s| s.profile.guaranteed_memory(self.policy.min_memory_bytes)).sum())
    }

    /// A workbook moved to another space: its live allocation now counts there.
    pub fn moved(&self, workbook: &str, space: &str) {
        if let Some(s) = self.slots().map.get_mut(workbook) {
            s.space = space.to_string();
        }
    }

    /// Stop admitting starts and stop every allocation (shutdown).
    pub fn shutdown(&self) {
        let live: Vec<String> = {
            let mut slots = self.slots();
            slots.stopping = true;
            self.changed.notify_all();
            slots.map.keys().cloned().collect()
        };
        for wb in live {
            if let Err(e) = self.stop(&wb, &operation_id("shutdown"), StopReason::Shutdown) {
                tracing::warn!(workbook = wb.as_str(), "stopping on shutdown failed: {e:#}");
            }
        }
    }

    pub fn status(&self, workbook: &str) -> Result<RuntimeStatus> {
        let dir = self.data.join(workbook);
        let meta = read_meta(&dir)?;
        let (allocation, sessions, note, queue_position, last_activity_ms) = {
            let slots = self.slots();
            match slots.map.get(workbook) {
                Some(s) => (
                    Some(s.record.clone()),
                    s.handle.as_ref().map(|h| h.sessions()).unwrap_or(0),
                    s.note.clone(),
                    slots.queue_position(workbook),
                    s.handle.as_ref().map(|h| h.last_activity_ms()),
                ),
                None => (self.load_record(workbook), 0, None, None, None),
            }
        };
        let spec_id = meta.runtime_spec.clone().unwrap_or_else(|| self.spec_id.clone());
        let spec = super::load_spec(&self.control, &spec_id).unwrap_or_else(|_| self.spec.clone());
        let ownership = allocation
            .as_ref()
            .filter(|a| a.state.is_live())
            .and_then(|_| std::fs::read(dir.join("owner.json")).ok())
            .and_then(|b| serde_json::from_slice(&b).ok());
        Ok(RuntimeStatus {
            workbook: workbook.to_string(),
            spec,
            spec_id,
            policy: self.policy.clone(),
            allocation,
            ownership,
            sessions,
            profile: meta.profile.clone().unwrap_or_else(|| super::STANDARD.to_string()),
            note,
            queue_position,
            last_activity_ms,
            storage_bytes: crate::state::dir_size(&dir),
        })
    }

    /// Whether `operation` already moved this workbook's current allocation.
    pub fn seen(&self, workbook: &str, operation: &str) -> bool {
        let slots = self.slots();
        let record = slots.map.get(workbook).map(|s| s.record.clone()).or_else(|| self.load_record(workbook));
        record.is_some_and(|r| r.transitions.iter().any(|t| t.operation == operation))
    }

    pub fn reserve_upload(&self, bytes: u64) -> std::result::Result<(), u64> {
        self.ledger().reserve_upload(&self.budget, bytes)
    }

    pub fn release_upload(&self, bytes: u64) {
        self.ledger().release_upload(bytes)
    }

    pub fn usage(&self) -> ResourceUsage {
        let handles: Vec<(String, Arc<WorkbookHandle>)> =
            self.slots().map.iter().filter_map(|(k, s)| s.handle.clone().map(|h| (k.clone(), h))).collect();
        let ledger = self.ledger();
        let reservations: HashMap<String, Reservation> = ledger.reservations().into_iter().collect();
        let allocations = handles
            .iter()
            .map(|(k, h)| {
                let (mem, threads) = h.limits().unwrap_or_default();
                AllocationUsage {
                    workbook: k.clone(),
                    reservation: reservations.get(k).copied().unwrap_or_default(),
                    duckdb_memory_limit: mem,
                    duckdb_threads: threads,
                    jobs: h.sched.load() as u64,
                }
            })
            .collect();
        ResourceUsage {
            budget: self.budget,
            allocations,
            reserved: ledger.total(),
            upload_bytes: ledger.uploads(),
            upload_budget: self.budget.upload_bytes(),
        }
    }

    pub fn temp_dir(&self, workbook: &str) -> PathBuf {
        self.provider.temp_dir(workbook)
    }
}

/// Why an allocation stops; decides what its clients are told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Deleted,
    Evicted,
    Requested,
    Shutdown,
    /// A controlled restart (7.15): clients reconnect to the replacement.
    Restarting,
}

impl StopReason {
    fn closed(self) -> crate::state::Closed {
        match self {
            StopReason::Deleted => crate::state::Closed::Deleted,
            _ => crate::state::Closed::Stopped,
        }
    }

    fn event(self) -> crate::ws::Event {
        match self {
            StopReason::Deleted => crate::ws::Event::WorkbookDeleted { origin: 0 },
            StopReason::Shutdown => crate::ws::Event::WorkbookClosed { reason: "shutdown".into() },
            StopReason::Evicted => crate::ws::Event::WorkbookClosed { reason: "idle".into() },
            StopReason::Requested => crate::ws::Event::WorkbookClosed { reason: "stopped".into() },
            StopReason::Restarting => crate::ws::Event::WorkbookClosed { reason: "restarting".into() },
        }
    }
}

pub fn meta_of(dir: &Path) -> Result<WorkbookMeta> {
    read_meta(dir)
}
