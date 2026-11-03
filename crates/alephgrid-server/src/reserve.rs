//! Disk kept back for when the disk fills (3.T03, docs/DURABILITY.md).
//!
//! A write that runs out of space is rolled back, but DuckDB keeps the blocks it had already
//! allocated in the file, so the disk stays full. Without space the server cannot record its
//! allocations, checkpoint or restart. It therefore keeps a reserve file of a few megabytes in
//! `.control/` and deletes it the moment a write fails for lack of space: that leaves room to
//! record state, close workbooks cleanly and start again, so an administrator can free space
//! from a working server. The reserve is recreated once the disk has room for it again.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct DiskReserve {
    path: PathBuf,
    bytes: u64,
    held: AtomicBool,
}

impl DiskReserve {
    /// A reserve of `bytes` at `<control>/reserve`; 0 turns it off. Nothing is written yet.
    pub fn new(control: &Path, bytes: u64) -> Arc<DiskReserve> {
        Arc::new(DiskReserve { path: control.join("reserve"), bytes, held: AtomicBool::new(false) })
    }

    /// A reserve that is never kept (tests, or `--disk-reserve 0`).
    pub fn disabled() -> Arc<DiskReserve> {
        Arc::new(DiskReserve { path: PathBuf::new(), bytes: 0, held: AtomicBool::new(false) })
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn held(&self) -> bool {
        self.held.load(Ordering::SeqCst)
    }

    /// Keep the reserve: create it when it is missing and the disk has room for it and twice
    /// as much again, so creating it never fills the disk. Returns whether it is held.
    pub fn ensure(&self) -> bool {
        if self.bytes == 0 {
            return false;
        }
        if self.held() {
            return true;
        }
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() == self.bytes) {
            self.held.store(true, Ordering::SeqCst);
            return true;
        }
        let Some(dir) = self.path.parent() else { return false };
        let free = crate::backup::free_bytes(dir).unwrap_or(0);
        if free < self.bytes.saturating_mul(3) {
            return false;
        }
        let tmp = self.path.with_extension("tmp");
        match write_filler(&tmp, self.bytes).and_then(|()| std::fs::rename(&tmp, &self.path).map_err(Into::into)) {
            Ok(()) => {
                alephgrid_core::storage::sync_dir(dir);
                self.held.store(true, Ordering::SeqCst);
                tracing::info!("keeping {} MiB of disk in reserve for a full disk", self.bytes >> 20);
                true
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                tracing::warn!("could not keep the disk reserve: {e:#}");
                false
            }
        }
    }

    /// The disk is full: give the reserve back. Returns whether there was one to give.
    pub fn release(&self) -> bool {
        if self.bytes == 0 {
            return false;
        }
        self.held.store(false, Ordering::SeqCst);
        let released = std::fs::remove_file(&self.path).is_ok();
        if released {
            tracing::warn!(
                "the disk is full: released the {} MiB reserve so the server can keep recording state; free space on the disk (the reserve comes back by itself)",
                self.bytes >> 20
            );
        }
        released
    }

    /// Release the reserve when `e` says the disk is full.
    pub fn note(&self, e: &anyhow::Error) {
        if is_disk_full(e) {
            self.release();
        }
    }
}

/// Whether an error comes from a full disk: an I/O error saying so, or DuckDB's own I/O
/// error (its root cause, which starts "IO Error"). Only the root cause is read, never the
/// messages around it, which may quote what a user typed (a table name, say): a request
/// must not be able to make the server give its reserve away.
pub fn is_disk_full(e: &anyhow::Error) -> bool {
    let io = e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>().is_some_and(|io| io.raw_os_error() == Some(libc::ENOSPC) || io.kind() == std::io::ErrorKind::StorageFull)
    });
    let root = e.root_cause().to_string();
    io || (root.starts_with("IO Error") && root.contains("No space left on device"))
}

/// Write `bytes` of data that file systems cannot compress or deduplicate away, so the reserve
/// really holds disk blocks.
fn write_filler(path: &Path, bytes: u64) -> anyhow::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    let mut chunk = vec![0u8; 1 << 20];
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15 ^ bytes;
    let mut left = bytes;
    while left > 0 {
        for b in chunk.chunks_mut(8) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            b.copy_from_slice(&x.to_le_bytes()[..b.len()]);
        }
        let n = left.min(chunk.len() as u64) as usize;
        f.write_all(&chunk[..n])?;
        left -= n as u64;
    }
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reserve_is_created_released_and_recreated() {
        let dir = tempfile::tempdir().unwrap();
        let r = DiskReserve::new(dir.path(), 2 << 20);
        assert!(r.ensure());
        assert_eq!(std::fs::metadata(dir.path().join("reserve")).unwrap().len(), 2 << 20);
        assert!(r.held());
        // A disk-full error releases it; another error does not.
        r.note(&anyhow::anyhow!("a parse error"));
        assert!(r.held());
        r.note(&anyhow::Error::new(std::io::Error::from_raw_os_error(libc::ENOSPC)));
        assert!(!r.held());
        assert!(!dir.path().join("reserve").exists());
        assert!(!r.release(), "nothing left to release");
        // It comes back when there is room, and a file of the right size is recognised.
        assert!(r.ensure());
        let again = DiskReserve::new(dir.path(), 2 << 20);
        assert!(again.ensure());
        assert!(!dir.path().join("reserve.tmp").exists());
    }

    #[test]
    fn duckdb_and_io_disk_full_errors_are_recognised() {
        assert!(is_disk_full(&anyhow::anyhow!("IO Error: Could not write file \"x\": No space left on device")));
        assert!(is_disk_full(&anyhow::Error::new(std::io::Error::from_raw_os_error(libc::ENOSPC)).context("could not record the allocation")));
        assert!(!is_disk_full(&anyhow::anyhow!("permission denied")));
        // Words a user chose are never a full disk, wherever they appear.
        assert!(!is_disk_full(&anyhow::anyhow!("unknown table \"No space left on device\"")));
        assert!(!is_disk_full(&anyhow::anyhow!("Catalog Error: Table with name IO Error No space left on device does not exist")));
        assert!(!is_disk_full(&anyhow::anyhow!("permission denied").context("IO Error: No space left on device")));
    }

    #[test]
    fn a_disabled_reserve_writes_nothing() {
        let r = DiskReserve::disabled();
        assert!(!r.ensure());
        assert!(!r.release());
    }
}
