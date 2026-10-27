//! Adopting a pre-journal scope wait when a slice resumes.
//!
//! A scope-wait written by a binary older than the scope journal has no
//! prepared request: the wait lives only in `slice_scope` (the state and the
//! holder record). Resuming it needs a source key the journal can hang the
//! stable scope round on, so the slice's recorded original hand job is
//! adopted as the source. Adoption is workflow bookkeeping only: it journals
//! the request and its recorded wait — it never publishes a need-level
//! permission note, and the historical ask is counted once, not again on
//! every tick. A wait adopted once is found through the journal from then
//! on, so a replay resumes the same continuation.

use crate::error::VivariumError;
use crate::pump::flow::index::StepRow;
use crate::pump::flow::need_scope::RulingDecision;
use crate::pump::flow::scope::{HELD_PATH_PREFIX, Resolution};
use crate::pump::flow::slice_dispatch::KIND;
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::{PumpStore, ScopeCoordinationResult, ScopeRequest};

/// The reason recorded on an adopted wait's journal row.
const ADOPTED_REASON: &str = "adopted from a scope wait written before the scope journal";

/// The source key the pre-journal scope wait of `step` resumes from: the
/// slice's recorded original hand job, validated against the job store and
/// journalled as the adoption of its historical wait. Preparing the same
/// identity again adopts the record, so a replay returns the same source.
///
/// # Errors
/// Returns [`VivariumError`] when the slice recorded no hand job to resume,
/// the recorded job is gone from the job store or is not a hand job, or a
/// store call fails.
pub(crate) fn adopt_legacy_wait<S: JobStore>(
    store: &PumpStore,
    st: &S,
    need: &str,
    step: &StepRow,
) -> Result<String, VivariumError> {
    let job = recorded_hand_job(store, st, step)?;
    let request = adopted_request(store, need, step, &job)?;
    store.prepare_scope_request(&request)?;
    store.commit_scope_coordination(&request, &adopted_candidate(store, &step.task)?)?;
    Ok(job.id)
}

/// The recorded hand job the wait resumes: the latest step job of the slice,
/// which must still be in the job store and be a hand job.
fn recorded_hand_job<S: JobStore>(
    store: &PumpStore,
    st: &S,
    step: &StepRow,
) -> Result<PoolJob, VivariumError> {
    let id = store.latest_hand_step_job(&step.task)?.ok_or_else(|| {
        VivariumError::Message(format!(
            "slice {}: scope wait names no recorded hand job to resume",
            step.task
        ))
    })?;
    let job = st.jobs().get(&id).ok_or_else(|| {
        VivariumError::Message(format!(
            "slice {}: scope wait resumes job {id}, which the job store no longer holds",
            step.task
        ))
    })?;
    if job.kind != KIND {
        return Err(VivariumError::Message(format!(
            "slice {}: scope wait resumes job {id}, which is a {} job, not a {KIND} job",
            step.task, job.kind
        )));
    }
    Ok(job.clone())
}

/// The journal request of an adopted wait: the recorded dispatch's identity
/// and the wait's held paths as the asked paths. Decision grant, no issue:
/// adoption records workflow state, it rules on nothing.
fn adopted_request(
    store: &PumpStore,
    need: &str,
    step: &StepRow,
    job: &PoolJob,
) -> Result<ScopeRequest, VivariumError> {
    let attempt = store
        .step_job(&job.id)?
        .map(|row| row.attempt)
        .ok_or_else(|| {
            VivariumError::Message(format!(
                "slice {}: scope wait resumes job {}, which has no step-job row",
                step.task, job.id
            ))
        })?;
    Ok(ScopeRequest {
        source: job.id.clone(),
        need: need.to_owned(),
        target: job.target.clone().unwrap_or_default(),
        run: step.run,
        node: step.node.clone(),
        task: step.task.clone(),
        attempt,
        paths: held_paths(store, &step.task)?,
        reason: String::from(ADOPTED_REASON),
        design: false,
        decision: RulingDecision::Grant,
        issue: None,
    })
}

/// The typed wait a legacy holder record adopts: the recorded holder tasks
/// and the recorded held paths. The free half is empty — the historical free
/// paths are already in the write scope and must not be widened again.
fn adopted_candidate(
    store: &PumpStore,
    task: &str,
) -> Result<ScopeCoordinationResult, VivariumError> {
    let mut holders = Vec::new();
    let mut held = Vec::new();
    for entry in store.scope_holders(task)? {
        match entry.strip_prefix(HELD_PATH_PREFIX).map(str::to_owned) {
            Some(path) => held.push(path),
            None => holders.push(entry),
        }
    }
    Ok(ScopeCoordinationResult {
        resolution: Resolution::WaitOn(holders),
        held,
    })
}

/// The `HELD_PATH_PREFIX` entries of the wait record: the paths the ask
/// found held. A record without any (a hand-made row) adopts empty.
fn held_paths(store: &PumpStore, task: &str) -> Result<Vec<String>, VivariumError> {
    Ok(store
        .scope_holders(task)?
        .iter()
        .filter_map(|entry| entry.strip_prefix(HELD_PATH_PREFIX).map(str::to_string))
        .collect())
}

#[cfg(test)]
#[path = "scope_replay_test.rs"]
mod tests;
