//! Applies a finished slice merge job: closes the slice merged, or counts a failed attempt.
//!
//! A merged result closes the slice task `merged` with the need branch and the
//! published sha. A conflict, a red formatter or task rung, or a merge that
//! left no change is a failed attempt: the pump replies on the slice task and
//! retries the hand through [`slice_dispatch::retry`], which exhausts the
//! slice after `MAX_ATTEMPTS`. An environment failure (a missing tool, a host
//! setup failure) is never retried: the pump alarms once and holds the slice.
//! A result the receipt cannot settle goes to git.

use crate::error::VivariumError;
use crate::letter::records::NoteWord;
use crate::mailspace::pump_note::note_body;
use crate::pump::flow::fanout;
use crate::pump::flow::handlers::FlowMail;
use crate::pump::flow::index::{JOB_STALE, PayloadIdentity, StepRow, StepState};
use crate::pump::flow::need_branch;
use crate::pump::flow::slice_dispatch::{self, Retried};
use crate::pump::flow::slice_merge::classify::{SliceMergeResult, classify};
use crate::pump::flow::slice_merge::queue::{JobRun, classify_job, stale_text};
use crate::pump::flow::slice_merge::spec::KIND;
use crate::pump::flow::step_error::{self, STEP_ERROR_LIMIT};
use crate::pump::flow::tick::{OPERATOR_HOLD, SIDE_EFFECT_UNCLEAR, raise_once};
use crate::pump::flow::verify::{self, RunnerPublishReceipt, SideEffect, VerifyReq};
use crate::pump::host::Ctx;
use crate::pump::jobs::rung_result::{ENV_FAILURE, EnvFailure};
use crate::pump::jobs::{self, JobStore, PoolJob};
use crate::pump::store::PumpStore;

/// Exception kind for a finished slice merge that could not be applied.
const APPLY_FAILED: &str = "flow-slice-merge-apply-failed";

/// Apply every finished, unapplied slice merge job.
///
/// # Errors
/// Returns [`VivariumError`] when the flow index cannot be read.
pub(crate) fn run<S: JobStore, M: FlowMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &mut M,
) -> Result<(), VivariumError> {
    let finished: Vec<PoolJob> = st
        .jobs()
        .values()
        .filter(|job| job.kind == KIND && job.status.is_terminal() && job.applied_at.is_none())
        .cloned()
        .collect();
    for job in finished {
        match apply(ctx, st, store, mail, &job) {
            Ok(()) => step_error::note_success(ctx, &job.id),
            Err(error) => give_up_or_alarm(ctx, st, store, &job, &error)?,
        }
    }
    Ok(())
}

/// A failed apply: alarm once below [`STEP_ERROR_LIMIT`] ticks in a row; at the
/// limit mark the job applied and hold its slice for the operator.
fn give_up_or_alarm<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    job: &PoolJob,
    error: &VivariumError,
) -> Result<(), VivariumError> {
    let n = step_error::note_failure(ctx, &job.id);
    if n < STEP_ERROR_LIMIT {
        raise_once(
            ctx,
            APPLY_FAILED,
            &job.id,
            &format!("slice merge job {} cannot be applied: {error}", job.id),
        );
        return Ok(());
    }
    jobs::mark_applied(st, &job.id, ctx.now());
    if let Some(task) = job.task.as_deref() {
        let text = format!(
            "slice merge job {} for slice {task}: expected the result to apply; actual: {error} on {n} ticks in a row; held for vivi pump answer",
            job.id
        );
        step_error::hold(ctx, store, task, &text)?;
    }
    Ok(())
}

fn apply<S: JobStore, M: FlowMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &mut M,
    job: &PoolJob,
) -> Result<(), VivariumError> {
    let Some(identity) = live_identity(ctx, st, store, job)? else {
        return Ok(());
    };
    let need = identity.need.clone();
    let slice = match job.task.as_deref() {
        Some(task) => store.step_by_task(task)?,
        None => None,
    };
    if let Some(row) = slice.as_ref().filter(|row| row.run != identity.run) {
        let found = format!("slice {} is of run {}", row.task, row.run);
        let reason = stale_text(job, "slice run", &identity.run.to_string(), &found);
        return settle_stale(ctx, st, job, &reason);
    }
    let open = slice.filter(|row| {
        matches!(
            row.state,
            StepState::Pending | StepState::Running | StepState::Waiting
        )
    });
    let (Some(slice), Some(result)) = (open, classify(job)) else {
        ctx.log(&format!(
            "slice merge: job {} needs no apply (slice closed or job not terminal)",
            job.id
        ));
        jobs::mark_applied(st, &job.id, ctx.now());
        return Ok(());
    };
    let ended = Ended {
        ctx,
        store,
        need: &need,
        slice: &slice,
        job,
    };
    match result {
        SliceMergeResult::Merged(sha) => ended.merged(mail, &sha)?,
        SliceMergeResult::Conflict(paths) => {
            ended.failed(st, mail, &format!("conflict: {}", paths.join(", ")))?;
        }
        SliceMergeResult::Red(tail) => {
            ended.failed(st, mail, &format!("format or task rung failed\n{tail}"))?;
        }
        SliceMergeResult::Env(env) => ended.environment(mail, &env)?,
        SliceMergeResult::Unclear => ended.unclear(st, mail)?,
    }
    jobs::mark_applied(st, &job.id, ctx.now());
    Ok(())
}

/// The job's payload identity when it names a live run. A superseded or
/// withdrawn run is the expected end: log, mark applied, give `None`. A stale
/// identity raises once, is marked applied, and gives `None`.
fn live_identity<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    job: &PoolJob,
) -> Result<Option<PayloadIdentity>, VivariumError> {
    match classify_job(job, store)? {
        JobRun::Live(identity) => Ok(Some(identity)),
        JobRun::Dead(identity, state) => {
            ctx.log(&format!(
                "slice merge: job {} of {} r{} ({state}) needs no apply",
                job.id, identity.need, identity.run
            ));
            jobs::mark_applied(st, &job.id, ctx.now());
            Ok(None)
        }
        JobRun::Stale(reason) => {
            settle_stale(ctx, st, job, &reason)?;
            Ok(None)
        }
    }
}

/// A stale job names a run or slice that is not the live one: raise once, then
/// mark it applied so it is never looked at again.
fn settle_stale<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    job: &PoolJob,
    reason: &str,
) -> Result<(), VivariumError> {
    raise_once(ctx, JOB_STALE, &job.id, reason);
    jobs::mark_applied(st, &job.id, ctx.now());
    Ok(())
}

/// One finished slice merge job with the slice it belongs to.
struct Ended<'a> {
    ctx: &'a Ctx,
    store: &'a PumpStore,
    need: &'a str,
    slice: &'a StepRow,
    job: &'a PoolJob,
}

impl Ended<'_> {
    fn merged<M: FlowMail>(&self, mail: &mut M, sha: &str) -> Result<(), VivariumError> {
        fanout::close_slice_merged(
            self.store,
            mail,
            &self.slice.task,
            &need_branch::branch(self.need),
            sha,
        )
    }

    /// The hand attempt this merge belongs to. A missing, non-numeric or
    /// out-of-range payload value is a parse error naming the job.
    fn attempt(&self) -> Result<u32, VivariumError> {
        let bad = || {
            VivariumError::Parse(format!(
                "slice merge job {} has no valid numeric attempt in its payload",
                self.job.id
            ))
        };
        let value = self
            .job
            .payload
            .get("attempt")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(bad)?;
        u32::try_from(value).map_err(|_| bad())
    }

    /// Reply on the slice task once per job with a slice-merge note whose
    /// reference is the job id; a task that already has one is not posted to again.
    fn reply_once<M: FlowMail>(&self, mail: &mut M, reason: &str) -> Result<(), VivariumError> {
        let seen = mail
            .notes(&self.slice.task, NoteWord::SliceMerge)?
            .iter()
            .any(|(_, note)| note.pump.reference.as_deref() == Some(self.job.id.as_str()));
        if seen {
            return Ok(());
        }
        let key = format!("job {}:", self.job.id);
        let body = note_body(
            NoteWord::SliceMerge,
            Some(&self.job.id),
            &format!("{key} {reason}"),
        )?;
        mail.reply(&self.slice.task, &body)
    }

    /// Reply with the reason (once per job), then count a failed hand attempt.
    fn failed<S: JobStore, M: FlowMail>(
        &self,
        st: &mut S,
        mail: &mut M,
        reason: &str,
    ) -> Result<(), VivariumError> {
        let attempt = self.attempt()?;
        self.reply_once(mail, reason)?;
        let retried = slice_dispatch::retry(
            self.ctx, st, self.store, &*mail, self.need, self.slice, self.job, attempt,
        )?;
        if retried == Retried::Exhausted {
            self.ctx.log(&format!(
                "slice merge: slice {} of {} is exhausted after attempt {attempt}",
                self.slice.node, self.need
            ));
        }
        Ok(())
    }

    /// The worker could not run the job: alarm once, reply once and hold the
    /// slice for the operator. No hand attempt is used or queued.
    fn environment<M: FlowMail>(
        &self,
        mail: &mut M,
        env: &EnvFailure,
    ) -> Result<(), VivariumError> {
        let job = self.job;
        let tool = env.tool.as_deref().unwrap_or("none named");
        let text = format!(
            "slice merge job {} of slice {} ({}) hit an environment failure ({:?}, tool {tool}): {}; target {}, pool {}; fix the worker, then answer with `vivi pump answer`",
            job.id,
            self.slice.node,
            self.slice.task,
            env.kind,
            env.evidence,
            job.target.as_deref().unwrap_or("none"),
            job.pool.as_deref().unwrap_or("none"),
        );
        raise_once(self.ctx, ENV_FAILURE, &job.id, &text);
        self.reply_once(mail, &format!("environment: {}", env.evidence))?;
        self.store
            .set_step_state(&self.slice.task, StepState::Waiting, Some(OPERATOR_HOLD))
    }

    /// The remote ref's tip for the failure text: a sha, `absent`, or the git error.
    fn ref_tip(&self, remote_ref: &str) -> String {
        let name = self.job.target.clone().unwrap_or_default();
        let Some(target) = self.ctx.for_target(&name) else {
            return String::from("unknown (target not configured)");
        };
        match verify::remote_tip(&target, &target.cfg.remote, remote_ref) {
            Ok(Some(sha)) => sha,
            Ok(None) => String::from("absent"),
            Err(reason) => format!("unknown ({reason})"),
        }
    }

    /// The receipt did not settle it: git decides.
    fn unclear<S: JobStore, M: FlowMail>(
        &self,
        st: &mut S,
        mail: &mut M,
    ) -> Result<(), VivariumError> {
        let job = self.job;
        let receipt = job
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.runner_publish())
            .filter(|publish| publish.succeeded())
            .map(|publish| RunnerPublishReceipt {
                sha: publish.remote_sha,
            });
        let remote_ref = job.publish_ref.clone().unwrap_or_default();
        let req = VerifyReq {
            target: job.target.clone().unwrap_or_default(),
            remote_ref: remote_ref.clone(),
            before: job
                .payload
                .get("before")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            must_contain: text(job, "hand_sha"),
            receipt,
        };
        match verify::check(self.ctx, &req) {
            SideEffect::Verified { sha } => self.merged(mail, &sha),
            SideEffect::Absent => {
                let tip = self.ref_tip(&remote_ref);
                self.failed(
                    st,
                    mail,
                    &format!(
                        "the slice merge left no change on the need branch: expected {} on {remote_ref}; actual: ref tip {tip}, before {}",
                        text(job, "hand_sha"),
                        text(job, "before")
                    ),
                )
            }
            SideEffect::Unclear { reason } => {
                self.ctx.exception(
                    SIDE_EFFECT_UNCLEAR,
                    &self.slice.task,
                    &format!(
                        "step {} task {} job {} ref {remote_ref}: {reason}; answer with `vivi pump answer`",
                        self.slice.node, self.slice.task, job.id
                    ),
                );
                self.store
                    .set_step_state(&self.slice.task, StepState::Waiting, Some(OPERATOR_HOLD))
            }
        }
    }
}

fn text(job: &PoolJob, key: &str) -> String {
    job.payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
#[path = "apply_test.rs"]
mod tests;
