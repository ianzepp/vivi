//! A nonblocking cap-admission lock for one pool of one mailspace on this
//! machine, under the mailspace's local state.
//!
//! This is only lock machinery: nothing here decides whether a pool needs
//! the lock or holds it across submission — a later slice does. Contention
//! is an answer, not an error: [`try_acquire`] returns `None` when another
//! holder is in the way and never waits. The mechanics follow
//! [`runpod_lock`](super::super::runpod_lock), but the path lives below the
//! mailspace's local state directory, keyed by the mailspace identity and
//! the pool, never under a provider slot directory.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use crate::error::VivariumError;
use crate::pool::slot::PoolId;

/// Held while a cap admission is in flight; released when dropped.
#[derive(Debug)]
pub struct DispatchLock {
    _file: File,
}

/// The stable lock path for pool `id` below the mailspace's local state
/// directory, keyed by both the mailspace identity and the pool.
#[must_use]
pub fn path_for(local_state_dir: &Path, id: &PoolId) -> PathBuf {
    local_state_dir
        .join("locks")
        .join(format!("{}.{}.lock", id.mailspace, id.pool))
}

/// Take the cap-admission lock at `path` without waiting: `Some` when this
/// caller holds it, `None` when another holder does. The lock file is
/// stable — created when missing, never truncated, never unlinked — and the
/// advisory lock releases when the returned [`DispatchLock`] drops or the
/// process exits.
///
/// # Errors
/// Returns an error for real filesystem or locking failures, such as a
/// parent that exists as a file or a path that cannot be opened.
pub fn try_acquire(path: &Path) -> Result<Option<DispatchLock>, VivariumError> {
    let io =
        |e: std::io::Error| VivariumError::Message(format!("cannot lock {}: {e}", path.display()));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(io)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(DispatchLock { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(io(e)),
    }
}

#[cfg(test)]
#[path = "admission_test.rs"]
mod tests;
