//! The role continuation of a settled scope request: regenerate the asking
//! node's real job at the same attempt.
//!
//! After [`super::authorize_request`] publishes or adopts an allowed ruling,
//! the collector calls [`continue_role`] to send the asking role out again.
//! The flow node the journal records is resolved through the live registry —
//! a registered node override wins — and its handler builds a fresh
//! [`StartCx`] brief over the current mailspace, store and run, so the
//! regenerated job sees the widened authoritative scope. The handler's own
//! kind, role and brief stand; the source job (an envelope fix, say) is never
//! copied and the attempt never moves. The only edit is the job id: the
//! journal's stable continuation identity (`<base job>-cont`), reserved once
//! per source key so a replay of the same request queues no duplicate. The
//! step_jobs row and the pool job are inserted or adopted symmetrically, the
//! existing step returns to running, and applying the job's result stays the
//! tick's job.

use crate::error::VivariumError;
use crate::pump::envelope::correct;
use crate::pump::flow::def::{FlowDef, FlowNode};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::{PayloadIdentity, RunRow, StepJobRow, StepRow, StepState};
use crate::pump::flow::outcome::{StartAction, StartCx};
use crate::pump::jobs::{self, JobStore, PoolJob};
use crate::pump::store::{PumpStore, ScopeJournalRow};

/// The suffix of a source's one stable continuation job id: `<base>-cont`.
const CONTINUATION_SUFFIX: &str = "-cont";

/// The stable continuation job id of a journaled source: the source's base
/// job id — only the envelope-correction suffix stripped — plus one fixed
/// suffix. A pure function of the source key, so every replay of the same
/// request maps to the same job.
#[must_use]
fn continuation_id(source: &str) -> String {
    format!("{}{CONTINUATION_SUFFIX}", correct::base_id(source.trim()))
}

/// Regenerate the asking role's job of a settled scope request. The caller
/// invokes this only for a published or adopted allowed ruling; nothing here
/// classifies or publishes.
///
/// The job is queued (or adopted when a replay or an interrupted run left it
/// in the pool), its step_jobs row is inserted (or adopted), and the existing
/// step is set running. Returns the stable continuation job id.
///
/// # Errors
/// Returns [`VivariumError`] when no request is journaled under `source`'s
/// base, the journal records work outside `cx`'s run, the flow has no node or
/// no registered handler for the asking node, the step row is missing or
/// drifted, the handler offers no role job, the source attempt has no
/// step_jobs row, a reserved continuation would drift, or an existing job or
/// row of the continuation id carries different work.
// Its caller is the slice collector of a settled request.
#[allow(dead_code)]
pub(crate) fn continue_role<S: JobStore, M: FlowMail>(
    cx: &StartCx<'_>,
    st: &mut S,
    registry: &Registry<S, M>,
    source: &str,
) -> Result<String, VivariumError> {
    let key = correct::base_id(source.trim());
    let row = journaled(cx.store, cx.run, source)?;
    let node = node_of(cx.def, &row)?;
    let handler = registry.handler_for(cx.def.kind, node).ok_or_else(|| {
        VivariumError::Other(format!(
            "scope continuation {key}: node {} of the {} flow has no registered step handler",
            node.id,
            cx.def.kind.name()
        ))
    })?;
    let step = step_of(cx.store, &row)?;
    let action = handler.start(cx, &step)?;
    let StartAction::Job(mut spec) = action else {
        return Err(VivariumError::Other(format!(
            "scope continuation {key}: node {} offered no role job to regenerate",
            row.node
        )));
    };
    let id = continuation_id(source);
    cx.store.reserve_scope_continuation(key, &id)?;
    let source_row = cx.store.step_job(key)?.ok_or_else(|| {
        VivariumError::Other(format!(
            "scope continuation {key}: the source attempt has no step_jobs row to continue"
        ))
    })?;
    let record = source_row.again(&id);
    ensure_journaled(&record, &row)?;
    spec.id.clone_from(&id);
    record.stamp(&cx.run.need, &mut spec.payload);
    if st.jobs().contains_key(&id) {
        ensure_pool_job(&st.jobs()[&id], &record, &cx.run.need)?;
    } else {
        jobs::enqueue(st, *spec)?;
    }
    match cx.store.step_job(&id)? {
        Some(existing) => ensure_job_row(&existing, &record)?,
        None => cx.store.insert_step_job(&cx.run.need, &record)?,
    }
    cx.store
        .set_step_state(&record.task, StepState::Running, None)?;
    Ok(id)
}

/// The journaled request of `source`, when it belongs to `run`'s need and run.
fn journaled(
    store: &PumpStore,
    run: &RunRow,
    source: &str,
) -> Result<ScopeJournalRow, VivariumError> {
    let key = correct::base_id(source.trim());
    let row = store.scope_request(key)?.ok_or_else(|| {
        VivariumError::Other(format!(
            "scope continuation {source}: no journaled scope request under {key}"
        ))
    })?;
    if row.need != run.need || row.run != run.run {
        return Err(VivariumError::Other(format!(
            "scope continuation {key}: the journal records {} r{}, not {} r{}",
            row.need, row.run, run.need, run.run
        )));
    }
    Ok(row)
}

/// The asking node of a journaled request, in `def`.
fn node_of<'a>(def: &'a FlowDef, row: &ScopeJournalRow) -> Result<&'a FlowNode, VivariumError> {
    def.nodes
        .iter()
        .find(|node| node.id == row.node)
        .ok_or_else(|| {
            VivariumError::Other(format!(
                "scope continuation {}: node {} is not in the {} flow",
                row.source,
                row.node,
                def.kind.name()
            ))
        })
}

/// The existing step row of a journaled request, when it still carries the
/// journaled node and run.
fn step_of(store: &PumpStore, row: &ScopeJournalRow) -> Result<StepRow, VivariumError> {
    let step = store.step_by_task(&row.task)?.ok_or_else(|| {
        VivariumError::Other(format!(
            "scope continuation {}: step task {} has no flow_steps row",
            row.source, row.task
        ))
    })?;
    if step.run != row.run || step.node != row.node {
        return Err(VivariumError::Other(format!(
            "scope continuation {}: step task {} records node {} r{}, not {} r{}",
            row.source, row.task, step.node, step.run, row.node, row.run
        )));
    }
    Ok(step)
}

/// The source step job's work must be the work the journal records: need,
/// run, node, task and attempt never move.
fn ensure_journaled(record: &StepJobRow, row: &ScopeJournalRow) -> Result<(), VivariumError> {
    let same = record.run == row.run
        && record.node == row.node
        && record.task == row.task
        && record.attempt == row.attempt;
    if same {
        return Ok(());
    }
    Err(VivariumError::Other(format!(
        "scope continuation {}: the journal records run {}, node {}, task {}, attempt {} \
         but its step job records run {}, node {}, task {}, attempt {}; \
         the continuation must not move the work",
        row.source,
        row.run,
        row.node,
        row.task,
        row.attempt,
        record.run,
        record.node,
        record.task,
        record.attempt
    )))
}

/// Whether a pool job already carrying the continuation id belongs to this
/// continuation: its payload identity is the step work the journal records.
fn ensure_pool_job(job: &PoolJob, record: &StepJobRow, need: &str) -> Result<(), VivariumError> {
    let identity = PayloadIdentity::of(&job.payload)
        .map_err(|err| VivariumError::Other(format!("pool job {}: {err}", job.id)))?;
    let node = job.payload.get("node").and_then(serde_json::Value::as_str);
    let mine = identity.need == need
        && identity.run == record.run
        && identity.task == record.task
        && identity.attempt == Some(record.attempt)
        && node == Some(record.node.as_str());
    if mine {
        return Ok(());
    }
    Err(VivariumError::Other(format!(
        "pool job {} does not belong to this continuation: it carries need {}, \
         run {}, node {node:?}, task {}, attempt {:?}",
        job.id, identity.need, identity.run, identity.task, identity.attempt
    )))
}

/// An existing step_jobs row of the continuation id must record the same work
/// as the row this continuation would write.
fn ensure_job_row(existing: &StepJobRow, record: &StepJobRow) -> Result<(), VivariumError> {
    if existing == record {
        return Ok(());
    }
    Err(VivariumError::Other(format!(
        "step_jobs row {} does not belong to this continuation: it records run {}, \
         node {}, task {}, attempt {}",
        existing.job, existing.run, existing.node, existing.task, existing.attempt
    )))
}

#[cfg(test)]
#[path = "continuation_test.rs"]
mod tests;
