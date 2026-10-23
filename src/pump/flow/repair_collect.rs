//! Repairer result collector.
//!
//! Facts come from machines: a repairer job past its time cap answers
//! `overtime` with no retry, and a `built` envelope counts only when the
//! runner's publish receipt shows the attempt branch was pushed.

use serde_json::json;

use crate::error::VivariumError;
use crate::mailspace::marked_line;
use crate::pump::envelope::correct;
use crate::pump::envelope::roles::RoleKind;
use crate::pump::flow::collect::{self, Collected};
use crate::pump::flow::collect_step::{RunCx, StepCollector};
use crate::pump::flow::def::{FAILED, FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::{StepJobRow, StepRow};
use crate::pump::flow::repair::{self, AttemptFacts};
use crate::pump::jobs::{JobStatus, JobStore, PoolJob};

/// The answer word of a repairer job that ran past its time cap.
const OVERTIME: &str = "overtime";

/// Collects the finished jobs of repair-flow `fix` steps.
pub(crate) struct RepairerCollector;

/// Registers this module's handlers.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.collector(
        FlowKind::Repair,
        NodeClass::Repairer,
        Box::new(RepairerCollector),
    );
}

impl<S: JobStore, M: FlowMail> StepCollector<S, M> for RepairerCollector {
    fn collect(
        &self,
        cx: &mut RunCx<'_, S, M>,
        job: &PoolJob,
        row: &StepJobRow,
        step: &StepRow,
        class: &NodeClass,
    ) -> Result<(), VivariumError> {
        let is_correction = job.kind == correct::KIND;
        if job.status == JobStatus::Timeout && !is_correction {
            return cx.advance_word(step, OVERTIME, json!({"job": job.id}));
        }
        match collect::collect(cx.ctx, cx.mail, job, row, class, "") {
            Collected::Outcome(word, payload) if word == "built" => built(cx, job, step, payload),
            Collected::Outcome(word, payload) => cx.advance_word(step, &word, payload),
            Collected::Retry if !is_correction => cx.rerun(job, row, step),
            Collected::Retry | Collected::Failed | Collected::Verify => {
                cx.advance_word(step, FAILED, json!({"job": job.id}))
            }
            Collected::Correct { error } => cx.correct(job, row, RoleKind::Repairer, &error),
        }
    }
}

/// A `built` envelope: the publish receipt decides whether it holds.
fn built<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    job: &PoolJob,
    step: &StepRow,
    mut payload: serde_json::Value,
) -> Result<(), VivariumError> {
    // A correction job carries no receipt: the machine facts are the original's.
    let original = cx
        .st
        .jobs()
        .get(correct::base_id(&job.id))
        .unwrap_or(job)
        .clone();
    let Some(facts) = published(&original) else {
        let reason = "built but nothing published";
        return cx.advance_word(step, FAILED, json!({"job": job.id, "reason": reason}));
    };
    let text = repair::facts_text(&facts);
    cx.mail
        .reply(&step.task, &marked_line(repair::ATTEMPT_MARKER, &text))?;
    payload["branch"] = json!(facts.branch);
    payload["commit"] = json!(facts.commit);
    cx.advance_word(step, "built", payload)
}

/// What the runner published for `job`, when its receipt shows the job's own
/// publish ref pushed.
fn published(job: &PoolJob) -> Option<AttemptFacts> {
    let publish = job.outcome.as_ref()?.runner_publish()?;
    let wanted = job.publish_ref.as_deref()?;
    if !publish.succeeded() || publish.reference != wanted {
        return None;
    }
    Some(AttemptFacts {
        branch: wanted
            .strip_prefix("refs/heads/")
            .unwrap_or(wanted)
            .to_owned(),
        commit: publish.remote_sha,
        base: job.from_ref.clone()?,
    })
}

#[cfg(test)]
#[path = "repair_collect_test.rs"]
mod tests;
