//! Receipt persistence: one private, lossless file per dispatched job.
//!
//! [`save_receipt`] writes a job's exact [`JobOutcome`](crate::pump::jobs::JobOutcome)
//! receipt — the full
//! task-thread response, never the truncated extracted message — to
//! `job-replies/<job>.txt` under the pump home. The directory and file are
//! private, staging happens in a same-directory temporary file, and
//! publication is atomic and no-clobber, so a receipt file always holds a
//! whole pool reply and is written exactly once. A saved receipt's
//! modification time, taken from the host clock at save time, is the
//! retention timestamp; later observations never rewrite it.

use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::VivariumError;
use crate::pump::host::Ctx;
use crate::pump::jobs::{PoolJob, validate_id};

use super::REPLY_DIR;

/// Mode for the managed receipt directory: private to the owner.
const DIR_MODE: u32 = 0o700;
/// Mode for a receipt file: private to the owner.
const FILE_MODE: u32 = 0o600;

/// Save the job's collected pool receipt and return the saved path relative
/// to the pump home: `job-replies/<job>.txt`. A job without an outcome yet
/// saves an empty receipt. Saving an already-saved receipt keeps the bytes
/// and the retention timestamp already on disk and returns their path again.
///
/// # Errors
/// Returns [`VivariumError`] when the job ID is invalid, a managed path is a
/// symlink or otherwise obstructed, or the receipt cannot be persisted.
pub fn save_receipt(ctx: &Ctx, job: &PoolJob) -> Result<PathBuf, VivariumError> {
    validate_id(&job.id)?;
    let name = format!("{}.txt", job.id);
    let relative = Path::new(REPLY_DIR).join(&name);
    let dir = ctx.home.join(REPLY_DIR);
    let artifact = dir.join(&name);
    ensure_private_dir(&dir)?;
    match observe(&artifact)? {
        Observed::Saved => return Ok(relative),
        Observed::Absent => {}
        Observed::Symlink => return Err(symlink_rejected(&artifact)),
        Observed::Obstructed => return Err(obstructed(&artifact)),
    }
    let mut temp = tempfile::Builder::new()
        .prefix(&format!(".{name}."))
        .tempfile_in(&dir)
        .map_err(|error| failed("stage", &artifact, &error))?;
    temp.write_all(receipt_bytes(job))
        .map_err(|error| failed("write", &artifact, &error))?;
    temp.flush()
        .map_err(|error| failed("write", &artifact, &error))?;
    let file = temp.as_file_mut();
    file.set_permissions(fs::Permissions::from_mode(FILE_MODE))
        .map_err(|error| failed("privatize", &artifact, &error))?;
    file.set_times(fs::FileTimes::new().set_modified(retention(ctx)?))
        .map_err(|error| failed("timestamp", &artifact, &error))?;
    match temp.persist_noclobber(&artifact) {
        Ok(_) => Ok(relative),
        // Another save published first; its bytes and timestamp stand.
        Err(error) if error.error.kind() == ErrorKind::AlreadyExists => match observe(&artifact)? {
            Observed::Saved => Ok(relative),
            Observed::Symlink => Err(symlink_rejected(&artifact)),
            Observed::Obstructed | Observed::Absent => Err(obstructed(&artifact)),
        },
        Err(error) => Err(failed("publish", &artifact, &error.error)),
    }
}

/// The receipt bytes: exactly the collected pool reply, or nothing when the
/// job has no outcome yet.
fn receipt_bytes(job: &PoolJob) -> &[u8] {
    job.outcome
        .as_ref()
        .map_or(&[][..], |outcome| outcome.receipt.as_bytes())
}

/// The retention timestamp: the host clock when the receipt is saved.
fn retention(ctx: &Ctx) -> Result<SystemTime, VivariumError> {
    UNIX_EPOCH
        .checked_add(Duration::from_secs(ctx.host.now()))
        .ok_or_else(|| VivariumError::Other(String::from("pump.reply: host clock out of range")))
}

/// Create the receipt directory if needed and keep it private. A symlinked
/// managed directory is never entered, followed, or chmod'ed.
fn ensure_private_dir(dir: &Path) -> Result<(), VivariumError> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if !meta.is_dir() => {
            return Err(if meta.is_symlink() {
                symlink_rejected(dir)
            } else {
                obstructed(dir)
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {
            fs::create_dir_all(dir).map_err(|error| failed("create directory", dir, &error))?;
        }
        Err(error) => return Err(failed("inspect", dir, &error)),
    }
    // Re-stat after any create: never chmod a managed path that turned out
    // to be a symlink pointing elsewhere.
    let meta = fs::symlink_metadata(dir).map_err(|error| failed("inspect", dir, &error))?;
    if meta.is_symlink() {
        return Err(symlink_rejected(dir));
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(DIR_MODE))
        .map_err(|error| failed("privatize directory", dir, &error))
}

/// What stands at an artifact path.
enum Observed {
    /// A regular file: a saved receipt.
    Saved,
    /// Nothing; the artifact may be published.
    Absent,
    /// A symlink; never followed.
    Symlink,
    /// Some other non-file entry; the artifact cannot be published.
    Obstructed,
}

/// Classify `path` without following symlinks.
fn observe(path: &Path) -> Result<Observed, VivariumError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_symlink() => Ok(Observed::Symlink),
        Ok(meta) if meta.is_file() => Ok(Observed::Saved),
        Ok(_) => Ok(Observed::Obstructed),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Observed::Absent),
        Err(error) => Err(failed("inspect", path, &error)),
    }
}

/// A persistence error carrying what failed and where.
fn failed(what: &str, path: &Path, error: &std::io::Error) -> VivariumError {
    VivariumError::Other(format!("pump.reply: {what} {}: {error}", path.display()))
}

/// Refusal to follow a symlinked managed path.
fn symlink_rejected(path: &Path) -> VivariumError {
    VivariumError::Other(format!(
        "pump.reply: refusing to follow symlink {}",
        path.display()
    ))
}

/// A managed path blocked by something that is not a regular receipt file.
fn obstructed(path: &Path) -> VivariumError {
    VivariumError::Other(format!(
        "pump.reply: {} is not a regular receipt file",
        path.display()
    ))
}

#[cfg(test)]
#[path = "storage_test.rs"]
mod tests;
