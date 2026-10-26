//! Slot reservations: one row per in-flight claim a job holds on a pool's
//! model.
//!
//! Live reservations are files under the project's
//! `.vivi/pool-band-admission/`: a per-pool admission lock that takes the
//! dispatchers of one pool on one machine through "scan, then reserve" one
//! at a time, and one locked file per in-flight claim. The caller owns the
//! short admission lock and releases it as soon as it has reserved; the
//! reservation itself is guarded by a [`Reservation`], which holds the OS
//! file lock until dropped, so a claim dies with its process: whatever a
//! later scan can lock was abandoned and is reclaimed, and only what is
//! still locked counts against the pool's in-flight cap. This is model
//! admission state, not provider slot state; no provider resources are
//! touched here.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::VivariumError;

/// The directory under a project's `.vivi` that holds live pool-band
/// admission state: per-pool admission locks and live reservation files.
pub const ADMISSION_DIR: &str = "pool-band-admission";

/// One in-flight claim a job holds on a pool's model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservationRow {
    /// The pool whose slot is held.
    pub pool: String,
    /// The job holding the slot.
    pub job: String,
    /// The model slug the job runs with.
    pub model: String,
}

/// Why a pool's admission lock could not be taken.
#[derive(Debug, thiserror::Error)]
pub enum AdmissionLockError {
    /// Another dispatcher holds the pool's admission lock: the pool is busy
    /// admitting someone else, not broken. The stable code is
    /// `admission-busy` (see [`super::eligibility::Reason::AdmissionBusy`]).
    #[error("another dispatch holds this pool's admission lock (admission-busy)")]
    Busy,
    /// A real I/O failure on the admission directory or lock file.
    #[error(transparent)]
    Io(#[from] VivariumError),
}

impl From<AdmissionLockError> for VivariumError {
    fn from(error: AdmissionLockError) -> Self {
        match error {
            AdmissionLockError::Io(io) => io,
            busy @ AdmissionLockError::Busy => VivariumError::Message(busy.to_string()),
        }
    }
}

/// A pool's admission lock, held while its dispatcher scans live claims and
/// reserves one. Short by design: never held across provider work or for a
/// [`Reservation`]'s lifetime.
#[derive(Debug)]
pub struct AdmissionGuard {
    _file: File,
}

/// A live reservation: the pool holds one in-flight claim for the guard's
/// row. The OS file lock lasts as long as the guard; dropping it removes
/// the reservation file and releases the lock, on every exit.
#[derive(Debug)]
pub struct Reservation {
    path: PathBuf,
    row: ReservationRow,
    _file: File,
}

impl Reservation {
    /// The immutable row this reservation carries.
    #[must_use]
    pub fn row(&self) -> &ReservationRow {
        &self.row
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        // A leftover file is reclaimed by the next scan once its lock is
        // gone, so a failed removal is not fatal here.
        std::fs::remove_file(&self.path)
            .map(drop)
            .unwrap_or_default();
    }
}

/// A short path-safe name for one identity: 16 hex chars of its SHA-256,
/// so pool slugs and job ids never shape a path of their own.
fn identity_hash(identity: &str) -> String {
    let digest = Sha256::digest(identity.as_bytes());
    hex::encode(&digest[..8])
}

/// The project's pool-band admission directory.
fn admission_dir(vivi_dir: &Path) -> PathBuf {
    vivi_dir.join(ADMISSION_DIR)
}

/// The file of `pool`'s admission lock.
fn lock_path(vivi_dir: &Path, pool: &str) -> PathBuf {
    admission_dir(vivi_dir).join(format!("{}.lock", identity_hash(pool)))
}

/// The directory of `pool`'s live reservation files.
fn pool_reservations_dir(vivi_dir: &Path, pool: &str) -> PathBuf {
    admission_dir(vivi_dir)
        .join("reservations")
        .join(identity_hash(pool))
}

/// The reservation file of one job under its pool's directory.
fn reservation_path(vivi_dir: &Path, pool: &str, job: &str) -> PathBuf {
    pool_reservations_dir(vivi_dir, pool).join(format!("{}.json", identity_hash(job)))
}

/// Take `pool`'s admission lock without blocking.
///
/// # Errors
/// Returns [`AdmissionLockError::Busy`] when another dispatch holds the
/// lock, and [`AdmissionLockError::Io`] for real I/O failures.
pub fn admission_lock(vivi_dir: &Path, pool: &str) -> Result<AdmissionGuard, AdmissionLockError> {
    std::fs::create_dir_all(admission_dir(vivi_dir)).map_err(VivariumError::from)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path(vivi_dir, pool))
        .map_err(VivariumError::from)?;
    match file.try_lock() {
        Ok(()) => Ok(AdmissionGuard { _file: file }),
        Err(TryLockError::WouldBlock) => Err(AdmissionLockError::Busy),
        Err(TryLockError::Error(error)) => Err(AdmissionLockError::Io(error.into())),
    }
}

/// Every live reservation `pool` currently holds, one row per in-flight
/// claim. Call while holding the pool's admission lock. A reservation file
/// this process can lock was abandoned by a dead holder and is reclaimed —
/// removed, not counted — and age is never consulted, so a live slow launch
/// cannot be expired. A file that cannot be opened, read or parsed fails
/// the scan: silently skipping it could let the pool exceed its cap.
///
/// # Errors
/// Returns an error when a reservation file cannot be read, parsed or
/// reclaimed.
pub fn scan_live(vivi_dir: &Path, pool: &str) -> Result<Vec<ReservationRow>, VivariumError> {
    let entries = match std::fs::read_dir(pool_reservations_dir(vivi_dir, pool)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut paths: Vec<PathBuf> = entries
        .collect::<Result<Vec<_>, _>>()
        .map_err(VivariumError::from)?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    let mut live = Vec::new();
    for path in paths {
        if let Claim::Live(row) = scan_entry(&path)? {
            live.push(row);
        }
    }
    Ok(live)
}

/// The verdict on one reservation file found by a scan.
enum Claim {
    /// Locked by a live holder: counts against the pool's cap.
    Live(ReservationRow),
    /// Unlocked: its holder died, and the file was reclaimed.
    Abandoned,
}

/// Open, read and test-lock one reservation file: locked means live,
/// lockable means abandoned and reclaimable.
fn scan_entry(path: &Path) -> Result<Claim, VivariumError> {
    let open = |error: std::io::Error| {
        VivariumError::Message(format!(
            "pool-band admission: cannot open live reservation {}: {error}",
            path.display()
        ))
    };
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(open)?;
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(|error| {
        VivariumError::Message(format!(
            "pool-band admission: cannot read live reservation {}: {error}",
            path.display()
        ))
    })?;
    match file.try_lock() {
        Err(TryLockError::WouldBlock) => parse_live(path, &text),
        Ok(()) => {
            std::fs::remove_file(path).map_err(VivariumError::from)?;
            Ok(Claim::Abandoned)
        }
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

/// Parse the row of a live (locked) reservation.
fn parse_live(path: &Path, text: &str) -> Result<Claim, VivariumError> {
    let row: ReservationRow = serde_json::from_str(text).map_err(|error| {
        VivariumError::Message(format!(
            "pool-band admission: live reservation {} does not hold a reservation row: {error}",
            path.display()
        ))
    })?;
    Ok(Claim::Live(row))
}

/// Reserve a slot for `row`'s fresh dispatch job: write its immutable JSON
/// row and lock the file. Call while holding the pool's admission lock; a
/// fresh job id never collides with a live reservation.
///
/// # Errors
/// Returns an error when the reservation file cannot be made, written or
/// locked.
pub fn reserve(vivi_dir: &Path, row: ReservationRow) -> Result<Reservation, VivariumError> {
    std::fs::create_dir_all(pool_reservations_dir(vivi_dir, &row.pool))
        .map_err(VivariumError::from)?;
    let path = reservation_path(vivi_dir, &row.pool, &row.job);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
        .map_err(VivariumError::from)?;
    let text = serde_json::to_string(&row)
        .map_err(|error| VivariumError::Parse(format!("reservation row: {error}")))?;
    file.write_all(text.as_bytes())
        .map_err(VivariumError::from)?;
    match file.try_lock() {
        Ok(()) => Ok(Reservation {
            path,
            row,
            _file: file,
        }),
        Err(TryLockError::WouldBlock) => Err(VivariumError::Message(format!(
            "pool-band admission: the fresh reservation {} is locked by another dispatch",
            path.display()
        ))),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

#[cfg(test)]
#[path = "reservations_test.rs"]
mod tests;
