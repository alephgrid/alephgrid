//! Runtime objects and the provider contract (3.11–3.14).
//!
//! * A **workbook** owns its durable identity and data; nothing here changes either.
//! * A [`RuntimeSpec`] is an immutable, content-addressed description of what a workbook
//!   needs to run: engine and storage format, component versions, capabilities. A
//!   workbook pins the spec it was last opened with.
//! * A [`ComputePolicy`] says what resources an allocation may use, which isolation class
//!   it needs and which providers may run it.
//! * A [`RuntimeAllocation`] is the replaceable compute serving one workbook: its state,
//!   reservation, owner and the operations that moved it. At most one is live per
//!   workbook, and an [`OwnershipAssignment`] records which one and under what epoch.
//!
//! Providers implement [`RuntimeProvider`]. This build has one, [`local::LocalProvider`],
//! which opens the workbook inside the server process. The [`controller`] decides when
//! allocations start and stop and keeps their records.

pub mod budget;
pub mod controller;
pub mod local;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

/// Something a workbook may require of its runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Tables,
    Sheets,
    Sql,
    /// Python functions (phase 8); no provider offers it yet.
    Python,
}

/// Immutable description of a runtime. Its id is a hash of its content, so two specs with
/// the same id are the same spec, and a stored spec can never change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RuntimeSpec {
    pub engine: String,
    pub engine_version: String,
    /// Workbook storage format the runtime reads and writes (`rowfinity_engine::storage`).
    pub storage_format: u32,
    /// Versions of the components that compute results.
    pub components: BTreeMap<String, String>,
    pub capabilities: BTreeSet<Capability>,
}

impl RuntimeSpec {
    /// The spec of this build.
    pub fn current() -> RuntimeSpec {
        RuntimeSpec {
            engine: "rowfinity".into(),
            engine_version: env!("CARGO_PKG_VERSION").into(),
            storage_format: rowfinity_engine::storage::FORMAT,
            components: BTreeMap::from([
                ("duckdb".to_string(), duckdb_version()),
                ("ironcalc".to_string(), "0.9.1".to_string()),
            ]),
            capabilities: BTreeSet::from([Capability::Tables, Capability::Sheets, Capability::Sql]),
        }
    }

    /// `rs-` and the first 16 hex digits of the SHA-256 of the canonical JSON.
    pub fn id(&self) -> String {
        let canonical = serde_json::to_vec(self).expect("serializable");
        let digest = Sha256::digest(&canonical);
        format!("rs-{}", hex(&digest[..8]))
    }
}

fn duckdb_version() -> String {
    duckdb::Connection::open_in_memory()
        .and_then(|c| c.query_row("SELECT library_version FROM pragma_version()", [], |r| r.get::<_, String>(0)))
        .unwrap_or_else(|_| "unknown".into())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What separates one workbook's execution from others'. Only `shared_process` exists in
/// this build; the others name boundaries later providers may offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum IsolationClass {
    /// Inside the server process, next to other workbooks. Separates data only by the
    /// server's own code; a bug or a hostile query can affect every workbook. For one
    /// trusted user or team.
    SharedProcess,
    /// A separate OS process per workbook: a crash stays in that process, but it runs as
    /// the same user with the same file access unless the OS confines it further.
    Process,
    /// A container with its own filesystem view and cgroup limits.
    Container,
    /// A virtual machine.
    VirtualMachine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Local,
}

/// Resource and isolation constraints for allocations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct ComputePolicy {
    pub id: String,
    /// Smallest memory share an allocation may run with; below it, opening another
    /// workbook evicts an idle one or fails with `capacity`.
    pub min_memory_bytes: u64,
    /// Largest share for one allocation; `None` is a fair share of the host budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_memory_bytes: Option<u64>,
    pub isolation: IsolationClass,
    /// Close an allocation after this long without sessions or jobs.
    pub idle_timeout_ms: u64,
    pub providers: Vec<ProviderKind>,
}

/// Lifecycle of an allocation. `Requested → Starting → Running → Ready → Draining →
/// Stopped`, or `Failed` from any state before `Ready`. See [`AllocationState::can_become`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AllocationState {
    /// Recorded; nothing acquired yet.
    Requested,
    /// Acquiring ownership and resources, opening (and maybe migrating) the workbook.
    Starting,
    /// The engine is open; checks before serving are running.
    Running,
    /// Serving requests.
    Ready,
    /// Not admitting new work; finishing what runs, then persisting.
    Draining,
    Stopped,
    Failed,
}

impl AllocationState {
    pub fn is_live(self) -> bool {
        !matches!(self, AllocationState::Stopped | AllocationState::Failed)
    }

    /// Allowed transitions. Anything else is a bug and is refused.
    pub fn can_become(self, to: AllocationState) -> bool {
        use AllocationState::*;
        matches!(
            (self, to),
            (Requested, Starting)
                | (Starting, Running)
                | (Running, Ready)
                | (Ready, Draining)
                | (Draining, Stopped)
                | (Requested | Starting | Running, Stopped)
                | (Requested | Starting | Running, Failed)
                // Ready → Failed: found lost after a controller restart.
                | (Ready | Draining, Failed)
        )
    }
}

/// Why an allocation failed or could not start. Each kind maps to one HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// The workbook needs something no allowed provider offers (format, capability, isolation).
    Incompatible,
    /// No resources left on this host.
    Capacity,
    /// Another process holds the workbook.
    OwnedElsewhere,
    /// Opening or checking the workbook failed.
    StartupFailed,
    Cancelled,
    /// The controller restarted while the allocation was live; it no longer exists.
    Lost,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, thiserror::Error)]
#[error("{message}")]
pub struct RuntimeFailure {
    pub kind: FailureKind,
    pub message: String,
}

impl RuntimeFailure {
    pub fn new(kind: FailureKind, message: impl Into<String>) -> RuntimeFailure {
        RuntimeFailure { kind, message: message.into() }
    }
}

/// Resources reserved for one allocation out of the host budget.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Reservation {
    pub memory_bytes: u64,
    pub threads: u64,
    pub temp_disk_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Transition {
    pub at_ms: u64,
    pub from: AllocationState,
    pub to: AllocationState,
    pub operation: String,
}

/// Replaceable compute serving one workbook.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct RuntimeAllocation {
    pub id: String,
    pub workbook: String,
    pub spec: String,
    pub policy: String,
    pub provider: ProviderKind,
    pub state: AllocationState,
    /// Ownership epoch; increases with every allocation of the workbook.
    pub epoch: u64,
    pub reservation: Reservation,
    /// Host and process that own it (`host:pid` for the Local provider).
    pub holder: String,
    pub requested_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ready_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stopped_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub failure: Option<RuntimeFailure>,
    /// Operation ids applied to this allocation, with what they did (bounded).
    pub transitions: Vec<Transition>,
}

/// Most transitions kept per allocation record.
pub const MAX_TRANSITIONS: usize = 32;

impl RuntimeAllocation {
    pub fn transition(&mut self, to: AllocationState, operation: &str) -> Result<()> {
        if !self.state.can_become(to) {
            bail!("allocation {} cannot go from {:?} to {:?}", self.id, self.state, to);
        }
        let at_ms = rowfinity_engine::storage::now_ms();
        self.transitions.push(Transition { at_ms, from: self.state, to, operation: operation.to_string() });
        if self.transitions.len() > MAX_TRANSITIONS {
            self.transitions.remove(0);
        }
        self.state = to;
        match to {
            AllocationState::Ready => self.ready_at_ms = Some(at_ms),
            AllocationState::Stopped | AllocationState::Failed => self.stopped_at_ms = Some(at_ms),
            _ => {}
        }
        Ok(())
    }
}

/// Which allocation owns a workbook. Enforced, for the Local provider, by an exclusive
/// `flock` on `<workbook>/.owner.lock` held for the allocation's lifetime; this file is
/// the record, the lock is the proof.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct OwnershipAssignment {
    pub workbook: String,
    pub allocation: String,
    pub epoch: u64,
    pub holder: String,
    pub acquired_at_ms: u64,
    /// How exclusivity is enforced ("flock" for the Local provider).
    pub mechanism: String,
}

/// What a provider can run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct ProviderCapabilities {
    pub kind: ProviderKind,
    pub max_storage_format: u32,
    pub capabilities: BTreeSet<Capability>,
    pub isolation: Vec<IsolationClass>,
}

impl ProviderCapabilities {
    /// Why this provider cannot run `spec` under `policy`, if it cannot.
    pub fn incompatibility(&self, spec: &RuntimeSpec, policy: &ComputePolicy) -> Option<String> {
        if spec.storage_format > self.max_storage_format {
            return Some(format!(
                "this workbook needs storage format {}; this server supports up to {}",
                spec.storage_format, self.max_storage_format
            ));
        }
        let missing: Vec<String> =
            spec.capabilities.difference(&self.capabilities).map(|c| format!("{c:?}").to_lowercase()).collect();
        if !missing.is_empty() {
            return Some(format!("this workbook needs {}, which this server does not offer", missing.join(", ")));
        }
        if !self.isolation.contains(&policy.isolation) {
            return Some(format!("the policy requires {:?} isolation, which this server does not offer", policy.isolation));
        }
        if !policy.providers.contains(&self.kind) {
            return Some(format!("the policy does not allow the {:?} provider", self.kind));
        }
        None
    }
}

/// A request to make an allocation live.
pub struct EnsureRequest<'a> {
    pub workbook: &'a str,
    pub workbook_dir: &'a Path,
    pub meta: crate::state::WorkbookMeta,
    pub allocation: &'a RuntimeAllocation,
    pub spec: &'a RuntimeSpec,
    pub policy: &'a ComputePolicy,
    pub reservation: Reservation,
    /// The engine's own memory limit: the reservation minus what other parts (sheets) need.
    pub engine_memory_bytes: u64,
    /// Called at each stage boundary ("starting", "running"); an error aborts the start
    /// (cancellation, or a test's injected failure).
    pub checkpoint: &'a (dyn Fn(AllocationState) -> std::result::Result<(), RuntimeFailure> + Sync),
}

/// The narrow contract between the controller and whatever runs workbooks. All methods
/// are blocking and idempotent for the same allocation: ensuring an allocation that is
/// already live returns it, stopping a stopped one succeeds.
pub trait RuntimeProvider: Send + Sync {
    fn capabilities(&self) -> ProviderCapabilities;
    /// Acquire ownership and resources and open the workbook. On any error everything
    /// acquired so far is released before returning.
    fn ensure(&self, req: &EnsureRequest) -> std::result::Result<std::sync::Arc<crate::state::WorkbookHandle>, RuntimeFailure>;
    /// Stop admitting work, wait up to `deadline` for running work, persist.
    fn drain(&self, handle: &crate::state::WorkbookHandle, deadline: std::time::Duration) -> Result<()>;
    /// Close the workbook and release ownership and resources.
    fn stop(&self, handle: &crate::state::WorkbookHandle) -> Result<()>;
    /// After a controller restart: is anyone still holding this workbook? Cleans up what a
    /// dead allocation left behind (scratch files). `Ok(true)` means it is held elsewhere.
    fn reconcile(&self, record: &RuntimeAllocation, workbook_dir: &Path) -> Result<bool>;
}

// --------------------------------------------------------- control store

/// Write `bytes` to `path` through a temporary file, synced, then renamed.
pub fn write_durably(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("{}.{n}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        crate::state::fault_at("write_durably", path)?;
        let mut f = std::fs::File::create(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
        if let Some(dir) = path.parent() {
            rowfinity_engine::storage::sync_dir(dir);
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Store a spec under its id. A spec file is never rewritten: an existing file must hold
/// exactly this spec.
pub fn store_spec(control: &Path, spec: &RuntimeSpec) -> Result<String> {
    let id = spec.id();
    let dir = control.join("specs");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{id}.json"));
    match std::fs::read(&path) {
        Ok(bytes) => {
            let stored: RuntimeSpec = serde_json::from_slice(&bytes).context("unreadable runtime spec")?;
            if &stored != spec || stored.id() != id {
                bail!("runtime spec {id} on disk does not match its id");
            }
        }
        Err(_) => write_durably(&path, &serde_json::to_vec_pretty(spec)?)?,
    }
    Ok(id)
}

pub fn load_spec(control: &Path, id: &str) -> Result<RuntimeSpec> {
    let path = control.join("specs").join(format!("{id}.json"));
    let spec: RuntimeSpec = serde_json::from_slice(&std::fs::read(&path).with_context(|| format!("runtime spec {id} is missing"))?)?;
    if spec.id() != id {
        bail!("runtime spec {id} on disk does not match its id");
    }
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::AllocationState::*;
    use super::*;

    #[test]
    fn only_listed_transitions_are_allowed() {
        let all = [Requested, Starting, Running, Ready, Draining, Stopped, Failed];
        let allowed = [
            (Requested, Starting), (Starting, Running), (Running, Ready), (Ready, Draining), (Draining, Stopped),
            (Requested, Stopped), (Starting, Stopped), (Running, Stopped),
            (Requested, Failed), (Starting, Failed), (Running, Failed), (Ready, Failed), (Draining, Failed),
        ];
        for from in all {
            for to in all {
                assert_eq!(from.can_become(to), allowed.contains(&(from, to)), "{from:?} → {to:?}");
            }
        }
        // Terminal states go nowhere; nothing skips straight to Ready.
        assert!(all.iter().all(|&s| !Stopped.can_become(s) && !Failed.can_become(s)));
        assert!(all.iter().filter(|&&s| s.can_become(Ready)).eq([Running].iter()));
    }

    #[test]
    fn transitions_are_recorded_with_their_operation_and_bounded() {
        let mut a = RuntimeAllocation {
            id: "al".into(), workbook: "w".into(), spec: "s".into(), policy: "p".into(), provider: ProviderKind::Local,
            state: Requested, epoch: 1, reservation: Reservation::default(), holder: "h".into(), requested_at_ms: 0,
            ready_at_ms: None, stopped_at_ms: None, failure: None, transitions: vec![],
        };
        a.transition(Starting, "op1").unwrap();
        assert!(a.transition(Ready, "op1").is_err(), "skipping Running is refused");
        a.transition(Running, "op1").unwrap();
        a.transition(Ready, "op1").unwrap();
        assert!(a.ready_at_ms.is_some());
        assert_eq!(a.transitions.iter().map(|t| t.to).collect::<Vec<_>>(), [Starting, Running, Ready]);
        for i in 0..40 {
            a.state = Ready;
            a.transition(Draining, &format!("x{i}")).unwrap();
        }
        assert_eq!(a.transitions.len(), MAX_TRANSITIONS);
    }

    #[test]
    fn incompatibility_names_the_reason() {
        let provider = ProviderCapabilities {
            kind: ProviderKind::Local, max_storage_format: rowfinity_engine::storage::FORMAT,
            capabilities: [Capability::Tables, Capability::Sheets, Capability::Sql].into_iter().collect(),
            isolation: vec![IsolationClass::SharedProcess],
        };
        let policy = ComputePolicy {
            id: "p".into(), min_memory_bytes: 0, max_memory_bytes: None, isolation: IsolationClass::SharedProcess,
            idle_timeout_ms: 0, providers: vec![ProviderKind::Local],
        };
        let spec = RuntimeSpec::current();
        assert_eq!(provider.incompatibility(&spec, &policy), None);
        let mut newer = spec.clone();
        newer.storage_format = rowfinity_engine::storage::FORMAT + 1;
        let reason = provider.incompatibility(&newer, &policy).unwrap();
        assert!(reason.contains(&format!("storage format {}", rowfinity_engine::storage::FORMAT + 1)), "{reason}");
        let mut python = spec.clone();
        python.capabilities.insert(Capability::Python);
        assert!(provider.incompatibility(&python, &policy).unwrap().contains("python"));
        let container = ComputePolicy { isolation: IsolationClass::Container, ..policy.clone() };
        assert!(provider.incompatibility(&spec, &container).unwrap().contains("Container"));
    }
}

