//! Adjudication of need-scope requests: the shared flow-facing operation a
//! caller reaches for when a hand asks for paths outside its write scope.
//!
//! One call takes the request identity (who asked, on which need, run, node
//! and task), the role, the named target, the requested paths and the reason,
//! and settles it: an existing ruling for the originating job identity is
//! reused as stored, otherwise the request is evaluated against the latest
//! valid policy of the target it names. Requests must name configured
//! targets, and a need that explicitly names a different target is refused
//! with the typed mismatch rejection — policy and grants are never borrowed
//! from another target. The decision is persisted in `scope_rulings` (the
//! durable grant authority) and its ruling letter is posted on the need
//! thread with `MailWriter::reply` before the call returns success.
//!
//! Envelope corrections reuse the originating identity: a correction job id
//! is cut back to its base before it keys the ruling, so a corrected request
//! replays the original row. The deterministic decision reference and the
//! exact letter body are saved in the store before the post, so a retry
//! recognizes an already-posted ruling through the reply readers and never
//! reposts. Thread evidence only deduplicates an already-saved decision; it
//! never manufactures a grant.
//!
//! The module also carries the two pieces a role collector needs to act on a
//! settled grant: extraction of the structured request from a validated
//! planner `operator` or admitter `escalate` payload (the structured scope
//! fields only — free-form reasons are never scanned for paths), and the
//! same-attempt continuation that restarts the requesting step through its
//! registered handler so the fresh brief sees the effective scope.

use std::collections::BTreeMap;

use crate::error::VivariumError;
use crate::letter::Envelope;
use crate::letter::read::need_of;
use crate::letter::work::NeedFront;
use crate::pump::config::PumpTarget;
use crate::pump::config::scope::{self, ScopeAuthorization, ScopePolicy, ScopeRejection};
use crate::pump::envelope::correct;
use crate::pump::envelope::review::{Admitter, AdmitterAnswer};
use crate::pump::envelope::roles::{Planner, PlannerAnswer};
use crate::pump::flow::def::FlowDef;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::outcome::{StartAction, StartCx};
use crate::pump::flow::slice_dispatch::SliceMail;
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobSpec, JobStore};
use crate::pump::store::PumpStore;
use crate::pump::store::scope_ruling::{
    RulingDecision, ScopeRulingRequest, ScopeRulingRow, continuation_id,
};

/// The stable exception kind for the hand integration: raised when a scope
/// request ends without a usable grant and the operator must settle it.
pub const SCOPE_OPERATOR: &str = "flow-scope-operator";

/// The first line of every ruling letter; the reply readers key on it.
pub const RULING_MARKER: &str = "pump: scope ruling ";

/// A scope request as the flow receives it: who asked, what, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRequest {
    /// The job id the request arrived under; a correction id is cut back to
    /// its base before it keys the ruling.
    pub job: String,
    /// The kind of the originating request job.
    pub job_kind: String,
    /// The need the request belongs to; the ruling letter is posted here.
    pub need: String,
    /// The run the request belongs to.
    pub run: u32,
    /// The flow node the request came from.
    pub node: String,
    /// The task handle that made the request.
    pub task: String,
    /// The role that made the request.
    pub role: String,
    /// The target the requested paths are relative to.
    pub target: String,
    /// The requested path declarations, as requested.
    pub paths: Vec<String>,
    /// The reason the request stated.
    pub reason: String,
}

/// What adjudicating a request produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settled {
    /// The stored ruling: the durable grant authority for the request.
    pub ruling: ScopeRulingRow,
    /// True when the store already held this ruling before the call: the
    /// decision was replayed, never re-evaluated.
    pub reused: bool,
    /// True when the ruling letter was already on the thread and was
    /// recognized instead of reposted.
    pub already_posted: bool,
}

/// Adjudicate `request` and post its ruling on the need thread.
///
/// The ruling is looked up by the request's originating job identity; a
/// fresh request is evaluated against the named target's policy and stored.
/// The decision letter body is saved in the store before it is posted, so a
/// retry recognizes the already-posted letter through the reply readers.
/// Database and mail failures propagate: a call that returns `Ok` has its
/// ruling stored, its letter saved and posted, and the post recorded.
///
/// # Errors
/// Returns [`VivariumError`] when the need root does not parse, or a store
/// or mail operation fails.
pub fn adjudicate<M: SliceMail + MailWriter>(
    ctx: &Ctx,
    configs: &BTreeMap<String, PumpTarget>,
    store: &PumpStore,
    mail: &mut M,
    request: &ScopeRequest,
) -> Result<Settled, VivariumError> {
    let job = String::from(correct::base_id(&request.job));
    let (mut ruling, reused) = match store.scope_ruling(&job)? {
        Some(row) => (row, true),
        None => {
            let front = need_front(mail, &request.need)?;
            let (decision, policy_reason, policy) =
                evaluate(configs, front.front.target.as_deref(), request);
            let row = store.insert_scope_ruling(&ScopeRulingRequest {
                job,
                job_kind: request.job_kind.clone(),
                need: request.need.clone(),
                run: request.run,
                node: request.node.clone(),
                task: request.task.clone(),
                role: request.role.clone(),
                target: request.target.clone(),
                paths: request.paths.clone(),
                reason: request.reason.clone(),
                decision,
                policy_reason,
                policy,
            })?;
            (row, false)
        }
    };
    let body = match ruling.reply_body.clone() {
        Some(body) => body,
        None => {
            let body = drafted(store, mail, &ruling)?;
            store.set_ruling_reply(&ruling.job, &body)?;
            body
        }
    };
    let already_posted = posted(mail, &request.need, &body)?;
    if !already_posted {
        mail.reply(&request.need, &body)?;
    }
    store.mark_ruling_posted(&ruling.job, unix(ctx.now()))?;
    if let Some(row) = store.scope_ruling(&ruling.job)? {
        ruling = row;
    }
    Ok(Settled {
        ruling,
        reused,
        already_posted,
    })
}

/// Who and where a request came from, before its payload is read: the
/// identity the collector holds for the finished job whose reply carried
/// the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestOrigin {
    /// The id of the finished job whose reply carried the request.
    pub job: String,
    /// The kind of that job.
    pub job_kind: String,
    /// The need the request belongs to.
    pub need: String,
    /// The run the request belongs to.
    pub run: u32,
    /// The flow node the request came from.
    pub node: String,
    /// The step task that made the request.
    pub task: String,
    /// The role that made the request.
    pub role: String,
}

/// The structured scope request of a validated planner payload: `Some` only
/// for an `operator` answer that carries both scope fields. The payload has
/// already passed validation, so a requesting answer always has a reason;
/// it is carried as the request reason, never scanned for paths.
#[must_use]
pub fn planner_request(origin: RequestOrigin, payload: &Planner) -> Option<ScopeRequest> {
    structured(
        origin,
        payload.answer == PlannerAnswer::Operator,
        &payload.scope_target,
        &payload.scope_paths,
        payload.reason.as_deref(),
    )
}

/// The structured scope request of a validated admitter payload: `Some`
/// only for an `escalate` answer that carries both scope fields.
#[must_use]
pub fn admitter_request(origin: RequestOrigin, payload: &Admitter) -> Option<ScopeRequest> {
    structured(
        origin,
        payload.answer == AdmitterAnswer::Escalate,
        &payload.scope_target,
        &payload.scope_paths,
        payload.reason.as_deref(),
    )
}

/// The request shared by both extractions: only a requesting answer with
/// both structured fields present is one. An ordinary outcome gives `None`
/// and is never intercepted here.
fn structured(
    origin: RequestOrigin,
    requesting: bool,
    target: &Option<String>,
    paths: &Option<Vec<String>>,
    reason: Option<&str>,
) -> Option<ScopeRequest> {
    if !requesting {
        return None;
    }
    Some(ScopeRequest {
        job: origin.job,
        job_kind: origin.job_kind,
        need: origin.need,
        run: origin.run,
        node: origin.node,
        task: origin.task,
        role: origin.role,
        target: target.clone()?,
        paths: paths.clone()?,
        reason: reason.unwrap_or_default().to_string(),
    })
}

/// A same-attempt continuation started for an approved scope request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Continued {
    /// The deterministic continuation job id: [`continuation_id`] of the
    /// originating request job, the next id in its correction chain.
    pub job: String,
    /// True when the continuation job was already enqueued: its stored
    /// inputs were reused as they are and only missing index and progress
    /// writes were repaired.
    pub reused: bool,
}

/// Continue a role step in place after its approved request: restart the
/// step through its registered handler so the fresh brief sees the
/// effective scope and all current inputs, then enqueue the handler's job
/// under a deterministic continuation identity at the same task, run and
/// attempt. The step stays open and running and the run stays live:
/// attempts are never incremented and no rerun is issued.
///
/// Progress is persisted through the ruling store: the continuation
/// identity is written before the job is enqueued and the completion mark
/// after it, so a crash on the way leaves a recoverable gap.
///
/// A replay repairs gaps without duplicating work. When the continuation
/// job is already enqueued, its stored inputs are reused instead of
/// rebuilding a possibly conflicting brief, and only the missing index row
/// and ruling progress are written back.
///
/// # Errors
/// Returns [`VivariumError`] when the step's node is not in the loaded
/// flow, no handler is registered for the node, the handler does not offer
/// a job, no ruling for the request job exists, or a store or job operation
/// fails. Every error propagates before the caller advances anything.
pub(crate) fn continue_role<S: JobStore, M: SliceMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &mut M,
    registry: &Registry<S, M>,
    def: &FlowDef,
    run: &RunRow,
    row: &StepJobRow,
    step: &StepRow,
    request: &ScopeRequest,
) -> Result<Continued, VivariumError> {
    let id = String::from(continuation_id(&request.job));
    let at = unix(ctx.now());
    if st.jobs().contains_key(&id) {
        repair(store, run, row, &request.job, &id, at)?;
        return Ok(Continued {
            job: id,
            reused: true,
        });
    }
    store.set_ruling_continuation(&request.job, &id)?;
    let spec = resumed_brief(ctx, store, mail, registry, def, run, step, request)?;
    enqueue_continuation(st, store, run, row, step, *spec, &id)?;
    store.mark_ruling_continuation_posted(&request.job, at)?;
    Ok(Continued {
        job: id,
        reused: false,
    })
}

/// Queue `spec` as `id`, the same-attempt continuation of the request job:
/// the identity rides the payload via [`StepJobRow::again`] and `stamp`, the
/// index row is written and the step is left running for the live run.
fn enqueue_continuation<S: JobStore>(
    st: &mut S,
    store: &PumpStore,
    run: &RunRow,
    row: &StepJobRow,
    step: &StepRow,
    mut spec: JobSpec,
    id: &str,
) -> Result<(), VivariumError> {
    spec.id = id.to_string();
    if spec.task.is_none() {
        spec.task = Some(step.task.clone());
    }
    let record = row.again(id);
    record.stamp(&run.need, &mut spec.payload);
    jobs::enqueue(st, spec)?;
    store.insert_step_job(&run.need, &record)?;
    store.set_step_state(&step.task, StepState::Running, None)
}

/// The registered handler's fresh job for the step. Any action but a job
/// would fabricate an answer the role never gave, so a job is required.
fn resumed_brief<S, M>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    registry: &Registry<S, M>,
    def: &FlowDef,
    run: &RunRow,
    step: &StepRow,
    request: &ScopeRequest,
) -> Result<Box<JobSpec>, VivariumError>
where
    S: JobStore,
    M: SliceMail,
{
    let Some(node) = def.nodes.iter().find(|node| node.id == step.node) else {
        return Err(VivariumError::Parse(format!(
            "scope continuation for job {}: node {} is not in the loaded flow",
            request.job, step.node
        )));
    };
    let Some(handler) = registry.handler_for(def.kind, node) else {
        return Err(VivariumError::Message(format!(
            "scope continuation for job {}: no registered handler for node {}",
            request.job, step.node
        )));
    };
    let cx = StartCx {
        ctx,
        store,
        mail: &*mail,
        run,
        def,
    };
    match handler.start(&cx, step)? {
        StartAction::Job(spec) => Ok(spec),
        _other => Err(VivariumError::Message(format!(
            "scope continuation for job {}: the handler for node {} did not \
             offer a job, so no answer is fabricated",
            request.job, step.node
        ))),
    }
}

/// Repair a replay whose continuation job is already enqueued: write back
/// any missing step-job index row and ruling progress. The stored job's
/// inputs are reused as they are; nothing is rebuilt or duplicated.
fn repair(
    store: &PumpStore,
    run: &RunRow,
    row: &StepJobRow,
    request_job: &str,
    id: &str,
    at: i64,
) -> Result<(), VivariumError> {
    if store.step_job(id)?.is_none() {
        store.insert_step_job(&run.need, &row.again(id))?;
    }
    store.set_ruling_continuation(request_job, id)?;
    store.mark_ruling_continuation_posted(request_job, at)
}

/// The typed front of the immutable need root.
///
/// # Errors
/// Returns [`VivariumError`] when the need cannot be read or does not parse.
fn need_front(mail: &dyn SliceMail, need: &str) -> Result<Envelope<NeedFront>, VivariumError> {
    let body = mail.task_body(need)?;
    need_of(need, &body)
        .map_err(|error| VivariumError::Message(format!("need {need} does not parse: {error}")))
}

/// Settle a fresh request: configured target, then the need's own target,
/// then the target's policy. The policy snapshot is the one the decision was
/// made under; a target with no resolvable policy fails closed to an empty
/// snapshot that grants nothing.
fn evaluate(
    configs: &BTreeMap<String, PumpTarget>,
    need_target: Option<&str>,
    request: &ScopeRequest,
) -> (RulingDecision, Option<ScopeRejection>, ScopePolicy) {
    let Some(config) = configs.get(&request.target) else {
        return (
            RulingDecision::Denied,
            Some(ScopeRejection::UnknownTarget {
                target: request.target.clone(),
                known: configs.keys().cloned().collect(),
            }),
            fails_closed(&request.target),
        );
    };
    let policy = resolved(&request.target, config).unwrap_or_else(|| fails_closed(&request.target));
    if let Some(need_target) = need_target
        && need_target != request.target
    {
        return (
            RulingDecision::Denied,
            Some(ScopeRejection::NeedTargetMismatch {
                unit_target: request.target.clone(),
                need_target: need_target.to_string(),
            }),
            policy,
        );
    }
    match scope::authorize(configs, &request.target, &request.paths) {
        ScopeAuthorization::Grant(_) => (RulingDecision::Approved, None, policy),
        ScopeAuthorization::Reject(rejection) => (RulingDecision::Denied, Some(rejection), policy),
    }
}

/// The resolved policy of a configured target, as `target_configs` provides
/// it: `scope_allow` is already default-filled; the default here only guards
/// direct construction.
fn resolved(target: &str, config: &PumpTarget) -> Option<ScopePolicy> {
    let allow = config
        .scope_allow
        .clone()
        .unwrap_or_else(|| vec![String::from(scope::WHOLE_REPOSITORY)]);
    match ScopePolicy::snapshot(target, &allow, &config.scope_deny) {
        Ok(policy) => Some(policy),
        Err(_) => None,
    }
}

/// The empty snapshot of `target`: denies everything, borrows nothing.
fn fails_closed(target: &str) -> ScopePolicy {
    ScopePolicy {
        target: String::from(target),
        allow: Vec::new(),
        deny: Vec::new(),
    }
}

/// The exact ruling letter body: the stored decision with the effective need
/// scope additions it leaves the need with.
///
/// # Errors
/// Returns [`VivariumError`] when the need root or the rulings cannot be read.
fn drafted(
    store: &PumpStore,
    mail: &dyn SliceMail,
    ruling: &ScopeRulingRow,
) -> Result<String, VivariumError> {
    let front = need_front(mail, &ruling.need)?;
    let scope =
        store.effective_need_scope(&ruling.need, &ruling.target, &front.front.write_scope)?;
    Ok(render(ruling, &scope.additions))
}

/// Render the deterministic body of a ruling letter from the stored row.
fn render(ruling: &ScopeRulingRow, additions: &[String]) -> String {
    let mut body = format!(
        "{RULING_MARKER}{} for job {} on need {}\n\nrole: {}\ntarget: {}\npaths: {}\n",
        ruling.decision_ref.as_deref().unwrap_or("(unset)"),
        ruling.job,
        ruling.need,
        one_line(&ruling.role),
        one_line(&ruling.target),
        listed(&ruling.paths),
    );
    for bad in &ruling.invalid {
        body.push_str(&format!(
            "requested (unusable): {}: {}\n",
            one_line(&bad.path),
            one_line(&bad.problem)
        ));
    }
    body.push_str(&format!("reason: {}\n", one_line(&ruling.reason)));
    body.push_str(&format!("decision: {}\n", ruling.decision.word()));
    if let Some(reason) = &ruling.policy_reason {
        body.push_str(&format!(
            "policy reason: {}\n",
            one_line(&reason.to_string())
        ));
    }
    body.push_str(&format!(
        "policy: allow [{}] deny [{}]\n",
        ruling.policy.allow.join(", "),
        ruling.policy.deny.join(", "),
    ));
    body.push_str(&format!("effective additions: {}\n", listed(additions)));
    body
}

/// True when the exact saved body is already on the thread: the reply
/// readers find ruling letters by [`RULING_MARKER`], and only an exact
/// match with the saved body deduplicates. Thread text alone never grants.
///
/// # Errors
/// Returns [`VivariumError`] when the thread cannot be read.
fn posted(mail: &dyn SliceMail, need: &str, body: &str) -> Result<bool, VivariumError> {
    Ok(mail
        .marked_replies(need, RULING_MARKER)?
        .iter()
        .any(|(_, text)| text == body.trim()))
}

/// `a, b`, or `(none)` for an empty list.
fn listed(paths: &[String]) -> String {
    if paths.is_empty() {
        String::from("(none)")
    } else {
        paths.join(", ")
    }
}

/// Free text folded to one line, so every body field stays on its line.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Epoch seconds as unix seconds.
fn unix(seconds: u64) -> i64 {
    i64::try_from(seconds).unwrap_or(i64::MAX)
}

#[cfg(test)]
#[path = "scope_request_test.rs"]
mod tests;
