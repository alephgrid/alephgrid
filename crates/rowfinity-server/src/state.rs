//! Workbook and workspace registry.
//!
//! ```text
//! <data-dir>/workspaces.json          [{id, name, created_at}]
//! <data-dir>/<workbook-id>/workbook.json
//! <data-dir>/<workbook-id>/data.duckdb  tables, formula metadata and IronCalc sheets
//! <data-dir>/.trash/<workbook-id>/       deleted workbooks (same layout + deleted_at), restorable
//! <data-dir>/.uploads/<random>/<file>    uploads in progress; removed after import or failure
//! <data-dir>/.exports/<random>/<file>    exports being streamed; removed once opened
//! ```
//!
//! Deleting a workbook first marks its handle deleted (new operations fail), then waits
//! for the operation in progress, closes the database, and moves the directory to the
//! trash with one rename. Nothing is permanently removed until the trash is purged.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use rowfinity_engine::Workbook;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use ts_rs::TS;

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

pub struct WorkbookHandle {
    pub meta: Mutex<WorkbookMeta>,
    /// Read the progress of, or interrupt, the statement running now; usable without
    /// the engine lock (which that statement holds).
    pub progress: duckdb_handles::ProgressHandle,
    pub interrupt: std::sync::Arc<duckdb_handles::InterruptHandle>,
    /// Client-chosen id of the long job running now (an import), for progress and cancel.
    pub job: Mutex<Option<String>>,
    /// `None` once the workbook has been deleted and its database closed.
    engine: Mutex<Option<Workbook>>,
    deleted: AtomicBool,
    pub events: broadcast::Sender<Event>,
    pub dir: PathBuf,
}

impl WorkbookHandle {
    /// Run `f` with exclusive access to the workbook engine. Fails once the workbook
    /// has been deleted, including for operations that were waiting for the lock.
    pub fn with_engine<T>(&self, f: impl FnOnce(&mut Workbook) -> Result<T>) -> Result<T> {
        if self.is_deleted() {
            bail!("this workbook was deleted");
        }
        let mut guard = self.engine.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
            Some(wb) => f(wb),
            None => bail!("this workbook was deleted"),
        }
    }

    pub fn is_deleted(&self) -> bool {
        self.deleted.load(Ordering::SeqCst)
    }

    /// Reject new work, wait for the operation in progress, then close the database.
    fn close(&self) {
        self.deleted.store(true, Ordering::SeqCst);
        let mut guard = self.engine.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(wb) = guard.take() {
            if let Err(e) = wb.checkpoint() {
                tracing::warn!("checkpoint before close failed: {e:#}");
            }
        }
    }

    /// Recompute counts shown on the home screen and bump `updated_at`.
    pub fn refresh_summary(&self) -> Result<()> {
        let summary = self.with_engine(|wb| {
            let tables = wb.list_tables()?;
            Ok(Summary {
                tables: tables.len(),
                sheets: wb.sheets().list().len(),
                rows: tables.iter().map(|t| t.row_count).sum(),
            })
        })?;
        let mut meta = self.meta.lock().unwrap();
        meta.summary = summary;
        meta.updated_at = now();
        write_meta(&self.dir, &meta)
    }

    pub fn touch(&self, opened: bool) -> Result<()> {
        if self.is_deleted() {
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

pub struct Config {
    pub data_dir: PathBuf,
    pub token: Option<String>,
    pub allow_sql: bool,
    pub memory_limit: Option<String>,
    pub threads: Option<usize>,
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
pub const UPLOAD_TTL: std::time::Duration = std::time::Duration::from_secs(3600);

pub struct AppState {
    pub config: Config,
    pub uploads: Mutex<HashMap<String, Upload>>,
    open: Mutex<HashMap<String, Arc<WorkbookHandle>>>,
    /// Workbooks being deleted: their database may still be open, so they cannot be reopened.
    closing: Mutex<HashSet<String>>,
    workspaces: Mutex<()>,
    next_session: AtomicU64,
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

fn valid_id(id: &str) -> bool {
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

impl AppState {
    pub fn new(config: Config) -> Result<Shared> {
        std::fs::create_dir_all(&config.data_dir)
            .with_context(|| format!("creating data dir {}", config.data_dir.display()))?;
        // Leftovers of uploads/exports interrupted by a crash; never user data.
        for scratch in [".uploads", ".exports"] {
            let dir = config.data_dir.join(scratch);
            if dir.exists() {
                std::fs::remove_dir_all(&dir).with_context(|| format!("cleaning {}", dir.display()))?;
            }
        }
        Ok(Arc::new(AppState {
            config,
            open: Mutex::new(HashMap::new()),
            uploads: Mutex::new(HashMap::new()),
            closing: Mutex::new(HashSet::new()),
            workspaces: Mutex::new(()),
            next_session: AtomicU64::new(1),
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
        let open = self.open.lock().unwrap();
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.config.data_dir)? {
            let dir = entry?.path();
            if dir.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
                continue;
            }
            let Ok(bytes) = std::fs::read(dir.join("workbook.json")) else { continue };
            let Ok(mut meta) = serde_json::from_slice::<WorkbookMeta>(&bytes) else { continue };
            if let Some(h) = open.get(&meta.id) {
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
        };
        write_meta(&dir, &meta)?;
        self.get(&id)
    }

    pub fn get(&self, id: &str) -> Result<Arc<WorkbookHandle>> {
        if !valid_id(id) {
            bail!("invalid workbook id");
        }
        let mut open = self.open.lock().unwrap();
        if let Some(h) = open.get(id) {
            return Ok(h.clone());
        }
        if self.closing.lock().unwrap().contains(id) {
            bail!("workbook {id} is being deleted");
        }
        let dir = self.workbook_dir(id);
        let meta: WorkbookMeta = serde_json::from_slice(
            &std::fs::read(dir.join("workbook.json")).with_context(|| format!("workbook {id} not found"))?,
        )?;
        let engine = Workbook::open(&dir.join("data.duckdb"))?;
        let mut settings = Vec::new();
        if let Some(limit) = &self.config.memory_limit {
            settings.push(("memory_limit", limit.clone()));
        }
        if let Some(threads) = self.config.threads {
            settings.push(("threads", threads.to_string()));
        }
        engine.configure(&settings)?;
        let (events, _) = broadcast::channel(256);
        let handle = Arc::new(WorkbookHandle {
            progress: engine.progress_handle(),
            interrupt: engine.interrupt_handle(),
            job: Mutex::new(None),
            meta: Mutex::new(meta),
            engine: Mutex::new(Some(engine)),
            deleted: AtomicBool::new(false),
            events,
            dir,
        });
        open.insert(id.to_string(), handle.clone());
        Ok(handle)
    }

    /// Change metadata without opening the workbook's database.
    fn edit_meta(&self, id: &str, f: impl FnOnce(&mut WorkbookMeta) -> Result<()>) -> Result<WorkbookMeta> {
        if !valid_id(id) {
            bail!("invalid workbook id");
        }
        if let Some(h) = self.open.lock().unwrap().get(id).cloned() {
            let mut meta = h.meta.lock().unwrap();
            f(&mut meta)?;
            write_meta(&h.dir, &meta)?;
            return Ok(meta.clone());
        }
        let dir = self.workbook_dir(id);
        let mut meta: WorkbookMeta = serde_json::from_slice(
            &std::fs::read(dir.join("workbook.json")).with_context(|| format!("workbook {id} not found"))?,
        )?;
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

    /// Copy a workbook (tables, formulas and sheets) into a new one.
    pub fn duplicate(&self, id: &str) -> Result<WorkbookMeta> {
        let src = self.get(id)?;
        let src_meta = src.meta.lock().unwrap().clone();
        let new = new_id();
        let dir = self.workbook_dir(&new);
        std::fs::create_dir_all(&dir)?;
        // Hold the engine lock so nothing writes while the file is copied.
        let copied = src.with_engine(|wb| {
            wb.checkpoint()?;
            std::fs::copy(src.dir.join("data.duckdb"), dir.join("data.duckdb"))?;
            Ok(())
        });
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
        let handle = {
            let mut open = self.open.lock().unwrap();
            if !dir.join("workbook.json").exists() {
                bail!("workbook {id} not found");
            }
            let mut closing = self.closing.lock().unwrap();
            if !closing.insert(id.to_string()) {
                bail!("workbook {id} is already being deleted");
            }
            open.remove(id)
        };
        let result = self.move_to_trash(id, &dir, handle.as_deref());
        self.closing.lock().unwrap().remove(id);
        result?;
        if let Some(h) = handle {
            let _ = h.events.send(Event::WorkbookDeleted { origin: 0 });
        }
        Ok(())
    }

    fn move_to_trash(&self, id: &str, dir: &Path, handle: Option<&WorkbookHandle>) -> Result<()> {
        if let Some(h) = handle {
            h.close();
        }
        let trash = self.trash_dir();
        std::fs::create_dir_all(&trash)?;
        let target = trash.join(id);
        if target.exists() {
            std::fs::remove_dir_all(&target)?;
        }
        let mut meta: WorkbookMeta = serde_json::from_slice(&std::fs::read(dir.join("workbook.json"))?)?;
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
            let Ok(bytes) = std::fs::read(dir.join("workbook.json")) else { continue };
            let Ok(meta) = serde_json::from_slice::<WorkbookMeta>(&bytes) else { continue };
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
        let _open = self.open.lock().unwrap();
        let from = self.trash_dir().join(id);
        let to = self.workbook_dir(id);
        if !from.join("workbook.json").exists() {
            bail!("workbook {id} is not in the trash");
        }
        if to.exists() {
            bail!("a workbook with id {id} already exists");
        }
        let mut meta: WorkbookMeta = serde_json::from_slice(&std::fs::read(from.join("workbook.json"))?)?;
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

    /// Flush every open workbook to disk (called on shutdown).
    pub fn checkpoint_all(&self) {
        for h in self.open.lock().unwrap().values() {
            if let Err(e) = h.with_engine(|wb| wb.checkpoint()) {
                tracing::warn!("checkpoint failed: {e:#}");
            }
        }
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
        let tmp = self.workspaces_path().with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(list)?)?;
        std::fs::rename(tmp, self.workspaces_path())?;
        Ok(())
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

#[derive(Debug, Default, Deserialize, TS)]
#[ts(optional_fields)]
pub struct WorkbookPatch {
    pub name: Option<String>,
    /// Workspace id; an empty string moves the workbook out of any workspace.
    pub workspace: Option<String>,
    pub starred: Option<bool>,
}

fn write_meta(dir: &Path, meta: &WorkbookMeta) -> Result<()> {
    let tmp = dir.join("workbook.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(meta)?)?;
    std::fs::rename(tmp, dir.join("workbook.json"))?;
    Ok(())
}
