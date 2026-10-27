//! Need-level authorization of a hand's `blocked-scope` request, before any
//! slice coordination.
//!
//! The finished hand's request is classified whole — the target read from
//! the stored slice task ([`RequestTarget::SliceTask`]), never from the
//! envelope — journalled under the original hand job's source key (only the
//! envelope-correction suffix stripped), and published as a need-thread
//! scope-ruling note, or adopted when a replay finds the ruling already
//! published. Routing reads the journal's typed decision: a grant reaches
//! slice scheduling with the normalized authorized paths; an
//! operator-required ruling holds the slice for the operator without
//! widening the need or the slice, counting a scheduling request, adding a
//! dependency, requesting rework or redispatching. A publication failure
//! returns before scheduling, so nothing dispatches on unproven permission.

use crate::error::VivariumError;
use crate::pump::flow::collect_step::RunCx;
use crate::pump::flow::handlers::FlowMail;
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::need_scope::{
    self, RequestTarget, ScopeAsk, ScopeContext, ScopeIssue, authorize_request,
};
use crate::pump::flow::scope::BlockedScope;
use crate::pump::flow::tick::OPERATOR_HOLD;
use crate::pump::jobs::JobStore;
use crate::pump::store::ScopeJournalRow;

/// Exception kind raised when a blocked-scope request needs an operator.
pub(crate) const SCOPE_OPERATOR: &str = "flow-scope-operator";

/// What need-level authorization settled for a hand's request.
pub(crate) enum Authorized {
    /// Durable permission exists on the need thread: scheduling may settle
    /// the request over these normalized authorized paths.
    Allowed {
        /// The configured target the request writes in.
        target: String,
        /// The complete requested set, normalized and deduplicated.
        paths: Vec<String>,
    },
    /// The request was refused whole: its operator-required ruling is on
    /// record and the slice waits for the operator.
    Forbidden,
}

/// Authorize the `blocked-scope` `request` of the finished hand job
/// `source` at need level, before slice coordination.
///
/// `source` is the original hand job's id; the journal keys the request by
/// its correction-suffix-stripped base. On a grant the target and the
/// normalized paths are read back from the journal, so a replay schedules
/// exactly what was published.
///
/// # Errors
/// Returns [`VivariumError`] when the need branch, the classification, the
/// journal or the publication fails; a failed publication schedules
/// nothing.
pub(crate) fn authorize<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    step: &StepRow,
    row: &StepJobRow,
    source: &str,
    request: &BlockedScope,
) -> Result<Authorized, VivariumError> {
    let ask = ask_of(&cx.run.need, cx.run.run, step, row, source, request);
    let branch_target = cx
        .store
        .need_branch(&cx.run.need)?
        .map(|branch| branch.target);
    let context = ScopeContext {
        config: &cx.ctx.cfg,
        project: &cx.ctx.project,
        branch_target: branch_target.as_deref(),
        reply_from: &cx.ctx.cfg.reply_from,
    };
    authorize_request(cx.store, cx.mail, &context, &ask, cx.ctx.now())?;
    let decided = cx
        .store
        .scope_request(&ask.source)?
        .ok_or_else(|| missing_journal(&ask.source))?;
    if decided.decision.is_grant() {
        return Ok(Authorized::Allowed {
            target: decided.target,
            paths: decided.paths,
        });
    }
    hold_for_operator(cx, step, source, &decided)?;
    Ok(Authorized::Forbidden)
}

/// The authorization ask of a hand request: the classifier's request with
/// the slice task's own target, the work identity, and the asked content.
fn ask_of(
    need: &str,
    run: u32,
    step: &StepRow,
    row: &StepJobRow,
    source: &str,
    request: &BlockedScope,
) -> ScopeAsk {
    ScopeAsk {
        source: source.to_owned(),
        request: need_scope::ScopeRequest {
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

/// Record why the request waits for the operator and hold the slice: the
/// sticky operator verdict is the only scheduling effect of a refusal.
fn hold_for_operator<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    step: &StepRow,
    source: &str,
    decided: &ScopeJournalRow,
) -> Result<(), VivariumError> {
    cx.ctx.exception(
        SCOPE_OPERATOR,
        &step.task,
        &format!(
            "hand job {source}: the blocked-scope request needs an operator ({}): {}",
            issue_text(decided.issue),
            decided.reason
        ),
    );
    cx.store
        .set_step_state(&step.task, StepState::Waiting, Some(OPERATOR_HOLD))
}

/// The typed issue of a journalled operator-required ruling, as words.
fn issue_text(issue: Option<ScopeIssue>) -> &'static str {
    match issue {
        Some(ScopeIssue::UnknownTarget) => "unknown target",
        Some(ScopeIssue::TargetMismatch) => "target mismatch",
        Some(ScopeIssue::InvalidPath) => "invalid path",
        Some(ScopeIssue::Denied) => "denied path",
        Some(ScopeIssue::OutsideAllow) => "outside the allow list",
        Some(ScopeIssue::UnverifiablePath) => "unverifiable path",
        None => "operator decision",
    }
}

fn missing_journal(source: &str) -> VivariumError {
    VivariumError::Other(format!(
        "scope journal: authorization of {source} recorded no decision"
    ))
}

#[cfg(test)]
#[path = "authorize_test.rs"]
mod tests;
