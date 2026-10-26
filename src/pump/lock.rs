//! The pump lock owner: an advisory lock that unlocks explicitly on drop.
//!
//! Releasing an advisory lock by closing the descriptor only works while no
//! other descriptor refers to the same open file description. Between `fork`
//! and `exec` a spawned child can hold a duplicate of the lock descriptor, and
//! close-on-exec does not remove that interval: while the duplicate lives, the
//! lock survives the owner closing its own copy. [`PumpLock`] therefore calls
//! [`File::unlock`] while it still owns the descriptor, so a dropped owner
//! frees the lock even when a duplicate is open somewhere.

use std::fs::File;

/// One exclusive holder of the pump lock file. Dropping it unlocks.
///
/// The type is deliberately not `Clone` and keeps its descriptor private:
/// duplicating the file with [`File::try_clone`] puts both descriptors on the
/// same open file description, so a duplicate is a second reference to the
/// same lock, not a transferable ownership token.
#[derive(Debug)]
pub(crate) struct PumpLock {
    file: File,
}

impl PumpLock {
    /// Take the lock on `file` with a single non-blocking acquisition.
    ///
    /// # Errors
    /// Returns the [`std::fs::TryLockError`] from that acquisition:
    /// `WouldBlock` when another holder keeps the lock, or an IO error. A
    /// failed contender constructs no owner and unlocks nothing.
    pub(crate) fn try_lock(file: File) -> Result<Self, std::fs::TryLockError> {
        file.try_lock()?;
        Ok(Self { file })
    }
}

impl Drop for PumpLock {
    fn drop(&mut self) {
        if let Err(error) = self.file.unlock() {
            tracing::warn!("pump lock unlock failed while dropping the owner: {error}");
        }
    }
}

#[cfg(test)]
#[path = "lock_test.rs"]
mod tests;
