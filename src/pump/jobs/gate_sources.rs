//! Gate lifecycle bookkeeping for integration-required role jobs.

use super::JobStore;

/// Role job IDs whose merged tips still await their gate receipt.
#[must_use]
pub fn sources_waiting_for_gate(st: &impl JobStore, target: &str) -> Vec<String> {
    st.jobs()
        .values()
        .filter(|job| {
            job.integration_required
                && job.target.as_deref() == Some(target)
                && job.merged_tip.is_some()
                && job.gated_tip.is_none()
                && job.integration_error.is_none()
        })
        .map(|job| job.id.clone())
        .collect()
}

/// Whether an ungated role source needs a gate attempt at this integration tip.
#[must_use]
pub fn sources_need_gate_attempt(st: &impl JobStore, target: &str, tip: &str) -> bool {
    st.jobs().values().any(|job| {
        job.integration_required
            && job.target.as_deref() == Some(target)
            && job.merged_tip.is_some()
            && job.gated_tip.is_none()
            && job.integration_error.is_none()
            && job.gate_attempted_tip.as_deref() != Some(tip)
    })
}

/// Remember a completed gate attempt so a red result does not requeue the
/// same candidate every pump tick.
pub fn mark_gate_attempted(st: &mut impl JobStore, ids: &[String], tip: &str) {
    for id in ids {
        if let Some(job) = st.jobs_mut().get_mut(id)
            && job.integration_required
            && job.merged_tip.is_some()
            && job.gated_tip.is_none()
            && job.integration_error.is_none()
        {
            job.gate_attempted_tip = Some(tip.to_owned());
        }
    }
}

/// Record the integration tip for role branches accepted by a merge job.
pub fn mark_merged(st: &mut impl JobStore, ids: &[String], tip: &str) {
    for id in ids {
        if let Some(job) = st.jobs_mut().get_mut(id)
            && job.integration_required
            && job.merged_tip.is_none()
        {
            job.merged_tip = Some(tip.to_owned());
        }
    }
}

/// Record the clean gate covering a role job's merged tip.
pub fn mark_gated(st: &mut impl JobStore, ids: &[String], tip: &str) {
    for id in ids {
        if let Some(job) = st.jobs_mut().get_mut(id)
            && job.integration_required
            && job.merged_tip.is_some()
            && job.gated_tip.is_none()
            && job.integration_error.is_none()
        {
            job.gated_tip = Some(tip.to_owned());
        }
    }
}

/// Record a decided failure in merging a role job's pushed output.
pub fn mark_integration_failed(st: &mut impl JobStore, id: &str, detail: &str) {
    if let Some(job) = st.jobs_mut().get_mut(id)
        && job.integration_required
        && job.integration_error.is_none()
    {
        job.integration_error = Some(detail.to_owned());
    }
}
