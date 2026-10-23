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
use crate::mailspace::marked_line;
use crate::pump::flow::fanout;
use crate::pump::flow::handlers::FlowMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::need_branch;
use crate::pump::flow::slice_dispatch::{self, Retried};
use crate::pump::flow::slice_merge::classify::{SliceMergeResult, classify};
use crate::pump::flow::slice_merge::spec::KIND;
use crate::pump::flow::tick::{OPERATOR_HOLD, SIDE_EFFECT_UNCLEAR, raise_once};
use crate::pump::flow::verify::{self, RunnerPublishReceipt, SideEffect, VerifyReq};
use crate::pump::host::Ctx;
use crate::pump::jobs::rung_result::{ENV_FAILURE, EnvFailure};
use crate::pump::jobs::{self, JobStore, PoolJob};
use crate::pump::store::PumpStore;

/// Marker of the reply that records why a slice merge failed.
pub(crate) const FAILED_MARKER: &str = "pump-slice-merge:";

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
    let runs = store.live_runs()?;
    let finished: Vec<PoolJob> = st
        .jobs()
        .values()
        .filter(|job| job.kind == KIND && job.status.is_terminal() && job.applied_at.is_none())
        .cloned()
        .collect();
    for job in finished {
        if let Err(error) = apply(ctx, st, store, mail, &runs, &job) {
            raise_once(
                ctx,
                APPLY_FAILED,
                &job.id,
                &format!("slice merge job {} cannot be applied: {error}", job.id),
            );
        }
    }
    Ok(())
}

fn apply<S: JobStore, M: FlowMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &mut M,
    runs: &[RunRow],
    job: &PoolJob,
) -> Result<(), VivariumError> {
    let need = text(job, "need");
    let slice = match job.task.as_deref() {
        Some(task) => store.step_by_task(task)?,
        None => None,
    };
    let open = slice.filter(|row| {
        matches!(
            row.state,
            StepState::Pending | StepState::Running | StepState::Waiting
        ) && runs
            .iter()
            .any(|run| run.need == need && u64::from(run.run) == u64::from(row.run))
    });
    let (Some(slice), Some(result)) = (open, classify(job)) else {
        ctx.log(&format!(
            "slice merge: job {} needs no apply (slice closed, run not live, or job not terminal)",
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

    /// Reply with the reason, then count a failed hand attempt.
    fn failed<S: JobStore, M: FlowMail>(
        &self,
        st: &mut S,
        mail: &mut M,
        reason: &str,
    ) -> Result<(), VivariumError> {
        mail.reply(&self.slice.task, &marked_line(FAILED_MARKER, reason))?;
        let attempt = u32::try_from(
            self.job
                .payload
                .get("attempt")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(1),
        )
        .unwrap_or(1);
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
        mail.reply(
            &self.slice.task,
            &marked_line(FAILED_MARKER, &format!("environment: {}", env.evidence)),
        )?;
        self.store
            .set_step_state(&self.slice.task, StepState::Waiting, Some(OPERATOR_HOLD))
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
            SideEffect::Absent => self.failed(
                st,
                mail,
                "the slice merge left no change on the need branch",
            ),
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
