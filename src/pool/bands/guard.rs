//! The nonblocking dispatch guard of a pool: one advisory file lock per
//! (mailspace, pool) pair on this machine, taken once and never waited on.
//!
//! A dispatch holds [`DispatchGuard`] for its whole run; a second dispatch
//! of the same pool in the same mailspace is told at once (`Ok(None)`
//! means contention, never a wait). The lock file lives under the
//! mailspace's local-state directory and is named by a hash of the pool
//! name, so any pool name — with slashes, odd case, or any length — maps
//! to one stable, collision-safe file. Different pools and different
//! mailspaces therefore never block each other. The file is created once
//! and never unlinked, so a crashed holder's lock is released by the OS
//! and the next dispatch reuses the same file.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::VivariumError;

/// Held for the length of one pool dispatch on this machine; dropping the
/// guard releases the advisory lock.
#[derive(Debug)]
pub struct DispatchGuard {
    _file: File,
}

impl DispatchGuard {
    /// Take this pool's dispatch guard below the mailspace local-state
    /// directory `mailspace_dir`, or answer `Ok(None)` when another holder
    /// on this machine already has it. Independent pools and mailspaces
    /// never contend: each (mailspace, pool) pair locks its own file.
    ///
    /// # Errors
    /// Returns an error when the lock file cannot be created or the lock
    /// cannot be probed. Contention is `Ok(None)`, not an error.
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
            // Contention is an answer, not a failure; our unused handle is
            // dropped here and the holder's lock is untouched.
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(io(e)),
        }
    }
}

/// The dispatch lock file of `pool` below the mailspace's local-state
/// directory: the pool name is hashed, so every name maps to one stable,
/// collision-safe file name.
#[must_use]
fn lock_path(mailspace_dir: &Path, pool: &str) -> PathBuf {
    let key = hex::encode(Sha256::digest(pool.as_bytes()));
    mailspace_dir
        .join("locks")
        .join(format!("dispatch-{key}.lock"))
}

#[cfg(test)]
#[path = "guard_test.rs"]
mod tests;
