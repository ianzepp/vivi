//! Default result collectors for flow steps (roles and merge-main; rungs are in `rung_step`).
//!
//! A finished step job reaches its [`StepCollector`] through the registry,
//! keyed by `(FlowKind, NodeClass)`. [`RunCx`] is the per-run context the
//! tick builds and every collector works through.

use crate::error::VivariumError;
use crate::pump::envelope::correct;
use crate::pump::envelope::roles::RoleKind;
use crate::pump::flow::advance::{self, route};
use crate::pump::flow::collect::{self, Collected};
use crate::pump::flow::def::{FAILED, FlowDef, FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState};
use crate::pump::flow::outcome::Outcome;
use crate::pump::flow::rung_step::RungCollector;
use crate::pump::flow::tick::{OPERATOR_HOLD, SIDE_EFFECT_UNCLEAR, WORD_REJECTED};
use crate::pump::flow::verify::{self, RunnerPublishReceipt, SideEffect, VerifyReq};
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobSpec, JobStore, PoolJob};
use crate::pump::store::PumpStore;

/// One live run's tick: the context every step and slice collector works in.
pub(crate) struct RunCx<'a, S, M> {
    pub(crate) ctx: &'a Ctx,
    pub(crate) st: &'a mut S,
    pub(crate) store: &'a PumpStore,
    pub(crate) mail: &'a mut M,
    pub(crate) registry: &'a Registry<S, M>,
    pub(crate) decide: route::Decide<'a>,
    pub(crate) def: FlowDef,
    pub(crate) run: RunRow,
}

/// Turns one finished job of a step into an answer word, a retry or a hold.
pub(crate) trait StepCollector<S, M> {
    /// Collect the finished `job` of `step`, whose node has class `class`.
    ///
    /// # Errors
    /// Returns a [`VivariumError`]; the tick holds it on the run.
    fn collect(
        &self,
        cx: &mut RunCx<'_, S, M>,
        job: &PoolJob,
        row: &StepJobRow,
        step: &StepRow,
        class: &NodeClass,
    ) -> Result<(), VivariumError>;
}

/// Roles, merge and fanout steps: the reply is parsed for its envelope.
pub(crate) struct RoleCollector;

/// Merge-main steps: the runner's publish receipt decides.
pub(crate) struct MergeMainCollector;

/// The default collector of a step class; `None` for a class that is not a
/// step (the match names every class, no catch-all).
pub(crate) fn default_for<'a, S: JobStore + 'a, M: FlowMail + 'a>(
    class: &NodeClass,
) -> Option<&'a (dyn StepCollector<S, M> + 'a)> {
    match class {
        NodeClass::Researcher
        | NodeClass::Planner
        | NodeClass::Admitter
        | NodeClass::Slicer
        | NodeClass::Auditor
        | NodeClass::Judge
        | NodeClass::Cto
        | NodeClass::Merge
        | NodeClass::Repairer
        | NodeClass::Fanout => Some(&RoleCollector),
        NodeClass::RungNeed
        | NodeClass::RungMain
        | NodeClass::RungRelease
        | NodeClass::RungRaised => Some(&RungCollector),
        NodeClass::MergeMain => Some(&MergeMainCollector),
        NodeClass::Join
        | NodeClass::Raisecheck
        | NodeClass::Milestone
        | NodeClass::Park
        | NodeClass::Lock
        | NodeClass::Restart(_)
        | NodeClass::Decision
        | NodeClass::Done => None,
    }
}

/// Register the default collector of every role and merge-main class of
/// every flow kind. Rung classes are registered by `rung_step::register`; the
/// default lookup still reaches its collector for a hand-built registry.
pub(crate) fn register_defaults<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    for kind in [
        FlowKind::Build,
        FlowKind::Research,
        FlowKind::Release,
        FlowKind::Repair,
    ] {
        for class in [
            NodeClass::Researcher,
            NodeClass::Planner,
            NodeClass::Admitter,
            NodeClass::Slicer,
            NodeClass::Auditor,
            NodeClass::Judge,
            NodeClass::Cto,
            NodeClass::Merge,
            NodeClass::Fanout,
        ] {
            registry.collector(kind, class, Box::new(RoleCollector));
        }
        registry.collector(kind, NodeClass::MergeMain, Box::new(MergeMainCollector));
    }
}

impl<S: JobStore, M: FlowMail> StepCollector<S, M> for MergeMainCollector {
    fn collect(
        &self,
        cx: &mut RunCx<'_, S, M>,
        job: &PoolJob,
        _row: &StepJobRow,
        step: &StepRow,
        _class: &NodeClass,
    ) -> Result<(), VivariumError> {
        match jobs::merge_main::classify(job) {
            Some(jobs::merge_main::MergeMainResult::Merged(sha)) => {
                cx.advance_word(step, "merged", merged_payload(&sha))
            }
            Some(jobs::merge_main::MergeMainResult::Conflict) => {
                cx.advance_word(step, "conflict", serde_json::json!({}))
            }
            _ => cx.verify_step(job, step),
        }
    }
}

impl<S: JobStore, M: FlowMail> StepCollector<S, M> for RoleCollector {
    fn collect(
        &self,
        cx: &mut RunCx<'_, S, M>,
        job: &PoolJob,
        row: &StepJobRow,
        step: &StepRow,
        class: &NodeClass,
    ) -> Result<(), VivariumError> {
        let expected = job.publish_ref.clone().unwrap_or_default();
        let collected = collect::collect(cx.ctx, cx.mail, job, row, class, &expected);
        match collected {
            Collected::Outcome(word, payload) => cx.advance_word(step, &word, payload),
            Collected::Failed => cx.advance_word(step, FAILED, serde_json::json!({"job": job.id})),
            Collected::Verify => cx.verify_step(job, step),
            Collected::Retry if job.kind == correct::KIND => {
                cx.advance_word(step, FAILED, serde_json::json!({"job": job.id}))
            }
            Collected::Retry => cx.rerun(job, row, step),
            Collected::Correct { error } => {
                let kind = RoleKind::from_class(class).ok_or_else(|| {
                    VivariumError::Parse(format!("node {} has no role kind", step.node))
                })?;
                cx.correct(job, row, kind, &error)
            }
        }
    }
}

impl<S, M> RunCx<'_, S, M>
where
    S: JobStore,
    M: FlowMail,
{
    pub(crate) fn class_of(&self, node: &str) -> Option<NodeClass> {
        self.def
            .nodes
            .iter()
            .find(|n| n.id == node)
            .map(|n| n.class.clone())
    }

    pub(crate) fn advance_word(
        &mut self,
        step: &StepRow,
        word: &str,
        payload: serde_json::Value,
    ) -> Result<(), VivariumError> {
        let outcome = Outcome {
            need: self.run.need.clone(),
            run: self.run.run,
            node: step.node.clone(),
            task: step.task.clone(),
            word: word.to_owned(),
            payload,
        };
        let advanced = advance::advance(
            self.ctx,
            self.store,
            self.mail,
            &self.def,
            &outcome,
            self.decide,
        )?;
        if let Some(reason) = advanced.routed.rejected {
            let held = format!("{reason}; held for vivi pump answer {} <word>", step.task);
            self.ctx.exception(WORD_REJECTED, &step.task, &held);
            self.store
                .set_step_state(&step.task, StepState::Waiting, Some(OPERATOR_HOLD))?;
        }
        Ok(())
    }

    /// Queue a correction job for the broken reply of `job`.
    pub(crate) fn correct(
        &mut self,
        job: &PoolJob,
        row: &StepJobRow,
        kind: RoleKind,
        error: &crate::pump::envelope::EnvelopeError,
    ) -> Result<(), VivariumError> {
        let n = correct::correction_n(&job.id) + 1;
        let spec = correct::spec(&self.run.need, row, &job.role, kind, job, error, n);
        correct::enqueue(self.st, self.store, &self.run.need, row, spec)
    }

    /// Run a role step's job again as the next attempt.
    pub(crate) fn rerun(
        &mut self,
        job: &PoolJob,
        row: &StepJobRow,
        step: &StepRow,
    ) -> Result<(), VivariumError> {
        self.rerun_with(job, row, step, |_| {})
    }

    /// [`Self::rerun`], with `edit` applied to the new job's spec before it is queued.
    pub(crate) fn rerun_with(
        &mut self,
        job: &PoolJob,
        row: &StepJobRow,
        step: &StepRow,
        edit: impl FnOnce(&mut JobSpec),
    ) -> Result<(), VivariumError> {
        let attempt = row.attempt + 1;
        let id = format!("{}-a{attempt}", correct::base_id(&job.id));
        let mut spec = JobSpec::new(
            id.clone(),
            &job.kind,
            &job.role,
            job.subject.clone(),
            job.body.clone(),
        );
        spec.task.clone_from(&job.task);
        spec.target.clone_from(&job.target);
        spec.from_ref.clone_from(&job.from_ref);
        spec.branch.clone_from(&job.branch);
        spec.publish_ref.clone_from(&job.publish_ref);
        spec.pool_candidates.clone_from(&job.pool_candidates);
        spec.local_only = job.local_only;
        spec.read_only = job.read_only;
        spec.budget = job.budget;
        spec.payload = job.payload.clone();
        edit(&mut spec);
        jobs::enqueue(self.st, spec)?;
        self.store.insert_step_job(
            &self.run.need,
            &StepJobRow {
                job: id,
                run: row.run,
                node: row.node.clone(),
                task: row.task.clone(),
                attempt,
            },
        )?;
        self.store
            .set_step_state(&step.task, StepState::Running, None)
    }

    /// A side-effect step with no decisive receipt: git decides.
    pub(crate) fn verify_step(
        &mut self,
        job: &PoolJob,
        step: &StepRow,
    ) -> Result<(), VivariumError> {
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
            before: job.from_ref.clone(),
            must_contain: job
                .publish_contains
                .clone()
                .or_else(|| job.publish_expect.clone())
                .unwrap_or_default(),
            receipt,
        };
        match verify::check(self.ctx, &req) {
            SideEffect::Verified { sha } => self.advance_word(step, "merged", merged_payload(&sha)),
            SideEffect::Absent => {
                let reason = format!(
                    "expected {remote_ref} to contain {}; actual: git says it does not",
                    req.must_contain
                );
                self.advance_word(
                    step,
                    FAILED,
                    serde_json::json!({"job": job.id, "reason": reason}),
                )
            }
            SideEffect::Unclear { reason } => {
                self.ctx.exception(
                    SIDE_EFFECT_UNCLEAR,
                    &step.task,
                    &format!(
                        "step {} task {} job {} ref {remote_ref}: {reason}; answer with `vivi pump answer`",
                        step.node, step.task, job.id
                    ),
                );
                self.store
                    .set_step_state(&step.task, StepState::Waiting, Some(OPERATOR_HOLD))
            }
        }
    }
}

pub(crate) fn merged_payload(sha: &str) -> serde_json::Value {
    serde_json::json!({"sha": sha, "commit": sha})
}

#[cfg(test)]
#[path = "collect_step_test.rs"]
mod tests;
