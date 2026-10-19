//! Verified landing receipts and safe need-branch cleanup lifecycle.

use serde::{Deserialize, Serialize};

use crate::error::VivariumError;
use crate::pump::state::State;

use super::{NeedAuditBinding, NeedBranchStatus, binding_for};

/// Verified target publication receipt from the landing role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NeedLandingReceipt {
    /// Full need branch identity that was accepted.
    pub binding: NeedAuditBinding,
    /// Stable acceptance receipt identity.
    pub acceptance_receipt_key: String,
    /// Audit job that produced the accepted receipt.
    pub acceptance_job_id: String,
    /// Accepted verdict recorded by the router.
    pub verdict: String,
    /// Durable landing job identity.
    pub landing_job_id: String,
    /// Exact target ref, for example `refs/heads/next`.
    pub target_ref: String,
    /// Candidate commit submitted to the runner.
    pub candidate_sha: String,
    /// Commit reported by the privileged publisher.
    pub runner_published_sha: String,
    /// Independently observed target ref after publication.
    pub observed_target_sha: String,
    /// Whether the accepted need tip is an ancestor of the target merge candidate.
    pub accepted_tip_in_candidate: bool,
    /// Whether the target currently contains the published candidate.
    pub candidate_in_observed_target: bool,
    /// Merge role report that explains the verified landing.
    pub merge_report: String,
}

/// Verified runner receipt from deleting the landed need ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NeedCleanupReceipt {
    /// Durable cleanup job identity.
    pub cleanup_job_id: String,
    /// Exact need ref submitted for deletion.
    pub branch_ref: String,
    /// Frozen tip the cleanup job was authorized to remove.
    pub expected_sha: String,
    /// Runner's terminal deletion state.
    pub runner_state: String,
    /// Ref tip seen before deletion, when it existed.
    pub before_sha: Option<String>,
    /// Independent proof that the ref is now absent.
    pub verified_absent: bool,
    /// Receipt completion time.
    pub completed_at: u64,
}

/// Whether a landed branch may receive another cleanup job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupRetryState {
    /// Landing has not been verified yet.
    NotLanded,
    /// A cleanup job currently owns the ref.
    InFlight,
    /// Cleanup completed and absence was verified.
    Complete,
    /// The first cleanup attempt may be queued.
    Ready,
    /// A transient cleanup failure may be retried with a new job ID.
    Retryable,
    /// Cleanup is held for an explicit decision, such as a ref mismatch.
    Held,
}

/// Lock the exact frozen binding while a durable landing job is pending.
pub fn reserve_landing(
    st: &mut State,
    target: &str,
    need: &str,
    binding: &NeedAuditBinding,
    job_id: &str,
) -> Result<(), VivariumError> {
    let mut record = super::require(st, target, need)?;
    if binding_for(st, target, need)?.as_ref() != Some(binding) || job_id.is_empty() {
        return Err(super::state_error(
            "landing reservation does not match the current frozen binding",
        ));
    }
    if record
        .landing_job_id
        .as_deref()
        .is_some_and(|current| current != job_id)
    {
        return Err(super::state_error(
            "another landing job already owns this need branch",
        ));
    }
    record.landing_job_id = Some(job_id.to_owned());
    record.landing_error = None;
    super::write(st, record)?;
    Ok(())
}

/// Release an unsuccessful landing reservation before verified inclusion.
pub fn release_landing(
    st: &mut State,
    target: &str,
    need: &str,
    job_id: &str,
    reason: &str,
) -> Result<(), VivariumError> {
    let mut record = super::require(st, target, need)?;
    if record.landing_job_id.as_deref() != Some(job_id) || record.landing_receipt.is_some() {
        return Err(super::state_error(
            "landing reservation cannot be released after verified inclusion",
        ));
    }
    record.landing_job_id = None;
    record.landing_error = Some(reason.to_owned());
    super::write(st, record)?;
    Ok(())
}

/// Persist a verified target inclusion receipt for the locked accepted binding.
pub fn record_landing(
    st: &mut State,
    target: &str,
    need: &str,
    job_id: &str,
    receipt: NeedLandingReceipt,
) -> Result<(), VivariumError> {
    let mut record = super::require(st, target, need)?;
    let Some(binding) = binding_for(st, target, need)? else {
        return Err(super::state_error(
            "landing receipt targets a stale or unfrozen binding",
        ));
    };
    if record.landing_job_id.as_deref() != Some(job_id)
        || receipt.landing_job_id != job_id
        || receipt.binding != binding
        || !super::is_full_sha(&receipt.candidate_sha)
        || receipt.runner_published_sha != receipt.candidate_sha
        || !super::is_full_sha(&receipt.observed_target_sha)
        || !receipt.accepted_tip_in_candidate
        || !receipt.candidate_in_observed_target
        || receipt.acceptance_receipt_key.is_empty()
        || receipt.acceptance_job_id.is_empty()
        || !matches!(receipt.verdict.as_str(), "clean_pass" | "residual")
        || receipt.target_ref.is_empty()
    {
        return Err(super::state_error(
            "landing receipt does not prove the accepted tip reached the target",
        ));
    }
    record.landing_receipt = Some(receipt);
    record.landing_error = None;
    super::write(st, record)?;
    Ok(())
}

/// Bind a durable cleanup job to a verified landing.
pub fn record_cleanup_job(
    st: &mut State,
    target: &str,
    need: &str,
    job_id: &str,
) -> Result<(), VivariumError> {
    let mut record = super::require(st, target, need)?;
    if record.landing_receipt.is_none()
        || record.status != NeedBranchStatus::Frozen
        || job_id.is_empty()
        || record.cleanup_receipt.is_some()
        || (record.cleanup_error.is_some() && !record.cleanup_retryable)
    {
        return Err(super::state_error(
            "cleanup job requires a verified landing receipt",
        ));
    }
    if record
        .cleanup_job_id
        .as_deref()
        .is_some_and(|current| current != job_id)
    {
        return Err(super::state_error(
            "another cleanup job already owns this branch",
        ));
    }
    record.cleanup_job_id = Some(job_id.to_owned());
    record.cleanup_error = None;
    record.cleanup_retryable = false;
    super::write(st, record)?;
    Ok(())
}

/// Release a failed cleanup job while retaining the landed branch and receipt.
pub fn release_cleanup_job(
    st: &mut State,
    target: &str,
    need: &str,
    job_id: &str,
    reason: &str,
    retryable: bool,
) -> Result<(), VivariumError> {
    let mut record = super::require(st, target, need)?;
    if record.cleanup_job_id.as_deref() != Some(job_id) || record.cleanup_receipt.is_some() {
        return Err(super::state_error(
            "cleanup job cannot be released after verified deletion",
        ));
    }
    record.cleanup_job_id = None;
    record.cleanup_error = Some(reason.to_owned());
    record.cleanup_retryable = retryable;
    super::write(st, record)?;
    Ok(())
}

/// Mark the need branch landed only after exact-ref deletion is verified.
pub fn finish_cleanup(
    st: &mut State,
    target: &str,
    need: &str,
    job_id: &str,
    receipt: NeedCleanupReceipt,
) -> Result<(), VivariumError> {
    let mut record = super::require(st, target, need)?;
    if record.cleanup_job_id.as_deref() != Some(job_id)
        || receipt.cleanup_job_id != job_id
        || receipt.branch_ref != format!("refs/heads/{}", record.branch)
        || receipt.expected_sha != record.frozen_tip_sha.as_deref().unwrap_or_default()
        || !receipt.verified_absent
        || !matches!(receipt.runner_state.as_str(), "deleted" | "already-absent")
    {
        return Err(super::state_error(
            "cleanup receipt does not prove exact need-branch deletion",
        ));
    }
    if receipt.runner_state == "deleted"
        && receipt.before_sha.as_deref() != Some(receipt.expected_sha.as_str())
    {
        return Err(super::state_error(
            "cleanup deleted a ref whose pre-delete tip did not match the frozen tip",
        ));
    }
    record.cleanup_receipt = Some(receipt);
    record.status = NeedBranchStatus::Landed;
    record.cleanup_error = None;
    record.cleanup_retryable = false;
    super::write(st, record)?;
    Ok(())
}

/// Current typed cleanup retry state for a need branch.
pub fn cleanup_retry_state(
    st: &State,
    target: &str,
    need: &str,
) -> Result<CleanupRetryState, VivariumError> {
    let record = super::require(st, target, need)?;
    Ok(if record.cleanup_receipt.is_some() {
        CleanupRetryState::Complete
    } else if record.landing_receipt.is_none() {
        CleanupRetryState::NotLanded
    } else if record.cleanup_job_id.is_some() {
        CleanupRetryState::InFlight
    } else if record.cleanup_error.is_none() {
        CleanupRetryState::Ready
    } else if record.cleanup_retryable {
        CleanupRetryState::Retryable
    } else {
        CleanupRetryState::Held
    })
}
