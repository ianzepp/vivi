//! Operator release of held jobs: `vivi pump requeue --job <id>` writes a
//! request into `pump.db`, and each dispatch pass applies the requests here.

use super::{JobStatus, JobStore};
use crate::error::VivariumError;
use crate::pump::host::Ctx;
use crate::pump::store::{DB_FILENAME, PumpStore};

/// Release every held job after a config reload changed the config. A job is
/// held after repeated pool refusals, whose message says to fix the cause and
/// then requeue; a changed config is that fix, so the jobs go back to the
/// dispatcher. A job the new config still cannot place is refused and held
/// again.
pub(in crate::pump) fn release_held_after_reload(ctx: &Ctx, st: &mut impl JobStore) {
    for (id, job) in st.jobs_mut().iter_mut() {
        if job.status != JobStatus::Pending || job.held.is_none() {
            continue;
        }
        job.held = None;
        job.dispatch_refusals = 0;
        ctx.log(&format!("job {id} released: the config changed"));
        ctx.resolve("job-dispatch-refused", id, "released after a config reload");
    }
}

/// Release every held job an operator asked to release. A home with no
/// `pump.db` has no requests.
///
/// # Errors
/// Returns the database error that prevents reading the requests.
pub(super) fn apply_releases(ctx: &Ctx, st: &mut impl JobStore) -> Result<(), VivariumError> {
    let path = ctx.home.join(DB_FILENAME);
    if !path.is_file() {
        return Ok(());
    }
    for id in PumpStore::open(&path)?.take_job_releases()? {
        let held = st
            .jobs_mut()
            .get_mut(&id)
            .filter(|job| job.status == JobStatus::Pending && job.held.is_some());
        let Some(job) = held else {
            ctx.log(&format!("job release ignored: {id} is not held"));
            continue;
        };
        job.held = None;
        job.dispatch_refusals = 0;
        ctx.log(&format!("job {id} released by operator"));
        ctx.resolve("job-dispatch-refused", &id, "released by operator");
    }
    Ok(())
}
