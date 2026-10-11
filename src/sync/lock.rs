//! One sync at a time per checkout.
//!
//! Push, pull, resync, fetch and prune all read and rewrite the same state
//! file and the same store. Two at once (a cron run over an interactive one)
//! would race on both, so each takes an exclusive lock on
//! `<project>/.vivi/sync/sync.lock` for its whole run. The operating system
//! releases the lock when the process exits, so a crash leaves nothing to
//! clean up.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::error::VivariumError;
use crate::sync::state::ensure_sync_dir;

const LOCK_FILE: &str = "sync.lock";

/// How long a second sync waits for the first before giving up.
pub const WAIT: Duration = Duration::from_secs(10);

const POLL: Duration = Duration::from_millis(100);

/// The held lock. Dropping it releases the lock.
#[derive(Debug)]
pub struct SyncLock {
    _file: File,
}

impl SyncLock {
    /// Take the lock under `dir`, waiting up to [`WAIT`] for another sync.
    ///
    /// # Errors
    /// Returns an error when another sync still holds the lock after the
    /// wait, or the lock file cannot be opened.
    pub fn acquire(dir: &Path) -> Result<Self, VivariumError> {
        Self::acquire_within(dir, WAIT)
    }

    fn acquire_within(dir: &Path, wait: Duration) -> Result<Self, VivariumError> {
        ensure_sync_dir(dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(LOCK_FILE))?;
        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(TryLockError::WouldBlock) => {
                    if started.elapsed() >= wait {
                        return Err(VivariumError::Message(format!(
                            "another vivi sync is running on this mailspace (waited {}s); \
                             try again when it finishes",
                            wait.as_secs()
                        )));
                    }
                    sleep(POLL);
                }
                Err(TryLockError::Error(error)) => return Err(error.into()),
            }
        }
    }
}

#[cfg(test)]
#[path = "lock_test.rs"]
mod tests;
