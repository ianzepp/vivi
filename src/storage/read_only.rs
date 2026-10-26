//! Classified read-only inspection and open for the mailspace database.
//!
//! A read-only SQLite connection may still create the `-shm` sidecar of a
//! WAL-mode database, so preflight inspection decides *before* SQLite is
//! opened whether the open is allowed. The ruling is explicit ownership
//! policy: a reader that provably owns the database file may open a WAL
//! database without an index (the open creates a correctly owned sidecar);
//! every foreign or unprovable case is refused with a classified error.
//!
//! Inspection is error-aware: genuine `NotFound` represents absence, while
//! permission errors, non-file paths, and header-read failures are
//! classified failures that never authorize an open. The connection is
//! always [`OpenFlags::SQLITE_OPEN_READ_ONLY`]; no immutable mode, no
//! copied-store fallback, no write-capable open, no migration.

use std::fmt;
use std::fs;
use std::io;
use std::io::Read;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::error::VivariumError;

#[cfg(test)]
#[path = "read_only_test.rs"]
mod tests;

/// How much of the WAL-index responsibility a read-only open may take on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexPolicy {
    /// Refuse unless the WAL index already exists (or the database does not
    /// use WAL journaling).
    ExistingIndexOnly,
    /// Tolerate a missing WAL index when the reader provably owns the
    /// database file: the open then creates a correctly owned sidecar.
    /// Unknown or foreign ownership is still refused.
    OwnerMayCreateIndex,
}

/// Outcome of a successful preflight inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Inspection {
    /// The open cannot create a sidecar: the database does not use WAL
    /// journaling, the WAL index already exists, or the database is
    /// genuinely absent (a read-only open of an absent database creates
    /// nothing and SQLite reports the absence itself).
    NoIndexNeeded,
    /// WAL database without an index, and the reader provably owns the
    /// database file. Only reachable under [`IndexPolicy::OwnerMayCreateIndex`];
    /// the open creates a correctly owned sidecar.
    OwnedSidecarAllowed,
}

/// Why a filesystem inspection could not decide. Any of these means absence
/// was never proven, so no open may rely on "the index is missing".
#[derive(Debug)]
pub(crate) enum InspectionFailure {
    /// The database path exists but is not a regular file.
    NotAFile,
    /// The database file could not be stat'ed for a reason other than
    /// genuine absence.
    DatabaseStat(io::Error),
    /// The database header could not be read; WAL-ness stays unknown.
    HeaderRead(io::Error),
    /// The WAL-index sidecar could not be stat'ed; absence was not proven.
    SidecarStat(io::Error),
}

/// Why a classified read-only open did not happen.
#[derive(Debug)]
pub(crate) enum ReadOnlyRefusal {
    /// [`IndexPolicy::ExistingIndexOnly`] and the WAL-mode database has no
    /// index sidecar: the open would create one.
    StrictIndexMissing { db_path: PathBuf },
    /// WAL-mode database without an index; the file belongs to a different
    /// user, so the created sidecar would be foreign-owned.
    ForeignIndexMissing { db_path: PathBuf, owner_uid: u32 },
    /// WAL-mode database without an index; ownership could not be proven on
    /// this platform or from the process table, so nothing is authorized.
    UnknownOwnerIndexMissing { db_path: PathBuf },
    /// Filesystem inspection failed; absence of the index was never proven.
    Inspection {
        db_path: PathBuf,
        failure: InspectionFailure,
    },
    /// SQLite refused the read-only connection.
    SqliteOpen(rusqlite::Error),
}

impl fmt::Display for ReadOnlyRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StrictIndexMissing { db_path } => write!(
                f,
                "read-only open refused: {} is in WAL mode with no {} sidecar; refusing to create it",
                db_path.display(),
                wal_index_path(db_path).display()
            ),
            Self::ForeignIndexMissing { db_path, owner_uid } => write!(
                f,
                "read-only open refused: {} is in WAL mode with no {} sidecar and is owned by uid {owner_uid}; refusing to create a foreign-owned sidecar",
                db_path.display(),
                wal_index_path(db_path).display()
            ),
            Self::UnknownOwnerIndexMissing { db_path } => write!(
                f,
                "read-only open refused: {} is in WAL mode with no {} sidecar and database ownership could not be established; refusing to create it",
                db_path.display(),
                wal_index_path(db_path).display()
            ),
            Self::Inspection { db_path, failure } => match failure {
                InspectionFailure::NotAFile => write!(
                    f,
                    "read-only inspection unavailable: {} is not a regular file; refusing to open it",
                    db_path.display()
                ),
                InspectionFailure::DatabaseStat(error) => write!(
                    f,
                    "read-only inspection unavailable: could not inspect {}: {error}",
                    db_path.display()
                ),
                InspectionFailure::HeaderRead(error) => write!(
                    f,
                    "read-only inspection unavailable: could not read the header of {}: {error}",
                    db_path.display()
                ),
                InspectionFailure::SidecarStat(error) => write!(
                    f,
                    "read-only inspection unavailable: could not inspect {}: {error}",
                    wal_index_path(db_path).display()
                ),
            },
            Self::SqliteOpen(source) => {
                write!(f, "failed to open storage database read-only: {source}")
            }
        }
    }
}

impl From<ReadOnlyRefusal> for VivariumError {
    fn from(refusal: ReadOnlyRefusal) -> Self {
        VivariumError::Other(refusal.to_string())
    }
}

/// Inspect a database for a read-only open under `policy`, before SQLite is
/// involved. Uses the real effective process identity for ownership.
///
/// # Errors
/// Returns the classified refusal when the policy forbids the open or the
/// filesystem could not be inspected.
pub(crate) fn inspect(db_path: &Path, policy: IndexPolicy) -> Result<Inspection, ReadOnlyRefusal> {
    inspect_with(db_path, policy, &system_effective_uid)
}

/// Open an ordinary read-only connection after the policy inspection. Never
/// creates the database or migrates it; SQLite may still create the WAL
/// index sidecar, which is exactly what the inspection rules on.
///
/// # Errors
/// Returns the classified refusal when the policy forbids the open, the
/// filesystem could not be inspected, or SQLite refuses the connection.
pub(crate) fn open(db_path: &Path, policy: IndexPolicy) -> Result<Connection, ReadOnlyRefusal> {
    open_with(db_path, policy, &system_effective_uid)
}

/// [`inspect`] with an injected effective-identity source. Production and
/// tests run the identical path; tests substitute identities instead of
/// switching users.
fn inspect_with(
    db_path: &Path,
    policy: IndexPolicy,
    effective_uid: &dyn Fn() -> Option<u32>,
) -> Result<Inspection, ReadOnlyRefusal> {
    match sidecar_index_presence(db_path) {
        SidecarPresence::Present => return Ok(Inspection::NoIndexNeeded),
        SidecarPresence::Absent => {}
        SidecarPresence::Unknown(error) => {
            return Err(ReadOnlyRefusal::Inspection {
                db_path: db_path.to_path_buf(),
                failure: InspectionFailure::SidecarStat(error),
            });
        }
    }
    match database_facts(db_path).map_err(|failure| ReadOnlyRefusal::Inspection {
        db_path: db_path.to_path_buf(),
        failure,
    })? {
        DatabaseFacts::Absent => Ok(Inspection::NoIndexNeeded),
        DatabaseFacts::Present {
            wal_mode: false, ..
        } => Ok(Inspection::NoIndexNeeded),
        DatabaseFacts::Present {
            wal_mode: true,
            owner_uid,
        } => rule_on_missing_index(db_path, policy, owner_uid, effective_uid),
    }
}

/// [`open`] with an injected effective-identity source (see [`inspect_with`]).
fn open_with(
    db_path: &Path,
    policy: IndexPolicy,
    effective_uid: &dyn Fn() -> Option<u32>,
) -> Result<Connection, ReadOnlyRefusal> {
    inspect_with(db_path, policy, effective_uid)?;
    Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(ReadOnlyRefusal::SqliteOpen)
}

/// Policy ruling for a WAL-mode database whose index is genuinely absent.
fn rule_on_missing_index(
    db_path: &Path,
    policy: IndexPolicy,
    owner_uid: Option<u32>,
    effective_uid: &dyn Fn() -> Option<u32>,
) -> Result<Inspection, ReadOnlyRefusal> {
    match policy {
        IndexPolicy::ExistingIndexOnly => Err(ReadOnlyRefusal::StrictIndexMissing {
            db_path: db_path.to_path_buf(),
        }),
        IndexPolicy::OwnerMayCreateIndex => match (effective_uid(), owner_uid) {
            (Some(reader), Some(owner)) if reader == owner => Ok(Inspection::OwnedSidecarAllowed),
            (Some(_), Some(owner)) => Err(ReadOnlyRefusal::ForeignIndexMissing {
                db_path: db_path.to_path_buf(),
                owner_uid: owner,
            }),
            _ => Err(ReadOnlyRefusal::UnknownOwnerIndexMissing {
                db_path: db_path.to_path_buf(),
            }),
        },
    }
}

/// Sidecar presence per an error-aware stat: genuine `NotFound` is absence,
/// anything else is unknown.
fn sidecar_index_presence(db_path: &Path) -> SidecarPresence {
    match fs::metadata(wal_index_path(db_path)) {
        Ok(_) => SidecarPresence::Present,
        Err(error) if error.kind() == io::ErrorKind::NotFound => SidecarPresence::Absent,
        Err(error) => SidecarPresence::Unknown(error),
    }
}

enum SidecarPresence {
    Present,
    Absent,
    Unknown(io::Error),
}

/// What inspection learned about the database file itself.
enum DatabaseFacts {
    /// The database is genuinely absent.
    Absent,
    Present {
        /// Header bytes 18 and 19 both read 2 (WAL journal mode).
        wal_mode: bool,
        /// Owning user id from file metadata; `None` off-unix.
        owner_uid: Option<u32>,
    },
}

/// Stat the database and read its header bytes 18/19. Genuine `NotFound` is
/// an absent database; any other stat error, a non-file path, or a failed
/// header read is an inspection failure that must not authorize an open.
fn database_facts(db_path: &Path) -> Result<DatabaseFacts, InspectionFailure> {
    let metadata = match fs::metadata(db_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(DatabaseFacts::Absent);
        }
        Err(error) => return Err(InspectionFailure::DatabaseStat(error)),
    };
    if !metadata.is_file() {
        return Err(InspectionFailure::NotAFile);
    }
    let mut file = fs::File::open(db_path).map_err(InspectionFailure::HeaderRead)?;
    let mut header = [0_u8; 20];
    file.read_exact(&mut header)
        .map_err(InspectionFailure::HeaderRead)?;
    Ok(DatabaseFacts::Present {
        wal_mode: header[18] == 2 && header[19] == 2,
        owner_uid: file_owner_uid(&metadata),
    })
}

/// Owning user id from file metadata. Off-unix there is no owner to prove.
#[cfg(unix)]
fn file_owner_uid(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.uid())
}

#[cfg(not(unix))]
fn file_owner_uid(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

/// Effective user id of the running process, from `sysinfo`. Only the
/// current process is refreshed, and only the identity fields. `None` when
/// the platform or the process table cannot establish identity; unknown
/// identity never authorizes sidecar creation.
#[cfg(unix)]
fn system_effective_uid() -> Option<u32> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    let pid = match sysinfo::get_current_pid() {
        Ok(pid) => pid,
        Err(_) => return None,
    };
    let mut system = System::new();
    let refreshed = system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::new().with_user(UpdateKind::OnlyIfNotSet),
    );
    if refreshed == 0 {
        return None;
    }
    system.process(pid)?.effective_user_id().map(|uid| **uid)
}

#[cfg(not(unix))]
fn system_effective_uid() -> Option<u32> {
    None
}

/// The WAL-index sidecar path beside `db_path`.
fn wal_index_path(db_path: &Path) -> PathBuf {
    let mut path = db_path.as_os_str().to_os_string();
    path.push("-shm");
    PathBuf::from(path)
}
