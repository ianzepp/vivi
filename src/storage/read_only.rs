//! Ownership-aware read-only guard for WAL-backed SQLite databases.
//!
//! Opening a WAL-mode database read-only makes SQLite create the missing
//! `-shm` shared-memory sidecar, so strict readers refuse instead. Some
//! readers running as the database owner may accept that sidecar creation;
//! the policy is decided here, once, for every opener in the crate. The
//! guard never creates, deletes, or rewrites a file: it only inspects, and
//! only the ordinary `SQLITE_OPEN_READ_ONLY` open follows. Ruling kept
//! exactly: no `immutable=1`, no skipping the WAL, no schema migration, no
//! permission changes, no sidecar manipulation.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::error::VivariumError;

/// How a read-only open may treat a WAL-mode database whose shared-memory
/// sidecar is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WalIndexPolicy {
    /// Refuse any open that would create the missing `-shm` sidecar. This
    /// preserves the historical strict behavior of the storage reader.
    ExistingIndex,
    /// Authorize the missing sidecar only when the database file is owned
    /// by the operating-system effective user running this process.
    /// Unavailable ownership or unavailable effective identity is not
    /// authorization; unsupported platforms stay conservative and refuse.
    OwnerMayCreateIndex,
}

/// Resolves the database file's owner uid; `None` when unavailable.
type OwnerUid = fn(&Path) -> Option<u32>;

/// Resolves the process effective uid; `None` when unavailable.
type ReaderUid = fn() -> Option<u32>;

/// Identity seam. Production runs the real Unix resolvers through
/// [`open`] and [`guard`]; tests inject synthetic owner/reader identities
/// through the private `*_as` functions those two entry points share.
#[derive(Clone, Copy)]
struct Identity {
    owner: OwnerUid,
    reader: ReaderUid,
}

const REAL_IDENTITY: Identity = Identity {
    owner: file_owner_uid,
    reader: system_effective_uid,
};

/// Open an existing database file read-only under `policy`.
///
/// # Errors
/// Returns the classified refusal when `policy` forbids the missing
/// sidecar, and the open error when SQLite cannot open the database.
pub(crate) fn open(db_path: &Path, policy: WalIndexPolicy) -> Result<Connection, VivariumError> {
    open_as(db_path, policy, REAL_IDENTITY)
}

/// Check whether a read-only open under `policy` is allowed, without
/// opening the database and without creating any file.
///
/// # Errors
/// Returns the classified refusal when `policy` forbids the missing
/// sidecar.
pub(crate) fn guard(db_path: &Path, policy: WalIndexPolicy) -> Result<(), VivariumError> {
    guard_as(db_path, policy, REAL_IDENTITY)
}

/// True when a read-only open of this exact database file would need to
/// create the missing shared-memory sidecar beside it: the sidecar is
/// absent and the database header declares the WAL read/write format.
/// Inspectable but inconclusive inputs (unreadable, truncated, or
/// non-WAL files) are not refusals and fall through to SQLite.
#[must_use]
pub(crate) fn wal_index_missing(db_path: &Path) -> bool {
    let Some(sidecar) = wal_index_sidecar(db_path) else {
        return false;
    };
    if sidecar.exists() {
        return false;
    }
    let Ok(mut file) = fs::File::open(db_path) else {
        return false;
    };
    let mut header = [0_u8; 20];
    file.read_exact(&mut header).is_ok() && header[18] == 2 && header[19] == 2
}

fn open_as(
    db_path: &Path,
    policy: WalIndexPolicy,
    identity: Identity,
) -> Result<Connection, VivariumError> {
    guard_as(db_path, policy, identity)?;
    Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| {
        VivariumError::Other(format!("failed to open storage database read-only: {e}"))
    })
}

fn guard_as(
    db_path: &Path,
    policy: WalIndexPolicy,
    identity: Identity,
) -> Result<(), VivariumError> {
    if !wal_index_missing(db_path) {
        return Ok(());
    }
    let authorized = match policy {
        WalIndexPolicy::ExistingIndex => false,
        WalIndexPolicy::OwnerMayCreateIndex => {
            match ((identity.owner)(db_path), (identity.reader)()) {
                (Some(owner), Some(reader)) => owner == reader,
                _ => false,
            }
        }
    };
    if authorized {
        Ok(())
    } else {
        Err(refusal(db_path))
    }
}

/// The strict refusal, worded for the exact database file.
fn refusal(db_path: &Path) -> VivariumError {
    let name = db_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "database".into());
    VivariumError::Other(format!(
        "read-only inspection unavailable: the WAL-mode {name} store has no {name}-shm sidecar; refusing to create it"
    ))
}

/// Sibling path of the shared-memory sidecar for this database file.
fn wal_index_sidecar(db_path: &Path) -> Option<PathBuf> {
    let name = db_path.file_name()?.to_str()?;
    Some(db_path.with_file_name(format!("{name}-shm")))
}

/// Owner uid of the database file via Unix file metadata; `None` when the
/// platform does not expose ownership or the file cannot be inspected.
#[cfg(unix)]
fn file_owner_uid(db_path: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;

    match fs::metadata(db_path) {
        Ok(meta) => Some(meta.uid()),
        Err(_) => None,
    }
}

#[cfg(not(unix))]
fn file_owner_uid(_db_path: &Path) -> Option<u32> {
    None
}

/// Effective uid of this process through a documented one-symbol
/// `geteuid(2)` binding, so the crate carries no libc dependency. POSIX
/// defines no failure mode for `geteuid`; `None` marks only platforms
/// without the binding.
#[cfg(unix)]
fn system_effective_uid() -> Option<u32> {
    unsafe { Some(geteuid()) }
}

#[cfg(not(unix))]
fn system_effective_uid() -> Option<u32> {
    None
}

#[cfg(unix)]
unsafe extern "C" {
    fn geteuid() -> u32;
}

#[cfg(test)]
#[path = "read_only_test.rs"]
mod tests;
