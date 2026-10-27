//! The nonblocking, process-shared dispatch guard of one pool in one
//! mailspace.
//!
//! At most one dispatch of a pool may run at a time in a mailspace on one
//! machine: a dispatch that resolved [`super::runtime::Decision::Ready`]
//! takes the guard with [`DispatchGuard::try_acquire`] and holds it until
//! it is dropped, spanning recording and rollback. The guard is an
//! advisory file lock, so exclusion holds between processes, not merely
//! threads; contention is answered immediately as [`None`] and never
//! waited on — the caller decides what busy means. Locks of different
//! pools and of different mailspaces live in different files and never
//! block each other.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::VivariumError;

/// Held by the one dispatch of a pool that may run in a mailspace on this
/// machine; released when dropped.
#[derive(Debug)]
#[must_use]
pub struct DispatchGuard {
    _file: File,
}

impl DispatchGuard {
    /// Try to take the dispatch guard of `pool` beneath the mailspace
    /// local-state directory `mailspace_dir`.
    ///
    /// `Ok(Some(guard))` lets the dispatch proceed and holds the lock until
    /// the guard is dropped. `Ok(None)` means another process is already
    /// dispatching this pool in this mailspace; the answer is immediate and
    /// nothing is acquired. `Err` is a genuine I/O failure reaching or
    /// locking the file, not contention.
    ///
    /// # Errors
    /// Returns an error when the lock file cannot be created, opened, or
    /// locked by this process for a reason other than contention.
    pub fn try_acquire(
        mailspace_dir: &Path,
        pool: &str,
    ) -> Result<Option<DispatchGuard>, VivariumError> {
        let path = lock_path(mailspace_dir, pool);
        let io = |e: std::io::Error| {
            VivariumError::Message(format!("cannot lock {}: {e}", path.display()))
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(DispatchGuard { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(io(e)),
        }
    }
}

/// The lock file of `pool` below the mailspace local-state directory.
///
/// The pool name is hashed into the file name rather than quoted: every
/// pool name maps to one stable, collision-safe path, so names that a
/// filesystem would fold together (`a.b` versus `a/b`, case variants)
/// cannot collide or escape the directory.
fn lock_path(mailspace_dir: &Path, pool: &str) -> PathBuf {
    let digest = hex::encode(Sha256::digest(pool.as_bytes()));
    mailspace_dir
        .join("locks")
        .join("dispatch")
        .join(format!("{digest}.lock"))
}

#[cfg(test)]
#[path = "guard_test.rs"]
mod tests;
