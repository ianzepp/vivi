//! Operator recovery of a need branch held `unverified`, and the once-per-hold
//! announcement of a held branch.

use crate::error::VivariumError;
use crate::pump::state::State;

use super::{NeedBranch, NeedBranchStatus, clear_freeze, is_full_sha, require, set_branch_tip};
use super::{state_error, write};

/// Mark the hold on `target`/`need` as announced and say whether this call is
/// the first: the pump logs a hold once, not on every tick. A branch with no
/// record, or one that is not held, is never announced.
pub fn announce_hold(st: &mut State, target: &str, need: &str) -> Result<bool, VivariumError> {
    let Some(mut record) = super::get(st, target, need)? else {
        return Ok(false);
    };
    if record.hold_announced
        || !matches!(
            record.status,
            NeedBranchStatus::Collision | NeedBranchStatus::Unverified
        )
    {
        return Ok(false);
    }
    record.hold_announced = true;
    write(st, record)?;
    Ok(true)
}

/// Adopt the remote tip an `unverified` hold refused: it becomes the branch's
/// trusted tip, every `unverified_*` field and the error clear, the invalid
/// receipt streak ends, and the status returns to `active`, where ordinary
/// dispatch resumes. Returns the record as it was held, so the caller can
/// report the old trusted tip.
///
/// `observed` is the remote ref as the caller just read it; it must equal the
/// tip the hold recorded.
///
/// # Errors
/// Returns an error when the branch is not `unverified`, or `observed` is not
/// the recorded unverified tip. The record is untouched then.
pub fn recover_unverified(
    st: &mut State,
    target: &str,
    need: &str,
    observed: &str,
) -> Result<NeedBranch, VivariumError> {
    let held = require(st, target, need)?;
    if held.status != NeedBranchStatus::Unverified
        || !is_full_sha(observed)
        || held.observed_unverified_tip_sha.as_deref() != Some(observed)
    {
        return Err(state_error(
            "recovery needs an unverified branch and its recorded unverified tip",
        ));
    }
    let mut record = held.clone();
    clear_freeze(&mut record);
    set_branch_tip(&mut record, observed);
    record.status = NeedBranchStatus::Active;
    record.unverified_base_sha = None;
    record.observed_unverified_tip_sha = None;
    record.unverified_job_id = None;
    record.unverified_detail = None;
    record.hold_announced = false;
    record.error = None;
    record.invalid_receipts.clear();
    write(st, record)?;
    Ok(held)
}
