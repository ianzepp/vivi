//! The live seam of the band runtime: acquiring a pool's dispatch guard and
//! reading the real evidence an [`Availability`] snapshot summarizes — the
//! mailspace event log, its recorded cooldown deadlines, and the provider's
//! own census. Nothing here draws a model, starts a job, or fabricates an
//! observation: a census that fails is never read as an empty one.

use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::pool::provider::PoolProvider;
use crate::pool::record;
use crate::pool::run;
use crate::pool::slot::PoolId;

use super::cooldown;
use super::guard::DispatchGuard;
use super::runtime::{Availability, Decision, Wait, WaitReason};

/// One acquired dispatch: the pool's [`DispatchGuard`], still held after
/// `acquire` returns and released when this value is dropped, with the
/// evidence snapshot that was read while holding the guard.
#[derive(Debug)]
pub struct Admission {
    /// The pool's dispatch guard. A second dispatch of the same pool in
    /// this mailspace waits until this is dropped.
    pub guard: DispatchGuard,
    /// The availability snapshot taken under the guard.
    pub availability: Availability,
}

/// Take `id`'s pool dispatch guard below the mailspace, then read the live
/// admission evidence while holding it: the mailspace's open dispatch
/// records, its cooldown deadlines folded across every pool, and the
/// provider's census of `id`. Contention answers
/// [`WaitReason::DispatchBusy`] at once; a census that fails answers a
/// conservative [`WaitReason::BandUnavailable`] and releases the guard
/// rather than granting capacity on an empty reading. No model is drawn
/// and no job is started.
///
/// # Errors
/// Returns an error when the guard cannot be probed or the event log
/// cannot be read; contention and a failed census are answers, not errors.
pub fn acquire(
    space: &Mailspace,
    provider: &dyn PoolProvider,
    id: &PoolId,
    now: u64,
) -> Result<Decision<Admission>, VivariumError> {
    let Some(guard) = DispatchGuard::try_acquire(&space.dir, &id.pool)? else {
        return Ok(Decision::Waiting(Wait {
            reason: WaitReason::DispatchBusy,
            retry_at: None,
        }));
    };
    let events = space.pool_events()?;
    let open = record::fold_open(&events);
    let deadlines = cooldown::fold(&events, now);
    // A failed listing is not an empty one: never grant capacity, and never
    // turn one failed census into a permanent refusal.
    let Ok(slots) = run::pool_status(provider, id) else {
        return Ok(Decision::Waiting(Wait {
            reason: WaitReason::BandUnavailable,
            retry_at: None,
        }));
    };
    let availability = Availability::from_evidence(&id.pool, &open, &slots, deadlines, now);
    Ok(Decision::Ready(Admission {
        guard,
        availability,
    }))
}

#[cfg(test)]
#[path = "live_test.rs"]
mod tests;
