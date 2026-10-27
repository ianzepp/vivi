//! Intercepting a planner's or an admitter's structured permission request
//! before its escalation word advances the flow.
//!
//! A planner `operator` or admitter `escalate` reply may carry the typed
//! scope request (`scope_target` + `scope_paths`) when path permission is
//! the sole blocker. [`intercept`] settles such a request through the shared
//! authorization operation ([`authorize_request`]: classify, journal, then
//! publish or adopt the need-thread ruling) before any word advances:
//!
//! - An allowed ruling — published, adopted, or already held — regenerates
//!   the asking role's job at the same attempt through its registered
//!   handler ([`continue_role`]). The step never routes onward, no attempt
//!   is consumed, no new run starts, and no envelope-fix job is queued.
//! - A refused ruling (an operator-required decision granting none of the
//!   paths) is published first. A planner keeps the ordinary `operator`
//!   route and parks; an admitter never routes its policy exception to the
//!   head-cto — the step waits with [`OPERATOR_HOLD`] and one
//!   [`FLOW_SCOPE_OPERATOR`] alarm.
//!
//! A reply without the structured fields is untouched: ordinary
//! `operator`/`escalate` and design replies advance exactly as before. A
//! publication failure is returned: nothing advances, nothing mutates beyond
//! the prepared journal record, and the tick leaves the source job unapplied
//! for the next collection.

use serde::Deserialize;

use crate::error::VivariumError;
use crate::pump::envelope::correct;
use crate::pump::flow::collect_step::RunCx;
use crate::pump::flow::def::NodeClass;
use crate::pump::flow::handlers::FlowMail;
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::need_scope::{
    RequestTarget, ScopeAsk, ScopeContext, ScopeRequest, authorize_request, continue_role,
};
use crate::pump::flow::outcome::StartCx;
use crate::pump::flow::tick::{OPERATOR_HOLD, raise_once};
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::ScopeJournalRow;

/// Exception kind: a refused structured scope request holds its step for the
/// operator instead of escalating to the head-cto.
pub(crate) const FLOW_SCOPE_OPERATOR: &str = "flow-scope-operator";

/// The typed scope-request fields of a planner `operator` or admitter
/// `escalate` front matter. The envelope parser has already validated them:
/// both request fields appear together, the target is nonblank, the paths
/// are nonempty and nonblank, and only those two answers may carry them.
#[derive(Debug, Default, Deserialize)]
struct RequestFields {
    #[serde(default)]
    scope_target: Option<String>,
    #[serde(default)]
    scope_paths: Option<Vec<String>>,
    #[serde(default)]
    reason: Option<String>,
}

/// The structured scope request a role reply carries.
struct RoleRequest {
    target: String,
    paths: Vec<String>,
    reason: String,
}

/// Whether `class` is a role whose envelope may carry a structured scope
/// request.
fn intercepts(class: &NodeClass) -> bool {
    matches!(class, NodeClass::Planner | NodeClass::Admitter)
}

/// The structured request of `payload`'s front matter, when it carries one.
/// Typed metadata only: the fields the validated envelope serialized, never
/// the reply's prose.
fn request_of(payload: &serde_json::Value) -> Result<Option<RoleRequest>, VivariumError> {
    let Some(front) = payload.get("front") else {
        return Ok(None);
    };
    let fields: RequestFields = serde_json::from_value(front.clone())
        .map_err(|error| VivariumError::Parse(format!("scope request front: {error}")))?;
    let (Some(target), Some(paths)) = (fields.scope_target, fields.scope_paths) else {
        return Ok(None);
    };
    if target.trim().is_empty() || paths.is_empty() {
        return Ok(None);
    }
    Ok(Some(RoleRequest {
        target,
        paths,
        reason: fields.reason.unwrap_or_default(),
    }))
}

/// Intercept a collected role outcome that carries a structured scope
/// request. Returns `false` when the reply has no request and advances
/// ordinarily. A settled allowed request continues the asking role and the
/// step never advances; a settled refused request parks a planner or holds
/// an admitter for the operator.
pub(crate) fn intercept<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    job: &PoolJob,
    row: &StepJobRow,
    step: &StepRow,
    class: &NodeClass,
    word: &str,
    payload: &serde_json::Value,
) -> Result<bool, VivariumError> {
    if !intercepts(class) {
        return Ok(false);
    }
    let Some(request) = request_of(payload)? else {
        return Ok(false);
    };
    authorize(cx, job, row, step, request)?;
    let settled = cx
        .store
        .scope_request(&correct::base_id(&job.id))?
        .ok_or_else(|| {
            VivariumError::Other(format!(
                "scope request of job {}: authorize left no journal record",
                job.id
            ))
        })?;
    if settled.decision.is_grant() {
        continue_the_role(cx, &job.id)?;
        return Ok(true);
    }
    refused(cx, step, class, word, payload, &settled)?;
    Ok(true)
}

/// Classify, journal and publish (or adopt) the ruling of `request` through
/// the shared authorization operation. Nothing here advances the step: a
/// failure returns and the tick leaves the source job unapplied.
fn authorize<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    job: &PoolJob,
    row: &StepJobRow,
    step: &StepRow,
    request: RoleRequest,
) -> Result<(), VivariumError> {
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
    let ask = ScopeAsk {
        source: job.id.clone(),
        request: ScopeRequest {
            need: cx.run.need.clone(),
            task: step.task.clone(),
            run: cx.run.run,
            target: RequestTarget::Named(request.target),
            paths: request.paths,
        },
        node: step.node.clone(),
        attempt: row.attempt,
        reason: request.reason,
        design: false,
    };
    authorize_request(store, &mut *cx.mail, &context, &ask, ctx.now()).map(|_| ())
}

/// Regenerate the asking role's job at the same attempt through its
/// registered handler: the published or adopted grant widened the need's
/// scope, so the fresh brief reads it and the step never advances.
fn continue_the_role<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    source: &str,
) -> Result<(), VivariumError> {
    let start = StartCx {
        ctx: cx.ctx,
        store: cx.store,
        mail: &*cx.mail,
        run: &cx.run,
        def: &cx.def,
    };
    continue_role(&start, cx.st, cx.registry, source).map(|_| ())
}

/// Settle a refused request: the operator-required ruling is published and
/// grants none of the paths. A planner keeps the ordinary `operator` route
/// (parked); an admitter never routes its policy exception to the head-cto —
/// the step waits for the operator under one alarm.
fn refused<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    step: &StepRow,
    class: &NodeClass,
    word: &str,
    payload: &serde_json::Value,
    settled: &ScopeJournalRow,
) -> Result<(), VivariumError> {
    if *class == NodeClass::Planner {
        return cx.advance_word(step, word, payload.clone());
    }
    raise_once(
        cx.ctx,
        FLOW_SCOPE_OPERATOR,
        &step.task,
        &format!(
            "admitter request of step {} task {} for {} r{}: {:?} ({:?}); \
             the operator decides, not the cto; answer with `vivi pump answer`",
            step.node, step.task, settled.need, settled.run, settled.decision, settled.issue,
        ),
    );
    cx.store
        .set_step_state(&step.task, StepState::Waiting, Some(OPERATOR_HOLD))
}

#[cfg(test)]
#[path = "scope_test.rs"]
mod tests;
