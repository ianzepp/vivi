//! A lock between the dispatchers of one pool on one machine.
//!
//! Dispatchers on one machine are the common race, and an advisory file lock
//! removes it: they take turns through "list, pick an index, make the pod,
//! check the claim". Dispatchers on other machines are not covered; the claim
//! rule of [`runpod_claim`](super::runpod_claim) is what they rely on.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::error::VivariumError;

/// Held while a dispatch makes its pod; released when dropped.
#[derive(Debug)]
pub struct DispatchLock {
    _file: File,
}

/// The lock file of a pool below the provider's state directory.
#[must_use]
pub fn path_for(dir: &Path, mailspace: &str, pool: &str) -> PathBuf {
    dir.join("locks").join(format!("{mailspace}.{pool}.lock"))
}

/// Take the lock at `path`, waiting up to `wait` and saying so once through
/// `note` when it has to.
///
/// # Errors
/// Returns an error if the lock cannot be made or is not free within `wait`.
pub fn acquire(
    path: &Path,
    wait: Duration,
    poll: Duration,
    note: &dyn Fn(&str),
) -> Result<DispatchLock, VivariumError> {
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
    let started = Instant::now();
    let mut told = false;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(DispatchLock { _file: file }),
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(e)) => return Err(io(e)),
        }
        if started.elapsed() >= wait {
            return Err(VivariumError::Message(format!(
                "another dispatch of this pool on this machine held the lock for {}s; try again when it has finished",
                wait.as_secs()
            )));
        }
        if !told {
            note("another dispatch of this pool is making its pod; waiting for it");
            told = true;
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
#[path = "runpod_lock_test.rs"]
mod tests;
