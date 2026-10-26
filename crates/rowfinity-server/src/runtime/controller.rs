//! Lifecycle controller (3.13–3.14): decides when allocations start and stop, keeps
//! their records in `<data>/.control/allocations/<workbook>.json`, and reconciles those
//! records after a restart.
//!
//! Rules:
//! * One live allocation per workbook. Concurrent requests to open the same workbook wait
//!   for the one start in progress (single flight), so duplicates never allocate twice.
//! * Every transition is persisted before it takes effect for callers.
//! * Operations carry ids. Repeating an operation id that already moved this workbook's
//!   allocation returns the current status without doing anything again.
//! * A failed or cancelled start releases its lock, reservation and scratch files before
//!   the failure is reported.
//! * Ownership is uncertain only if another process holds the lock; then the workbook is
//!   refused (`owned_elsewhere`) rather than opened twice.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::Serialize;
use ts_rs::TS;

use super::budget::{HostBudget, Ledger, ResourceUsage, AllocationUsage};
use super::local::LocalProvider;
use super::{
    AllocationState, ComputePolicy, EnsureRequest, FailureKind, OwnershipAssignment, Reservation, RuntimeAllocation,
    RuntimeFailure, RuntimeProvider, RuntimeSpec,
};
use crate::state::{read_meta, WorkbookHandle, WorkbookMeta};

/// Called at each start stage with the workbook id; an error fails or cancels the start.
pub type StageHook = Arc<dyn Fn(&str, AllocationState) -> std::result::Result<(), RuntimeFailure> + Send + Sync>;

struct Slot {
    record: RuntimeAllocation,
    handle: Option<Arc<WorkbookHandle>>,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct Slots {
    map: HashMap<String, Slot>,
    /// Workbooks that may not start (being deleted, server stopping).
    blocked: HashSet<String>,
    stopping: bool,
}

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
}

fn new_id(prefix: &str) -> String {
    use std::sync::atomic::AtomicU64;
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{:x}{:04x}{n:x}", rowfinity_engine::storage::now_ms(), std::process::id() & 0xffff)
}

pub fn operation_id(kind: &str) -> String {
    new_id(kind)
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
        })
    }

    fn slots(&self) -> MutexGuard<'_, Slots> {
        self.slots.lock().unwrap_or_else(|e| e.into_inner())
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

    /// The live handle of a workbook, starting an allocation if there is none.
    pub fn ensure(&self, workbook: &str, operation: &str) -> std::result::Result<Arc<WorkbookHandle>, RuntimeFailure> {
        loop {
            let slots = self.slots();
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
                    return Ok(h);
                }
                Some(s) if s.record.state.is_live() => {
                    // Someone else is starting or stopping it: wait for the outcome.
                    let _guard = self.changed.wait_timeout(slots, Duration::from_millis(200)).unwrap_or_else(|e| e.into_inner());
                    continue;
                }
                _ => {}
            }
            // Nobody is: this caller starts it.
            return self.start(slots, workbook, operation);
        }
    }

    fn start<'a>(&'a self, mut slots: MutexGuard<'a, Slots>, workbook: &str, operation: &str) -> std::result::Result<Arc<WorkbookHandle>, RuntimeFailure> {
        let dir = self.data.join(workbook);
        let meta = read_meta(&dir).map_err(|e| RuntimeFailure::new(FailureKind::StartupFailed, format!("{e:#}")))?;
        let previous = self.load_record(workbook);
        let epoch = previous.as_ref().map(|r| r.epoch).unwrap_or(0) + 1;
        // The spec the workbook is pinned to (or this build's, the first time).
        let spec_id = meta.runtime_spec.clone().unwrap_or_else(|| self.spec_id.clone());
        let spec = if spec_id == self.spec_id { self.spec.clone() } else { super::load_spec(&self.control, &spec_id).map_err(|e| RuntimeFailure::new(FailureKind::Incompatible, format!("{e:#}")))? };
        let mut record = RuntimeAllocation {
            id: new_id("al"),
            workbook: workbook.to_string(),
            spec: spec_id,
            policy: self.policy.id.clone(),
            provider: super::ProviderKind::Local,
            state: AllocationState::Requested,
            epoch,
            reservation: Reservation::default(),
            holder: LocalProvider::holder(),
            requested_at_ms: rowfinity_engine::storage::now_ms(),
            ready_at_ms: None,
            stopped_at_ms: None,
            failure: None,
            transitions: vec![],
        };
        if let Some(why) = self.provider.capabilities().incompatibility(&spec, &self.policy) {
            return Err(self.fail_unstarted(record, operation, RuntimeFailure::new(FailureKind::Incompatible, why)));
        }
        // Reserve a share, evicting idle workbooks if the budget is full.
        let mut attempts = 0;
        let plan = loop {
            attempts += 1;
            let plan = self.ledger().plan(&self.budget, Some(workbook), self.policy.min_memory_bytes);
            match plan {
                Ok(p) => break p,
                Err(e) => match self.idle_victim(&slots).filter(|_| attempts < 16) {
                    Some(victim) => {
                        drop(slots);
                        let evicted = self.stop_if_idle_now(&victim);
                        slots = self.slots();
                        if !evicted {
                            continue;
                        }
                    }
                    None => return Err(self.fail_unstarted(record, operation, e)),
                },
            }
        };
        record.reservation = plan[workbook];
        let cancel = Arc::new(AtomicBool::new(false));
        self.persist(&record)?;
        slots.map.insert(workbook.to_string(), Slot { record: record.clone(), handle: None, cancel: cancel.clone() });
        // Shrink the others first, so the sum stays within the budget.
        self.apply_plan(&slots, &plan, Some(workbook));
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
        let engine_memory = self.ledger().duckdb_memory(workbook, &record.reservation);
        let req = EnsureRequest {
            workbook,
            workbook_dir: &dir,
            meta,
            allocation: &record,
            spec: &spec,
            policy: &self.policy,
            reservation: record.reservation,
            engine_memory_bytes: engine_memory,
            checkpoint: &advance,
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
                let persisted = ready.and_then(|_| self.persist(&slot.record));
                if let Err(e) = persisted {
                    drop(slots);
                    let _ = self.provider.stop(&handle);
                    return Err(self.finish_failed(workbook, operation, e));
                }
                slot.handle = Some(handle.clone());
                self.ledger().set(workbook, record.reservation);
                self.changed.notify_all();
                drop(slots);
                self.pin_spec(&handle);
                Ok(handle)
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
        // Bound first: a guard in an `if let` scrutinee would live through the block.
        let plan = self.ledger().plan(&self.budget, None, self.policy.min_memory_bytes);
        if let Ok(plan) = plan {
            self.apply_plan(&slots, &plan, None);
        }
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

    /// Apply reservations to live engines (not `except`, which is not open yet).
    fn apply_plan(&self, slots: &Slots, plan: &HashMap<String, Reservation>, except: Option<&str>) {
        let mut ledger = self.ledger();
        for (wb, r) in plan {
            if Some(wb.as_str()) == except {
                continue;
            }
            if let Some(h) = slots.map.get(wb).and_then(|s| s.handle.clone()) {
                let mem = ledger.duckdb_memory(wb, r);
                if let Err(e) = h.apply_limits(mem, r) {
                    tracing::warn!(workbook = wb.as_str(), "could not apply resource limits: {e:#}");
                }
                ledger.set(wb, *r);
            }
        }
    }

    /// Ready, and nobody holds its handle but the controller (no session, no request in
    /// flight) and no work is running or queued.
    fn is_idle(slot: &Slot) -> bool {
        slot.record.state == AllocationState::Ready
            && slot.handle.as_ref().is_some_and(|h| Arc::strong_count(h) == 1 && h.is_quiet())
    }

    /// The least recently used idle workbook, if any.
    fn idle_victim(&self, slots: &Slots) -> Option<String> {
        slots
            .map
            .iter()
            .filter(|(_, s)| Self::is_idle(s))
            .map(|(k, s)| (k.clone(), s.handle.as_ref().unwrap().last_activity_ms()))
            .min_by_key(|(_, t)| *t)
            .map(|(k, _)| k)
    }

    /// Whether a cancellation of `workbook`'s start has been requested and not yet seen.
    pub fn cancel_pending(&self, workbook: &str) -> bool {
        self.slots().map.get(workbook).is_some_and(|s| s.cancel.load(Ordering::SeqCst) && s.record.state.is_live())
    }

    /// Cancel a start in progress. Does nothing to a ready or stopped allocation.
    pub fn cancel(&self, workbook: &str) -> Option<AllocationState> {
        let mut slots = self.slots();
        let slot = slots.map.get(workbook)?;
        if matches!(slot.record.state, AllocationState::Requested | AllocationState::Starting | AllocationState::Running) {
            slot.cancel.store(true, Ordering::SeqCst);
            // Wait for the starter to observe it.
            while slots.map.get(workbook).is_some_and(|s| s.record.state.is_live() && s.record.state != AllocationState::Ready) {
                slots = self.changed.wait_timeout(slots, Duration::from_millis(100)).unwrap_or_else(|e| e.into_inner()).0;
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
                }
                _ => {}
            }
            slots = self.changed.wait_timeout(slots, Duration::from_millis(100)).unwrap_or_else(|e| e.into_inner()).0;
        }
        let handle = self.begin_stop(&mut slots, workbook, operation)?;
        drop(slots);
        self.finish_stop(handle, workbook, operation, reason)
    }

    /// Ready → Draining, under the slots lock: from here on nobody gets this handle.
    fn begin_stop(&self, slots: &mut Slots, workbook: &str, operation: &str) -> Result<Arc<WorkbookHandle>> {
        let slot = slots.map.get_mut(workbook).ok_or_else(|| anyhow!("workbook {workbook} is not open"))?;
        slot.record.transition(AllocationState::Draining, operation)?;
        self.persist(&slot.record).map_err(|e| anyhow!(e))?;
        self.changed.notify_all();
        Ok(slot.handle.clone().expect("ready slots have a handle"))
    }

    fn finish_stop(&self, handle: Arc<WorkbookHandle>, workbook: &str, operation: &str, reason: StopReason) -> Result<()> {
        handle.begin_closing(reason.closed());
        let _ = handle.events.send(reason.event());
        let drained = self.provider.drain(&handle, self.drain_grace);
        let stopped = self.provider.stop(&handle);
        drop(handle);
        let mut slots = self.slots();
        if let Some(mut slot) = slots.map.remove(workbook) {
            match drained.as_ref().and(stopped.as_ref()) {
                Ok(_) => slot.record.transition(AllocationState::Stopped, operation)?,
                Err(e) => {
                    slot.record.transition(AllocationState::Failed, operation)?;
                    slot.record.failure = Some(RuntimeFailure::new(FailureKind::StartupFailed, format!("stopping failed: {e:#}")));
                }
            }
            self.persist(&slot.record).map_err(|e| anyhow!(e))?;
        }
        self.ledger().release(workbook);
        // Bound first: a guard in an `if let` scrutinee would live through the block.
        let plan = self.ledger().plan(&self.budget, None, self.policy.min_memory_bytes);
        if let Ok(plan) = plan {
            self.apply_plan(&slots, &plan, None);
        }
        self.changed.notify_all();
        drop(slots);
        drained.and(stopped)
    }

    /// Stop every allocation that has been idle (no sessions, no work) for the policy's
    /// idle timeout. Returns the workbooks stopped.
    pub fn evict_idle(&self) -> Vec<String> {
        let now = rowfinity_engine::storage::now_ms();
        let candidates: Vec<String> = {
            let slots = self.slots();
            slots
                .map
                .iter()
                .filter(|(_, s)| Self::is_idle(s))
                .filter(|(_, s)| now.saturating_sub(s.handle.as_ref().unwrap().last_activity_ms()) >= self.policy.idle_timeout_ms)
                .map(|(k, _)| k.clone())
                .collect()
        };
        candidates.into_iter().filter(|wb| self.stop_if_idle(wb, now)).collect()
    }

    /// Evict `workbook` if it is still idle, deciding and moving it to Draining under one
    /// lock, so a session arriving at the same moment either got the handle first (and
    /// keeps it open) or waits for the new allocation.
    fn stop_if_idle(&self, workbook: &str, now: u64) -> bool {
        let operation = operation_id("evict");
        let handle = {
            let mut slots = self.slots();
            let still_idle = slots.map.get(workbook).is_some_and(|s| {
                Self::is_idle(s) && now.saturating_sub(s.handle.as_ref().unwrap().last_activity_ms()) >= self.policy.idle_timeout_ms
            });
            if !still_idle {
                return false;
            }
            match self.begin_stop(&mut slots, workbook, &operation) {
                Ok(h) => h,
                Err(_) => return false,
            }
        };
        self.finish_stop(handle, workbook, &operation, StopReason::Evicted).is_ok()
    }

    /// Evict regardless of how long it has been idle (to make room).
    fn stop_if_idle_now(&self, workbook: &str) -> bool {
        let operation = operation_id("evict");
        let handle = {
            let mut slots = self.slots();
            if !slots.map.get(workbook).is_some_and(Self::is_idle) {
                return false;
            }
            match self.begin_stop(&mut slots, workbook, &operation) {
                Ok(h) => h,
                Err(_) => return false,
            }
        };
        self.finish_stop(handle, workbook, &operation, StopReason::Evicted).is_ok()
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

    /// Stop admitting starts and stop every allocation (shutdown).
    pub fn shutdown(&self) {
        let live: Vec<String> = {
            let mut slots = self.slots();
            slots.stopping = true;
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
        let (allocation, sessions) = {
            let slots = self.slots();
            match slots.map.get(workbook) {
                Some(s) => (Some(s.record.clone()), s.handle.as_ref().map(|h| h.sessions()).unwrap_or(0)),
                None => (self.load_record(workbook), 0),
            }
        };
        let spec_id = meta.runtime_spec.clone().unwrap_or_else(|| self.spec_id.clone());
        let spec = super::load_spec(&self.control, &spec_id).unwrap_or_else(|_| self.spec.clone());
        let ownership = allocation
            .as_ref()
            .filter(|a| a.state.is_live())
            .and_then(|_| std::fs::read(dir.join("owner.json")).ok())
            .and_then(|b| serde_json::from_slice(&b).ok());
        Ok(RuntimeStatus { workbook: workbook.to_string(), spec, spec_id, policy: self.policy.clone(), allocation, ownership, sessions })
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
                AllocationUsage { workbook: k.clone(), reservation: reservations.get(k).copied().unwrap_or_default(), duckdb_memory_limit: mem, duckdb_threads: threads }
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
        }
    }
}

pub fn meta_of(dir: &Path) -> Result<WorkbookMeta> {
    read_meta(dir)
}
