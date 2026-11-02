//! Consistent backups and restore (3.08).
//!
//! A backup is a directory:
//!
//! ```text
//! manifest.json              format, time, version, the control database's SHA-256 and
//!                            per workbook: revision, storage format, size and SHA-256 of
//!                            its database file
//! control.duckdb             organizations, users, spaces, roles, tokens and audit log
//! <workbook-id>/workbook.json
//! <workbook-id>/data.duckdb
//! ```
//!
//! **Online** (`POST /api/admin/backups`), each workbook is copied by a reader connection
//! with `COPY FROM DATABASE` inside one transaction, so the copy holds exactly the state of
//! one revision while edits continue. Workbooks are copied one after another: the backup
//! is consistent per workbook, not across workbooks (they are independent). **Offline**
//! (`alephgrid --backup-to`) refuses to run while a server holds the data directory, then
//! checkpoints and copies each file.
//!
//! **Restore** (`alephgrid --restore-from`) only writes into an empty or missing data
//! directory. It verifies every checksum first, copies into a staging directory, opens each
//! restored workbook (which also migrates an older format) and checks its revision and
//! identity against the manifest, and only then moves the staging directory into place.
//! Trashed workbooks, scratch files, runtime records and sign-in sessions' liveness are not
//! part of a backup (restored sessions still expire on time). A format-1 backup (before
//! spaces) carries `workspaces.json` instead of the control database; restoring it lets the
//! next start migrate its workspaces to spaces.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::state::{read_meta, Shared, WorkbookMeta, DB_FILE, META_FILE};

pub const BACKUP_FORMAT: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct BackupManifest {
    pub format: u32,
    pub created_at_ms: u64,
    pub server_version: String,
    /// SHA-256 of `control.duckdb` (format 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub control_sha256: Option<String>,
    pub workbooks: Vec<BackupEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct BackupEntry {
    pub id: String,
    pub name: String,
    /// Revision the copy holds.
    pub revision: u64,
    pub storage_format: u32,
    pub bytes: u64,
    pub sha256: String,
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(crate::runtime::hex(&h.finalize()))
}

fn entry(meta: &WorkbookMeta, db: &Path, revision: u64) -> Result<BackupEntry> {
    let conn = duckdb::Connection::open(db)?;
    let storage_format = alephgrid_core::storage::format_of(&conn)?;
    drop(conn);
    Ok(BackupEntry {
        id: meta.id.clone(),
        name: meta.name.clone(),
        revision,
        storage_format,
        bytes: std::fs::metadata(db)?.len(),
        sha256: sha256_file(db)?,
    })
}

/// Copy the control database into the backup, from one transaction.
fn control(store: &crate::auth::Store, dest: &Path) -> Result<String> {
    let path = dest.join("control.duckdb");
    store.backup_to(&path)?;
    sha256_file(&path)
}

fn finish(dest: &Path, workbooks: Vec<BackupEntry>, control_sha256: String) -> Result<BackupManifest> {
    let manifest = BackupManifest {
        format: BACKUP_FORMAT,
        created_at_ms: alephgrid_core::storage::now_ms(),
        server_version: env!("CARGO_PKG_VERSION").into(),
        control_sha256: Some(control_sha256),
        workbooks,
    };
    // Written last: a backup without a manifest is incomplete and is never restored.
    crate::runtime::write_durably(&dest.join("manifest.json"), &serde_json::to_vec_pretty(&manifest)?)?;
    Ok(manifest)
}

fn prepare(dest: &Path) -> Result<()> {
    if dest.exists() && std::fs::read_dir(dest)?.next().is_some() {
        bail!("{} is not empty; back up into a new directory", dest.display());
    }
    std::fs::create_dir_all(dest)?;
    Ok(())
}

/// Back up every workbook of a running server, each from one snapshot.
pub fn backup_online(state: &Shared, dest: &Path) -> Result<BackupManifest> {
    prepare(dest)?;
    let result = (|| {
        let mut entries = Vec::new();
        for listing in state.list()? {
            let id = listing.meta.id.clone();
            let handle = state.get(&id)?;
            let dir = dest.join(&id);
            std::fs::create_dir_all(&dir)?;
            let db = dir.join(DB_FILE);
            let revision = state.snapshot_to(&handle, &id, &db)?;
            let meta = handle.meta.lock().unwrap().clone();
            crate::state::write_meta(&dir, &meta)?;
            entries.push(entry(&meta, &db, revision)?);
        }
        // After the workbooks: every space a copied workbook names is in this copy.
        let sum = control(&state.auth.store, dest)?;
        finish(dest, entries, sum)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(dest);
    }
    result
}

/// Back up a data directory no server is using.
pub fn backup_offline(data_dir: &Path, dest: &Path) -> Result<BackupManifest> {
    let control = data_dir.join(".control");
    std::fs::create_dir_all(&control)?;
    let lock = std::fs::File::create(control.join("controller.lock"))?;
    if lock.try_lock().is_err() {
        bail!("a Alephgrid server is using {}; use its online backup (POST /api/admin/backups) or stop it first", data_dir.display());
    }
    prepare(dest)?;
    let result = (|| {
        let mut entries = Vec::new();
        for e in std::fs::read_dir(data_dir)? {
            let src = e?.path();
            if crate::state::is_hidden(&src) || !src.join(META_FILE).exists() {
                continue;
            }
            let meta = read_meta(&src)?;
            let dir = dest.join(&meta.id);
            std::fs::create_dir_all(&dir)?;
            let db = dir.join(DB_FILE);
            let revision = {
                let conn = duckdb::Connection::open(src.join(DB_FILE))?;
                conn.execute_batch("CHECKPOINT;")?;
                let rev = if alephgrid_core::storage::format_of(&conn)? >= 2 { alephgrid_core::storage::revision(&conn)? } else { 0 };
                drop(conn);
                rev
            };
            alephgrid_core::storage::copy_durably(&src.join(DB_FILE), &db)?;
            alephgrid_core::storage::copy_durably(&src.join(META_FILE), &dir.join(META_FILE))?;
            entries.push(entry(&meta, &db, revision)?);
        }
        let store = crate::auth::Store::open(&control.join("control.duckdb"))?;
        crate::state::migrate_to_spaces(data_dir, &store)?;
        let sum = self::control(&store, dest)?;
        drop(store);
        finish(dest, entries, sum)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(dest);
    }
    result
}

/// Restore a backup into an empty (or missing) data directory. Writes nothing unless
/// every check passes.
pub fn restore(src: &Path, data_dir: &Path) -> Result<BackupManifest> {
    let manifest: BackupManifest = serde_json::from_slice(
        &std::fs::read(src.join("manifest.json")).with_context(|| format!("{} has no manifest.json; it is not a complete backup", src.display()))?,
    )
    .context("unreadable manifest")?;
    if !(1..=BACKUP_FORMAT).contains(&manifest.format) {
        bail!("backup format {} is not supported (this version reads 1 to {BACKUP_FORMAT})", manifest.format);
    }
    if manifest.format >= 2 {
        let expected = manifest.control_sha256.as_deref().context("the manifest has no checksum for control.duckdb")?;
        let sum = sha256_file(&src.join("control.duckdb"))?;
        if sum != expected {
            bail!("the backup's control database is damaged: checksum {sum} does not match {expected}");
        }
    }
    if data_dir.exists() && std::fs::read_dir(data_dir)?.next().is_some() {
        bail!("{} is not empty; restore only into an empty data directory", data_dir.display());
    }
    // 1. Every file is exactly what was backed up.
    for e in &manifest.workbooks {
        if !crate::state::valid_id(&e.id) {
            bail!("invalid workbook id {:?} in the manifest", e.id);
        }
        let db = src.join(&e.id).join(DB_FILE);
        let sum = sha256_file(&db)?;
        if sum != e.sha256 {
            bail!("the backup of workbook {} ({}) is damaged: checksum {sum} does not match {}", e.id, e.name, e.sha256);
        }
        if e.storage_format > alephgrid_core::storage::FORMAT {
            bail!("workbook {} needs a newer Alephgrid (storage format {})", e.id, e.storage_format);
        }
    }
    // 2. Copy into a staging directory next to the target, then check each workbook opens
    //    at the revision the manifest says, bound to its own id.
    let parent = data_dir.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&parent)?;
    let staging = parent.join(format!(".alephgrid-restore-{}", alephgrid_core::storage::now_ms()));
    let result = (|| -> Result<()> {
        // A new directory of its own: create_dir fails if anything is already there (SEC.14).
        std::fs::create_dir(&staging).with_context(|| format!("cannot create the staging directory {}", staging.display()))?;
        for e in &manifest.workbooks {
            for f in [DB_FILE, META_FILE] {
                let from = src.join(&e.id).join(f);
                // Only plain files are restored: a link could point anywhere (SEC.14).
                if std::fs::symlink_metadata(&from).map(|m| !m.file_type().is_file()).unwrap_or(true) {
                    bail!("workbook {}'s {f} in the backup is not a plain file", e.id);
                }
            }
            let to = staging.join(&e.id);
            std::fs::create_dir_all(&to)?;
            alephgrid_core::storage::copy_durably(&src.join(&e.id).join(DB_FILE), &to.join(DB_FILE))?;
            alephgrid_core::storage::copy_durably(&src.join(&e.id).join(META_FILE), &to.join(META_FILE))?;
            // workbook.json must name the workbook the manifest and the database are bound to (SEC.02).
            let meta: WorkbookMeta = serde_json::from_slice(&std::fs::read(to.join(META_FILE))?)
                .with_context(|| format!("workbook {} has an unreadable workbook.json", e.id))?;
            if meta.id != e.id {
                bail!("workbook {}'s workbook.json names a different workbook ({:?})", e.id, meta.id);
            }
            // Opened with the engine locked down: nothing in the file runs with file or network access (SEC.01).
            let wb = alephgrid_core::Workbook::open_locked(&to.join(DB_FILE), &alephgrid_core::storage::Lockdown::default())
                .with_context(|| format!("restored workbook {} does not open", e.id))?;
            let revision = wb.revision()?;
            if revision != e.revision {
                bail!("restored workbook {} is at revision {revision}, the manifest says {}", e.id, e.revision);
            }
            wb.bind_identity(&e.id)?;
            wb.checkpoint()?;
            drop(wb);
            // A migration during the check leaves a pre-migration copy; it belongs to the backup, not the restore.
            for v in 1..alephgrid_core::storage::FORMAT {
                let _ = std::fs::remove_file(to.join(format!("data.duckdb.v{v}.bak")));
            }
        }
        if manifest.format >= 2 {
            std::fs::create_dir_all(staging.join(".control"))?;
            alephgrid_core::storage::copy_durably(&src.join("control.duckdb"), &staging.join(".control").join("control.duckdb"))?;
            crate::auth::Store::open(&staging.join(".control").join("control.duckdb")).context("the restored control database does not open")?;
        } else if src.join("workspaces.json").exists() {
            alephgrid_core::storage::copy_durably(&src.join("workspaces.json"), &staging.join("workspaces.json"))?;
        }
        if data_dir.exists() {
            std::fs::remove_dir(data_dir)?; // empty, checked above
        }
        std::fs::rename(&staging, data_dir).with_context(|| format!("moving the restored data into {}", data_dir.display()))?;
        alephgrid_core::storage::sync_dir(&parent);
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    Ok(manifest)
}

/// Free bytes on the file system holding `dir`, when the platform says.
pub fn free_bytes(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a valid C string and `st` a writable statvfs.
    let rc = unsafe { libc::statvfs(path.as_ptr(), &mut st) };
    (rc == 0).then(|| st.f_bavail as u64 * st.f_frsize as u64)
}

/// An online backup on disk.
#[derive(Debug, Clone, Serialize)]
pub struct BackupListing {
    pub name: String,
    pub created_at_ms: u64,
    pub workbooks: usize,
    pub bytes: u64,
}

/// The complete backups under `root`, newest first.
pub fn list(root: &Path) -> Result<Vec<BackupListing>> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else { return Ok(out) };
    for e in entries {
        let dir = e?.path();
        let name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let Some(ms) = name.strip_prefix("backup-").and_then(|m| m.parse::<u64>().ok()) else { continue };
        let Ok(bytes) = std::fs::read(dir.join("manifest.json")) else { continue };
        let Ok(manifest) = serde_json::from_slice::<BackupManifest>(&bytes) else { continue };
        let size = manifest.workbooks.iter().map(|w| w.bytes).sum();
        out.push(BackupListing { name, created_at_ms: ms, workbooks: manifest.workbooks.len(), bytes: size });
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.created_at_ms));
    Ok(out)
}

/// Remove all but the newest `keep` complete backups under `root` (and any incomplete one
/// older than the newest). Returns how many were removed.
pub fn prune(root: &Path, keep: usize) -> Result<usize> {
    let all = list(root)?;
    let mut removed = 0;
    for b in all.iter().skip(keep.max(1)) {
        std::fs::remove_dir_all(root.join(&b.name))?;
        removed += 1;
    }
    Ok(removed)
}
