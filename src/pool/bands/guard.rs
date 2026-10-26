//! A nonblocking admission guard between the capped admissions of one pool.
//!
//! A capped pool admits one dispatch at a time on a machine; concurrent
//! capped admissions of one pool take turns through this advisory file lock.
//! The guard is keyed by mailspace and pool identity alone — never by
//! repository target or selected model — so other pools and other mailspaces
//! never contend. A contender is not made to wait: it is answered
//! [`Admission::Deferred`] as [`DeferKind::DispatchBusy`] with no expiry,
//! and the runtime orchestration decides what to do with the deferral.
//! Uncapped pools and `--command` jobs need no guard at all.
//!
//! The mechanism is the nonblocking half of
//! [`crate::pool::runpod_lock`], which it deliberately mirrors: create the
//! lock file if needed (never truncating a live one), try to lock it, and
//! hold the open descriptor until the guard drops. Creation, open, and
//! locking failures are errors, not busy results.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use super::types::{Admission, DeferKind, Deferred};
use crate::error::VivariumError;

/// Held while a capped pool's admission session runs; released when dropped.
#[derive(Debug)]
pub struct DispatchGuard {
    _file: File,
}

/// The guard file of one pool below the mailspace's `.vivi` directory.
///
/// The file name carries both identities so two mailspaces pointed at one
/// directory still never share a guard.
#[must_use]
pub fn path_for(dir: &Path, mailspace: &str, pool: &str) -> PathBuf {
    dir.join("guards").join(format!("{mailspace}.{pool}.guard"))
}

/// Take the guard at `path` without waiting.
///
/// Answers [`Admission::Ready`] with the guard, or
/// [`Admission::Deferred`] as dispatch-busy with no expiry when another
/// admission of the same pool holds it. The guard is released when the
/// returned value drops; the lock file itself is never removed.
///
/// # Errors
/// Returns an error when the guard file cannot be created or opened, or
/// when the lock attempt itself fails — these are never busy results.
pub fn acquire(path: &Path) -> Result<Admission<DispatchGuard>, VivariumError> {
    let io =
        |e: std::io::Error| VivariumError::Message(format!("cannot guard {}: {e}", path.display()));
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
        Ok(()) => Ok(Admission::Ready(DispatchGuard { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(Admission::Deferred(Deferred {
            kind: DeferKind::DispatchBusy,
            until: None,
        })),
        Err(TryLockError::Error(e)) => Err(io(e)),
    }
}

#[cfg(test)]
#[path = "guard_test.rs"]
mod tests;
