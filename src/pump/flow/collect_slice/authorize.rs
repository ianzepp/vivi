//! Need-level authorization of a hand's blocked-scope request, before slice
//! coordination.
//!
//! A hand answers `blocked-scope` with paths outside its slice's write scope.
//! Before the slice coordination settles the ask (widen, wait on holders,
//! rework), [`authorize_hand`] runs the shared authorization operation (the
//! need_scope collector service): the whole request is classified with the
//! target read from the stored slice task's front matter — never from the
//! hand envelope's own metadata — journalled under the original hand job's
//! source key, and the ruling published on the need thread. An
//! operator-required ruling ([`refuse`]) holds the slice for the operator
//! under one alarm and moves nothing else: no widening, no scheduling
//! request count, no dependency, no rework, no re-dispatch. A publication
//! failure returns, and the tick leaves the source job unapplied.

use crate::error::VivariumError;
use crate::pump::flow::collect_step::RunCx;
use crate::pump::flow::handlers::FlowMail;
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::need_scope::{
    RequestTarget, ScopeAsk, ScopeContext, ScopeRequest, authorize_request,
};
use crate::pump::flow::scope::BlockedScope;
use crate::pump::flow::tick::{OPERATOR_HOLD, raise_once};
use crate::pump::jobs::JobStore;
use crate::pump::store::{PumpStore, ScopeJournalRow};

/// Exception kind: a refused blocked-scope request holds its slice for the
/// operator instead of a slicer rework. The same word the role collectors
/// raise (`collect_step::scope`), so one (kind, task) pair never
/// double-alarms; the constant lives here because that module is private.
const FLOW_SCOPE_OPERATOR: &str = "flow-scope-operator";

/// Authorize a hand's blocked-scope request at need level and return the
/// settled journal record. The source key is the original hand job with only
/// the envelope-correction suffix stripped (`envelope::correct::base_id`);
/// the target comes from the stored slice task.
///
/// # Errors
/// Returns [`VivariumError`] when the authorization operation fails, a
/// publication failure included: the caller dispatches nothing.
pub(super) fn authorize_hand<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    step: &StepRow,
    row: &StepJobRow,
    source: &str,
    request: &BlockedScope,
) -> Result<ScopeJournalRow, VivariumError> {
    let ctx = cx.ctx;
    let store = cx.store;
    let branch = store.need_branch(&cx.run.need)?;
    let context = ScopeContext {
        config: &ctx.cfg,
        project: &ctx.project,
        branch_target: branch
            .as_ref()
            .filter(|branch| !branch.target.is_empty())
            .map(|branch| branch.target.as_str()),
        reply_from: &ctx.cfg.reply_from,
    };
    let ask = ask_of(step, row, source, &cx.run.need, cx.run.run, request);
    authorize_request(store, &mut *cx.mail, &context, &ask, ctx.now())?;
    journal_of(store, source)
}

/// The authorization ask of a hand's request: the classifier's request with
/// the slice-task target source, the flow node, and the asked content the
/// journal records.
fn ask_of(
    step: &StepRow,
    row: &StepJobRow,
    source: &str,
    need: &str,
    run: u32,
    request: &BlockedScope,
) -> ScopeAsk {
    ScopeAsk {
        source: source.to_owned(),
        request: ScopeRequest {
            need: need.to_owned(),
            task: step.task.clone(),
            run,
            target: RequestTarget::SliceTask,
            paths: request.paths.clone(),
        },
        node: step.node.clone(),
        attempt: row.attempt,
        reason: request.reason.clone(),
        design: request.design,
    }
}

/// Settle a refused request: the published operator-required ruling granted
/// none of the paths, so the slice waits for the operator under one alarm
/// and nothing else moves — no widening, no scheduling count, no
/// dependency, no rework, no re-dispatch.
///
/// # Errors
/// Returns [`VivariumError`] when the wait cannot be recorded.
pub(super) fn refuse<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    step: &StepRow,
    settled: &ScopeJournalRow,
) -> Result<(), VivariumError> {
    raise_once(
        cx.ctx,
        FLOW_SCOPE_OPERATOR,
        &step.task,
        &format!(
            "hand request of step {} task {} for {} r{}: {:?} ({:?}); \
             the operator decides, not the slicer; answer with `vivi pump answer`",
            step.node, step.task, settled.need, settled.run, settled.decision, settled.issue,
        ),
    );
    cx.store
        .set_step_state(&step.task, StepState::Waiting, Some(OPERATOR_HOLD))
}

/// The journal record authorization left for `source`.
///
/// # Errors
/// Returns [`VivariumError`] when authorize left no record.
fn journal_of(store: &PumpStore, source: &str) -> Result<ScopeJournalRow, VivariumError> {
    store.scope_request(source)?.ok_or_else(|| {
        VivariumError::Other(format!(
            "scope request of job {source}: authorize left no journal record"
        ))
    })
}

#[cfg(test)]
#[path = "authorize_test.rs"]
mod tests;
