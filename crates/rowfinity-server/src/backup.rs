//! Consistent backups and restore (3.08).
//!
//! A backup is a directory:
//!
//! ```text
//! manifest.json              format, time, version, and per workbook: revision, storage
//!                            format, size and SHA-256 of its database file
//! workspaces.json            (if any)
//! <workbook-id>/workbook.json
//! <workbook-id>/data.duckdb
//! ```
//!
//! **Online** (`POST /api/admin/backups`), each workbook is copied by a reader connection
//! with `COPY FROM DATABASE` inside one transaction, so the copy holds exactly the state of
//! one revision while edits continue. Workbooks are copied one after another: the backup
//! is consistent per workbook, not across workbooks (they are independent). **Offline**
//! (`rowfinity --backup-to`) refuses to run while a server holds the data directory, then
//! checkpoints and copies each file.
//!
//! **Restore** (`rowfinity --restore-from`) only writes into an empty or missing data
//! directory. It verifies every checksum first, copies into a staging directory, opens each
//! restored workbook (which also migrates an older format) and checks its revision and
//! identity against the manifest, and only then moves the staging directory into place.
//! Trashed workbooks, scratch files and runtime records are not part of a backup.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::state::{read_meta, Shared, WorkbookMeta};

pub const BACKUP_FORMAT: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct BackupManifest {
    pub format: u32,
    pub created_at_ms: u64,
    pub server_version: String,
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
    let storage_format = rowfinity_engine::storage::format_of(&conn)?;
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

fn finish(dest: &Path, workbooks: Vec<BackupEntry>, workspaces: Option<PathBuf>) -> Result<BackupManifest> {
    if let Some(ws) = workspaces.filter(|p| p.exists()) {
        rowfinity_engine::storage::copy_durably(&ws, &dest.join("workspaces.json"))?;
    }
    let manifest = BackupManifest {
        format: BACKUP_FORMAT,
        created_at_ms: rowfinity_engine::storage::now_ms(),
        server_version: env!("CARGO_PKG_VERSION").into(),
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
            let db = dir.join("data.duckdb");
            // The workbook's database may only write its own scratch directories (4.07):
            // copy there, then move the file into the backup.
            let scratch = state.scratch_dir(".exports", &id)?;
            let staged = scratch.join("data.duckdb");
            let copied = handle.with_reader(|r| r.backup_to(&staged)).and_then(|rev| {
                std::fs::rename(&staged, &db)?;
                Ok(rev)
            });
            let _ = std::fs::remove_dir_all(&scratch);
            let revision = copied?;
            let meta = handle.meta.lock().unwrap().clone();
            crate::runtime::write_durably(&dir.join("workbook.json"), &serde_json::to_vec_pretty(&meta)?)?;
            entries.push(entry(&meta, &db, revision)?);
        }
        finish(dest, entries, Some(state.config.data_dir.join("workspaces.json")))
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
        bail!("a Rowfinity server is using {}; use its online backup (POST /api/admin/backups) or stop it first", data_dir.display());
    }
    prepare(dest)?;
    let result = (|| {
        let mut entries = Vec::new();
        for e in std::fs::read_dir(data_dir)? {
            let src = e?.path();
            if src.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) || !src.join("workbook.json").exists() {
                continue;
            }
            let meta = read_meta(&src)?;
            let dir = dest.join(&meta.id);
            std::fs::create_dir_all(&dir)?;
            let db = dir.join("data.duckdb");
            let revision = {
                let conn = duckdb::Connection::open(src.join("data.duckdb"))?;
                conn.execute_batch("CHECKPOINT;")?;
                let rev = if rowfinity_engine::storage::format_of(&conn)? >= 2 { rowfinity_engine::storage::revision(&conn)? } else { 0 };
                drop(conn);
                rev
            };
            rowfinity_engine::storage::copy_durably(&src.join("data.duckdb"), &db)?;
            rowfinity_engine::storage::copy_durably(&src.join("workbook.json"), &dir.join("workbook.json"))?;
            entries.push(entry(&meta, &db, revision)?);
        }
        finish(dest, entries, Some(data_dir.join("workspaces.json")))
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
    if manifest.format != BACKUP_FORMAT {
        bail!("backup format {} is not supported (this version reads {BACKUP_FORMAT})", manifest.format);
    }
    if data_dir.exists() && std::fs::read_dir(data_dir)?.next().is_some() {
        bail!("{} is not empty; restore only into an empty data directory", data_dir.display());
    }
    // 1. Every file is exactly what was backed up.
    for e in &manifest.workbooks {
        if !crate::state::valid_id(&e.id) {
            bail!("invalid workbook id {:?} in the manifest", e.id);
        }
        let db = src.join(&e.id).join("data.duckdb");
        let sum = sha256_file(&db)?;
        if sum != e.sha256 {
            bail!("the backup of workbook {} ({}) is damaged: checksum {sum} does not match {}", e.id, e.name, e.sha256);
        }
        if e.storage_format > rowfinity_engine::storage::FORMAT {
            bail!("workbook {} needs a newer Rowfinity (storage format {})", e.id, e.storage_format);
        }
    }
    // 2. Copy into a staging directory next to the target, then check each workbook opens
    //    at the revision the manifest says, bound to its own id.
    let parent = data_dir.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&parent)?;
    let staging = parent.join(format!(".rowfinity-restore-{}", rowfinity_engine::storage::now_ms()));
    let result = (|| -> Result<()> {
        std::fs::create_dir_all(&staging)?;
        for e in &manifest.workbooks {
            let to = staging.join(&e.id);
            std::fs::create_dir_all(&to)?;
            rowfinity_engine::storage::copy_durably(&src.join(&e.id).join("data.duckdb"), &to.join("data.duckdb"))?;
            rowfinity_engine::storage::copy_durably(&src.join(&e.id).join("workbook.json"), &to.join("workbook.json"))?;
            let wb = rowfinity_engine::Workbook::open(&to.join("data.duckdb")).with_context(|| format!("restored workbook {} does not open", e.id))?;
            let revision = wb.revision()?;
            if revision != e.revision {
                bail!("restored workbook {} is at revision {revision}, the manifest says {}", e.id, e.revision);
            }
            wb.bind_identity(&e.id)?;
            wb.checkpoint()?;
            drop(wb);
            // A migration during the check leaves a pre-migration copy; it belongs to the backup, not the restore.
            for v in 1..rowfinity_engine::storage::FORMAT {
                let _ = std::fs::remove_file(to.join(format!("data.duckdb.v{v}.bak")));
            }
        }
        if src.join("workspaces.json").exists() {
            rowfinity_engine::storage::copy_durably(&src.join("workspaces.json"), &staging.join("workspaces.json"))?;
        }
        if data_dir.exists() {
            std::fs::remove_dir(data_dir)?; // empty, checked above
        }
        std::fs::rename(&staging, data_dir).with_context(|| format!("moving the restored data into {}", data_dir.display()))?;
        rowfinity_engine::storage::sync_dir(&parent);
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    Ok(manifest)
}
