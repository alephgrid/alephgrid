//! The Local provider: allocations run inside the server process (isolation class
//! `shared_process`). Exclusivity is an exclusive `flock` on `<workbook>/.owner.lock`,
//! held by the open file for the allocation's lifetime, so a second allocation of the same
//! workbook, in this process or another one on this host, cannot start. `flock` is
//! advisory and reliable on local filesystems only; it is not a fence across hosts or on
//! network filesystems.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use alephgrid_core::{TooNew, Workbook, WrongWorkbook};

use super::{
    AllocationState, Capability, EnsureRequest, FailureKind, IsolationClass, OwnershipAssignment, ProviderCapabilities,
    ProviderKind, RuntimeFailure, RuntimeProvider,
};
use crate::state::WorkbookHandle;

/// Reader connections per workbook (reads that run alongside a change).
pub const READERS: usize = 2;

pub struct LocalProvider {
    /// `<data>/.tmp`: one DuckDB temp directory per allocation.
    pub temp_root: PathBuf,
}

impl LocalProvider {
    pub fn temp_dir(&self, workbook: &str) -> PathBuf {
        self.temp_root.join(workbook)
    }

    pub fn holder() -> String {
        static HOST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        format!("{}:{}", HOST.get_or_init(hostname), std::process::id())
    }

    /// Try to take the ownership lock without waiting.
    pub fn lock(dir: &Path) -> std::result::Result<File, RuntimeFailure> {
        let path = dir.join(".owner.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| RuntimeFailure::new(FailureKind::StartupFailed, format!("cannot open {}: {e}", path.display())))?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => Err(RuntimeFailure::new(
                FailureKind::OwnedElsewhere,
                "another process has this workbook open; it can only be opened by one server at a time",
            )),
            Err(std::fs::TryLockError::Error(e)) => Err(RuntimeFailure::new(FailureKind::StartupFailed, format!("cannot lock workbook: {e}"))),
        }
    }

    fn start(&self, req: &EnsureRequest) -> std::result::Result<Arc<WorkbookHandle>, RuntimeFailure> {
        let startup = |e: anyhow::Error| {
            let kind = if e.downcast_ref::<TooNew>().is_some() { FailureKind::Incompatible } else { FailureKind::StartupFailed };
            let wrong = e.downcast_ref::<WrongWorkbook>().is_some();
            RuntimeFailure::new(
                kind,
                if wrong { format!("{e:#}; the workbook's files do not match its id") } else { format!("could not open the workbook: {e:#}") },
            )
        };
        let handed = req.handoff.lock().unwrap().take();
        let lock = match handed {
            Some(lock) => lock,
            None => Self::lock(req.workbook_dir)?,
        };
        let temp = self.temp_dir(req.workbook);
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).map_err(|e| startup(e.into()))?;
        let result = (|| {
            crate::state::fault_at("open_workbook", req.workbook_dir).map_err(startup)?;
            let engine = Workbook::open(&req.workbook_dir.join("data.duckdb")).map_err(startup)?;
            if let Some(from) = engine.open_report().migrated_from {
                tracing::info!(workbook = req.workbook, "migrated storage format {from} → {}", alephgrid_core::storage::FORMAT);
            }
            engine.bind_identity(req.workbook).map_err(startup)?;
            (req.checkpoint)(AllocationState::Running)?;
            let settings = vec![
                ("memory_limit", format!("{}B", req.engine_memory_bytes)),
                ("threads", req.reservation.threads.max(1).to_string()),
                ("temp_directory", temp.to_string_lossy().to_string()),
                ("max_temp_directory_size", format!("{}B", req.reservation.temp_disk_bytes.max(1 << 20))),
            ];
            engine.configure(&settings).map_err(startup)?;
            // The database may read and write only this workbook's scratch directories; user
            // SQL cannot reach files, URLs or extensions (4.07).
            let data_dir = req.workbook_dir.parent().unwrap_or(req.workbook_dir);
            let dirs = crate::state::allowed_dirs(data_dir, req.workbook, &temp).map_err(startup)?;
            engine.restrict_access(&dirs.iter().map(|d| d.as_path()).collect::<Vec<_>>()).map_err(startup)?;
            // Health check before serving.
            engine.revision().map_err(startup)?;
            let ownership = OwnershipAssignment {
                workbook: req.workbook.to_string(),
                allocation: req.allocation.id.clone(),
                epoch: req.allocation.epoch,
                holder: Self::holder(),
                acquired_at_ms: alephgrid_core::storage::now_ms(),
                mechanism: "flock".into(),
            };
            super::write_durably(&req.workbook_dir.join("owner.json"), &serde_json::to_vec_pretty(&ownership).unwrap())
                .map_err(startup)?;
            WorkbookHandle::new(req.meta.clone(), req.workbook_dir.to_path_buf(), engine, READERS, lock, temp.clone(), ownership)
                .map_err(startup)
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&temp);
        }
        result.map(Arc::new)
    }
}

impl RuntimeProvider for LocalProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            kind: ProviderKind::Local,
            max_storage_format: alephgrid_core::storage::FORMAT,
            capabilities: [Capability::Tables, Capability::Sheets, Capability::Sql].into_iter().collect(),
            isolation: vec![IsolationClass::SharedProcess],
        }
    }

    fn ensure(&self, req: &EnsureRequest) -> std::result::Result<Arc<WorkbookHandle>, RuntimeFailure> {
        (req.checkpoint)(AllocationState::Starting)?;
        self.start(req)
    }

    fn drain(&self, handle: &WorkbookHandle, deadline: Duration) -> Result<()> {
        handle.drain(deadline)
    }

    fn stop(&self, handle: &WorkbookHandle) -> Result<()> {
        handle.close()?;
        let _ = std::fs::remove_dir_all(self.temp_dir(&handle.id));
        Ok(())
    }

    fn reconcile(&self, record: &super::RuntimeAllocation, workbook_dir: &Path) -> Result<bool> {
        if !workbook_dir.exists() {
            let _ = std::fs::remove_dir_all(self.temp_dir(&record.workbook));
            return Ok(false);
        }
        match Self::lock(workbook_dir) {
            Ok(lock) => {
                // Nobody holds it: the allocation died with its process. Remove its scratch.
                drop(lock);
                std::fs::remove_dir_all(self.temp_dir(&record.workbook))
                    .or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) })
                    .context("removing a dead allocation's temp directory")?;
                Ok(false)
            }
            Err(f) if f.kind == FailureKind::OwnedElsewhere => Ok(true),
            Err(f) => Err(anyhow::anyhow!(f)),
        }
    }
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_string()))
        .filter(|h| !h.is_empty())
        .or_else(|| {
            std::process::Command::new("hostname").output().ok().and_then(|o| String::from_utf8(o.stdout).ok()).map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "localhost".into())
}
