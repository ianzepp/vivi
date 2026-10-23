//! Operator release of a held pool job: `vivi pump requeue --job <id>`.
//!
//! A job held after repeated pool refusals may have no step (a cadence or
//! planner job) or no pump record to post an action on, so the release does
//! not go through a need. It writes a request row into `pump.db`
//! ([`PumpStore::request_job_release`]) and the running pump applies it at its
//! next tick ([`crate::pump::jobs`]).

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::load_state;
use crate::VivariumError;
use crate::pump::jobs::JobStatus;
use crate::pump::store::{DB_FILENAME, PumpStore};

/// Ask the pump to release held job `id`. Refuses an unknown job and one that
/// is not held, naming its status. `dry_run` checks and prints, writes nothing.
///
/// # Errors
/// Returns [`VivariumError::Message`] for a mailspace with no pump database,
/// an unknown job, or a job that is not held, and whatever writing the
/// request fails with.
pub(super) fn request(home: &Path, id: &str, dry_run: bool) -> Result<(), VivariumError> {
    let path = home.join(DB_FILENAME);
    let st = load_state(&path)?;
    let job = st
        .jobs
        .get(id)
        .ok_or_else(|| VivariumError::Message(format!("unknown job: {id}")))?;
    if job.status != JobStatus::Pending || job.held.is_none() {
        return Err(VivariumError::Message(format!(
            "job {id} is {:?} and not held; nothing to release",
            job.status
        )));
    }
    if dry_run {
        println!("would request release of job {id}");
        return Ok(());
    }
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    PumpStore::open(&path)?.request_job_release(id, at)?;
    println!("release of job {id} requested; the pump applies it at its next tick");
    Ok(())
}
