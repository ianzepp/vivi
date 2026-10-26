//! Shared, ownership-aware WAL guard for path-based read-only database
//! opens. This guards `mail.sqlite` and `pump.db` alike; it is a path
//! decision, not a SQLite opener.
//!
//! A read-only SQLite open of a WAL database whose `-shm` shared-memory
//! index is absent makes SQLite create that sidecar — a write beside a
//! database this process may not own. This module decides from the database
//! header bytes alone whether a read-only open is safe: it never opens a
//! writer and never creates, removes, or rewrites any file.
//!
//! Ownership comes only from the platform: the database file's owner UID and
//! this process's effective UID. It is never inferred from environment
//! variables, directories, or writability. The operator ruling is: when the
//! reader owns the database file, a normal read-only open (which creates a
//! correctly owned sidecar) is safe. Never reach for `immutable=1` or skip
//! the WAL instead: that would show stale or missing recent data.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::VivariumError;

/// Bytes 18 (write) and 19 (read) of the SQLite header hold a file format
/// version; 2 marks WAL mode, 1 the rollback journal.
const WAL_FORMAT_VERSION: u8 = 2;

/// Length of the SQLite header prefix the guard inspects: the 16-byte magic
/// string plus the page and encoding bytes and the two format versions.
const HEADER_LEN: usize = 20;

/// How a read-only reader treats a WAL database whose `-shm` index is
/// absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WalSidecarPolicy {
    /// Refuse whenever the database is in WAL mode and the sidecar is
    /// absent: this reader never creates a shared-memory index, whatever
    /// the ownership.
    ExistingSidecarOnly,
    /// Permit the absent sidecar only when the database file is owned by
    /// the same user this process runs as: a normal read-only open then
    /// creates a correctly owned sidecar, so recently committed data stays
    /// visible. Unknown ownership or unknown effective identity still
    /// refuses the exception.
    AllowOwnedSidecar,
}

/// Gate a read-only open of `db_path` behind `policy`. `Ok(())` lets the
/// open proceed; an error refuses it. A database that is not in WAL mode,
/// or whose `-shm` sidecar already exists, always passes: there is nothing
/// to create.
///
/// # Errors
/// Propagates filesystem and header-read errors from
/// [`wal_index_missing`], and returns [`VivariumError::Other`] describing
/// the refused sidecar creation when the policy refuses.
pub(crate) fn guard_wal_sidecar(
    db_path: &Path,
    policy: WalSidecarPolicy,
) -> Result<(), VivariumError> {
    guard_wal_sidecar_with(db_path, policy, file_owner_uid, effective_uid)
}

/// Same gate with the two identity lookups injected per call. Production
/// passes [`file_owner_uid`] and [`effective_uid`]; tests pass substitutes.
fn guard_wal_sidecar_with<O, E>(
    db_path: &Path,
    policy: WalSidecarPolicy,
    file_owner_uid: O,
    effective_uid: E,
) -> Result<(), VivariumError>
where
    O: Fn(&Path) -> std::io::Result<Option<u32>>,
    E: Fn() -> Option<u32>,
{
    if !wal_index_missing(db_path)? {
        return Ok(());
    }
    let name = db_file_name(db_path);
    match policy {
        WalSidecarPolicy::ExistingSidecarOnly => Err(missing_sidecar_refusal(&name)),
        WalSidecarPolicy::AllowOwnedSidecar => {
            allow_owned_sidecar(db_path, &name, file_owner_uid, effective_uid)
        }
    }
}

/// The owned-sidecar exception: refuse unless the platform can name both the
/// database file's owner and this process's effective user, and the two are
/// the same user.
///
/// # Errors
/// Returns [`VivariumError::Other`] for unknown ownership, unknown
/// effective identity, or a mismatch; filesystem errors from the owner
/// lookup propagate as-is.
fn allow_owned_sidecar<O, E>(
    db_path: &Path,
    name: &str,
    file_owner_uid: O,
    effective_uid: E,
) -> Result<(), VivariumError>
where
    O: Fn(&Path) -> std::io::Result<Option<u32>>,
    E: Fn() -> Option<u32>,
{
    let Some(owner) = file_owner_uid(db_path)? else {
        return Err(VivariumError::Other(format!(
            "read-only open refused: cannot determine the owner of {name}; \
             refusing to create the {name}-shm sidecar"
        )));
    };
    let Some(effective) = effective_uid() else {
        return Err(VivariumError::Other(
            "read-only open refused: cannot determine the effective user of this process; \
             refusing to create a WAL shared-memory sidecar"
                .into(),
        ));
    };
    if owner == effective {
        return Ok(());
    }
    Err(VivariumError::Other(format!(
        "read-only open refused: {name} is owned by user {owner} but this process runs as \
         user {effective}; refusing to create the {name}-shm sidecar"
    )))
}

fn missing_sidecar_refusal(name: &str) -> VivariumError {
    VivariumError::Other(format!(
        "read-only open refused: {name} is in WAL mode with no {name}-shm sidecar; \
         refusing to create it"
    ))
}

/// Fallible, ownership-independent inspection: `Ok(true)` when a read-only
/// open of `db_path` would have to create the WAL shared-memory index — the
/// header says WAL mode and no `-shm` sidecar exists beside the database.
/// The header is read straight from the file bytes: no writer is opened and
/// no file is touched.
///
/// # Errors
/// Propagates filesystem errors. A missing database, a file too short for
/// the SQLite header, or an unreadable file is an error, not a guess.
pub(crate) fn wal_index_missing(db_path: &Path) -> Result<bool, VivariumError> {
    let header = read_header(db_path)?;
    Ok(header_says_wal(header) && !shm_path(db_path).exists())
}

fn read_header(db_path: &Path) -> Result<[u8; HEADER_LEN], VivariumError> {
    let mut file = fs::File::open(db_path)?;
    let mut header = [0_u8; HEADER_LEN];
    file.read_exact(&mut header)?;
    Ok(header)
}

#[must_use]
fn header_says_wal(header: [u8; HEADER_LEN]) -> bool {
    header[18] == WAL_FORMAT_VERSION && header[19] == WAL_FORMAT_VERSION
}

/// Path of the shared-memory index beside a database.
#[must_use]
fn shm_path(db_path: &Path) -> PathBuf {
    let mut name = db_path.as_os_str().to_os_string();
    name.push("-shm");
    PathBuf::from(name)
}

/// File name of the database for error messages, or the whole path when the
/// path has no usable final component.
#[must_use]
fn db_file_name(db_path: &Path) -> String {
    db_path.file_name().map_or_else(
        || db_path.to_string_lossy().into_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Owning user of the database file, or `None` when the platform cannot
/// say. Read from the file's own metadata; never inferred from environment
/// variables, directories, or writability.
#[cfg(unix)]
fn file_owner_uid(db_path: &Path) -> std::io::Result<Option<u32>> {
    use std::os::unix::fs::MetadataExt;
    Ok(Some(fs::metadata(db_path)?.uid()))
}

/// Unsupported platforms stay conservative: unknown ownership refuses the
/// owned-sidecar exception.
#[cfg(not(unix))]
fn file_owner_uid(_db_path: &Path) -> std::io::Result<Option<u32>> {
    Ok(None)
}

/// Effective user of this process, or `None` when the platform cannot say.
/// Read from the operating system, not from the environment.
#[cfg(unix)]
#[must_use]
fn effective_uid() -> Option<u32> {
    // SAFETY: `geteuid` takes no arguments, cannot fail, and libc is linked
    // into every unix target of this crate.
    Some(unsafe { geteuid() })
}

/// Unsupported platforms stay conservative: unknown identity refuses the
/// owned-sidecar exception.
#[cfg(not(unix))]
#[must_use]
fn effective_uid() -> Option<u32> {
    None
}

#[cfg(unix)]
unsafe extern "C" {
    fn geteuid() -> u32;
}

#[cfg(test)]
#[path = "read_only_test.rs"]
mod tests;
