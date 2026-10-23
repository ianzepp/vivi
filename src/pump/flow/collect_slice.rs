//! Collecting slice hand jobs and the built-slice hooks.
//!
//! A finished slice job is classified (built, no defect, failed, correction,
//! blocked scope) and acted on through the run's [`RunCx`]. A built slice
//! runs every registered [`BuiltHook`] in registration order; the default
//! hook posts the hand's deviations and follow-up work and leaves the slice
//! open. Hooks are independent: none reads another's result.

use serde::Serialize;

use crate::error::VivariumError;
use crate::mailspace::marked_line;
use crate::pump::envelope::correct;
use crate::pump::envelope::roles::{FollowUp, Hand, RoleKind};
use crate::pump::flow::collect_step::RunCx;
use crate::pump::flow::fanout::{self, VERDICT_MERGED, VERDICT_NO_DEFECT, is_slice_row};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::mail::TaskStatus;
use crate::pump::flow::reslice::{self, Mode};
use crate::pump::flow::scope::{
    BlockedScope, Resolution, ScopeState, finish_grant, grant_held, resolve, wait_holders,
};
use crate::pump::flow::slice_dispatch::{self, AttemptResult, DEVIATIONS_MARKER, Retried};
use crate::pump::flow::tick::{
    FOLLOW_UP, FOLLOW_UP_MARKER, OPERATOR_HOLD, SCOPE_REWORK, raise_once,
};
use crate::pump::jobs::rung_result::{self, ENV_FAILURE, EnvFailure};
use crate::pump::jobs::{JobStatus, JobStore, PoolJob};

/// Characters of a timed-out hand's receipt passed to the split job.
const RECEIPT_TAIL: usize = 2000;

/// The last `max` characters of `text`.
fn tail_of(text: &str, max: usize) -> &str {
    let skip = text.chars().count().saturating_sub(max);
    text.char_indices()
        .nth(skip)
        .map_or("", |(at, _)| &text[at..])
}

/// Marker of the reply that records a hand job's environment failure.
const ENV_MARKER: &str = "pump-slice-env:";

/// The runner-level environment failure of a hand job, if it has one: a host
/// failure the runner classified from its own facts, or the agent binary
/// itself exiting 126 or 127. A missing tool the hand's own test run printed
/// to stderr is the hand's judgment and does not count.
pub(crate) fn runner_env_failure(job: &PoolJob) -> Option<EnvFailure> {
    let env = rung_result::env_failure(job)?;
    let agent_missing = matches!(rung_result::parse(job).exit, Some(126 | 127));
    let host = job
        .outcome
        .as_ref()
        .and_then(|outcome| outcome.host_failure())
        .is_some_and(|host| host.class != "shell_marker");
    (agent_missing || host).then_some(env)
}

/// Something done for a slice whose hand job built.
pub(crate) trait BuiltHook<S, M> {
    /// Act on the built `step` with the hand's `envelope`.
    ///
    /// # Errors
    /// Returns a [`VivariumError`]; the tick holds it on the run.
    fn built(
        &self,
        cx: &mut RunCx<'_, S, M>,
        step: &StepRow,
        envelope: &Hand,
    ) -> Result<(), VivariumError>;
}

/// The default hook: post the deviations and follow-up work for the next
/// hands and the slicer. The slice stays open (the merge is a later stage).
pub(crate) struct PostBuiltNotes;

/// Register the default built-slice hook.
pub(crate) fn register_defaults<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.built_hook(Box::new(PostBuiltNotes));
}

impl<S: JobStore, M: FlowMail> BuiltHook<S, M> for PostBuiltNotes {
    fn built(
        &self,
        cx: &mut RunCx<'_, S, M>,
        step: &StepRow,
        envelope: &Hand,
    ) -> Result<(), VivariumError> {
        if !envelope.deviations.is_empty() {
            let text = envelope.deviations.join("; ");
            cx.mail
                .reply(&step.task, &marked_line(DEVIATIONS_MARKER, &text))?;
        }
        if !envelope.follow_up.is_empty() {
            #[derive(Serialize)]
            struct FollowUps<'a> {
                follow_up: &'a [FollowUp],
            }
            let toml = toml::to_string(&FollowUps {
                follow_up: &envelope.follow_up,
            })
            .map_err(|error| VivariumError::Other(format!("follow-up encoding: {error}")))?;
            cx.mail.reply(
                &step.task,
                &marked_line(FOLLOW_UP_MARKER, &format!("\n{toml}")),
            )?;
            cx.ctx.exception(
                FOLLOW_UP,
                &step.task,
                &format!(
                    "{} reported {} follow-up item(s)",
                    step.node,
                    envelope.follow_up.len()
                ),
            );
        }
        Ok(())
    }
}

impl<S, M> RunCx<'_, S, M>
where
    S: JobStore,
    M: FlowMail,
{
    pub(crate) fn slice_job(
        &mut self,
        job: &PoolJob,
        row: &StepJobRow,
        step: &StepRow,
    ) -> Result<(), VivariumError> {
        let hand = if correct::correction_n(&job.id) > 0 {
            self.st.jobs().get(correct::base_id(&job.id)).cloned()
        } else {
            None
        };
        let hand = hand.as_ref().unwrap_or(job);
        if let Some(env) = runner_env_failure(hand) {
            return self.environment(step, row, hand, &env);
        }
        let Some(result) = slice_dispatch::classify_corrected(self.ctx, hand, job) else {
            return Ok(());
        };
        let need = self.run.need.clone();
        match result {
            AttemptResult::Built { envelope, .. } => {
                let registry = self.registry;
                for hook in registry.built_hooks() {
                    hook.built(self, step, &envelope)?;
                }
                Ok(())
            }
            AttemptResult::NoDefect => {
                fanout::close_slice(self.store, &mut *self.mail, &step.task, VERDICT_NO_DEFECT)
            }
            AttemptResult::Failed if hand.status == JobStatus::Timeout => self.split(step, hand),
            AttemptResult::Failed => {
                let retried = slice_dispatch::retry(
                    self.ctx,
                    self.st,
                    self.store,
                    &*self.mail,
                    &need,
                    step,
                    hand,
                    row.attempt,
                )?;
                if retried == Retried::Exhausted {
                    self.ctx
                        .log(&format!("slice {} exhausted its attempts", step.task));
                }
                Ok(())
            }
            AttemptResult::Correct { error } => self.correct(job, row, RoleKind::Hand, &error),
            AttemptResult::BlockedScope(request) => self.blocked(step, row, &request),
        }
    }

    /// The worker could not run the hand job: alarm once, reply once and hold
    /// the slice for the operator. No attempt is counted, queued or split.
    fn environment(
        &mut self,
        step: &StepRow,
        row: &StepJobRow,
        hand: &PoolJob,
        env: &EnvFailure,
    ) -> Result<(), VivariumError> {
        let tool = env.tool.as_deref().unwrap_or("none named");
        let text = format!(
            "hand job {} of slice {} ({}): expected the agent to run; actual: {} ({:?}, tool {tool}); pool {}, attempt {} not counted; fix the worker, then `vivi pump requeue` or `vivi pump answer`",
            hand.id,
            step.node,
            step.task,
            env.evidence,
            env.kind,
            hand.pool.as_deref().unwrap_or("none"),
            row.attempt,
        );
        raise_once(self.ctx, ENV_FAILURE, &hand.id, &text);
        let key = format!("job {}:", hand.id);
        let seen = self
            .mail
            .marked_replies(&step.task, ENV_MARKER)?
            .iter()
            .any(|(_, body)| body.contains(&key));
        if !seen {
            let reason = format!("{key} environment: {}", env.evidence);
            self.mail
                .reply(&step.task, &marked_line(ENV_MARKER, &reason))?;
        }
        self.store
            .set_step_state(&step.task, StepState::Waiting, Some(OPERATOR_HOLD))
    }

    pub(crate) fn blocked(
        &mut self,
        step: &StepRow,
        row: &StepJobRow,
        request: &BlockedScope,
    ) -> Result<(), VivariumError> {
        let resolution = resolve(self.store, self.mail, self.run.run, &step.task, request)?;
        match resolution {
            Resolution::Widen(_) => self.redispatch(step, row.attempt),
            Resolution::WaitOn(_) => {
                self.store
                    .set_step_state(&step.task, StepState::Waiting, None)
            }
            Resolution::Rework(reason) => {
                self.store.set_scope_state(&step.task, ScopeState::Rework)?;
                self.ctx.exception(SCOPE_REWORK, &step.task, &reason);
                self.rework(step, request)
            }
        }
    }

    /// A timed-out hand goes to a split job instead of a retry.
    fn split(&mut self, step: &StepRow, hand: &PoolJob) -> Result<(), VivariumError> {
        let tail = hand
            .outcome
            .as_ref()
            .map_or("", |outcome| tail_of(&outcome.receipt, RECEIPT_TAIL));
        let input = format!(
            "job: {}\nbudget: {}\nreceipt tail:\n{tail}\n",
            hand.id, hand.budget
        );
        reslice::request(
            self.ctx,
            self.st,
            self.store,
            self.mail,
            &self.run,
            step,
            Mode::Split,
            &input,
        )
        .map(|_| ())
    }

    /// A blocked-scope request that needs a re-slice goes to a rework job, with
    /// the run's open scopes so the slicer can place the work.
    fn rework(&mut self, step: &StepRow, request: &BlockedScope) -> Result<(), VivariumError> {
        let scopes: String = self
            .store
            .open_scopes_of_run(&step.task, self.run.run)?
            .iter()
            .map(|(task, scope)| format!("{task}: {}\n", scope.join(", ")))
            .collect();
        let input = format!(
            "blocked-scope request:\npaths: {}\nreason: {}\ndesign: {}\n\nopen scopes of this run:\n{scopes}",
            request.paths.join(", "),
            request.reason,
            request.design
        );
        reslice::request(
            self.ctx,
            self.st,
            self.store,
            self.mail,
            &self.run,
            step,
            Mode::Rework,
            &input,
        )
        .map(|_| ())
    }

    pub(crate) fn redispatch(&mut self, step: &StepRow, attempt: u32) -> Result<(), VivariumError> {
        slice_dispatch::redispatch_scope(
            self.ctx,
            self.st,
            self.store,
            &*self.mail,
            &self.run.need,
            step,
            attempt,
        )
        .map(|_| ())
    }

    /// A `scope-wait` slice whose holders all closed goes out again.
    pub(crate) fn release_scope_waits(&mut self) -> Result<(), VivariumError> {
        for step in self.store.steps_of(&self.run.need, self.run.run)? {
            if !is_slice_row(&step) || self.store.scope_state(&step.task)? != Some(ScopeState::Wait)
            {
                continue;
            }
            let mut all_closed = true;
            for holder in wait_holders(self.store, &step.task)? {
                let state = self.mail.task_state(&holder)?;
                let landed = matches!(
                    state.verdict.as_deref(),
                    Some(VERDICT_MERGED | VERDICT_NO_DEFECT)
                );
                all_closed &= state.status == TaskStatus::Done && landed;
            }
            if all_closed {
                grant_held(self.store, &step.task)?;
                let attempt = self.current_attempt(&step.task)?;
                self.redispatch(&step, attempt)?;
                finish_grant(self.store, &step.task)?;
            }
        }
        Ok(())
    }

    /// The attempt of the newest job row of the slice `task`, from step_jobs.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the attempt cannot be read, or when the
    /// task has no step_jobs row.
    pub(crate) fn current_attempt(&self, task: &str) -> Result<u32, VivariumError> {
        match self.store.last_attempt(task)? {
            0 => Err(VivariumError::Message(format!(
                "slice {task}: expected a step_jobs row; actual: none"
            ))),
            attempt => Ok(attempt),
        }
    }
}

#[cfg(test)]
#[path = "collect_slice_test.rs"]
mod tests;

#[cfg(test)]
#[path = "collect_slice_reslice_test.rs"]
mod reslice_tests;

#[cfg(test)]
#[path = "collect_slice_env_test.rs"]
mod env_tests;
