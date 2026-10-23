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
use crate::pump::flow::index::StepState;
use crate::pump::flow::index::{StepJobRow, StepRow};
use crate::pump::flow::repair::{self, AttemptFacts};
use crate::pump::flow::tick::{OPERATOR_HOLD, raise_once};
use crate::pump::jobs::rung_result::{self, ENV_FAILURE};
use crate::pump::jobs::{JobStatus, JobStore, PoolJob};

/// The answer word of a repairer job that ran past its time cap.
const OVERTIME: &str = "overtime";

/// The answer word of a `built` attempt that changed nothing.
const UNCHANGED: &str = "unchanged";

/// The runner's `push:` word for an attempt with no change.
const NO_CHANGES: &str = "no_changes";

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
        if !is_correction && let Some(fact) = env_fact(job) {
            return hold_env(cx, job, step, &fact);
        }
        match collect::collect(cx.ctx, cx.mail, job, row, class, "") {
            Collected::Outcome(word, payload) if word == "built" => built(cx, job, step, payload),
            Collected::Outcome(word, payload) => cx.advance_word(step, &word, payload),
            Collected::Retry if !is_correction => {
                // A lost job may have pushed: the rerun publishes to a fresh ref.
                let fresh = format!(
                    "{}-t{}",
                    job.branch.as_deref().unwrap_or_default(),
                    row.attempt + 1
                );
                cx.rerun_with(job, row, step, |spec| {
                    spec.publish_ref = Some(format!("refs/heads/{fresh}"));
                    spec.branch = Some(fresh);
                })
            }
            Collected::Retry | Collected::Failed | Collected::Verify => {
                cx.advance_word(step, FAILED, json!({"job": job.id}))
            }
            Collected::Correct { error } => cx.correct(job, row, RoleKind::Repairer, &error),
        }
    }
}

/// The host-failure classes that come from runner facts. The other classes
/// scan stderr text, which the agent can influence.
const RUNNER_CLASSES: [&str; 3] = ["push_outcome", "agent_exit", "facts_exit"];

/// A runner-level environment fact of a repairer job: a host failure the
/// runner itself reports, or the agent binary exiting 126 or 127. Text markers
/// in the stderr tail never count. The evidence text, when there is a fact.
fn env_fact(job: &PoolJob) -> Option<String> {
    let host = job
        .outcome
        .as_ref()
        .and_then(|outcome| outcome.host_failure())
        .filter(|failure| RUNNER_CLASSES.contains(&failure.class.as_str()));
    if let Some(failure) = host {
        return Some(failure.detail);
    }
    match rung_result::parse(job).exit {
        Some(code @ (126 | 127)) => Some(format!("agent exit {code}")),
        _ => None,
    }
}

/// The repairer could not run: alarm once and hold the step for the
/// operator. Not an attempt, and no word.
fn hold_env<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    job: &PoolJob,
    step: &StepRow,
    fact: &str,
) -> Result<(), VivariumError> {
    let task = &step.task;
    raise_once(
        cx.ctx,
        ENV_FAILURE,
        &job.id,
        &format!(
            "repairer job {} for step {task}: expected the agent to run; actual: {fact}; \
             answer with vivi pump answer {task} built|failed",
            job.id
        ),
    );
    cx.store
        .set_step_state(task, StepState::Waiting, Some(OPERATOR_HOLD))
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
        let (word, reason) = unpublished(&original);
        return cx.advance_word(step, word, json!({"job": job.id, "reason": reason}));
    };
    let text = repair::facts_text(&facts);
    cx.mail
        .reply(&step.task, &marked_line(repair::ATTEMPT_MARKER, &text))?;
    payload["branch"] = json!(facts.branch);
    payload["commit"] = json!(facts.commit);
    cx.advance_word(step, "built", payload)
}

/// The pool summary's `push: <word>` of a receipt: only the lines before the
/// first section and before the hand's own text count. Empty when absent.
fn push_word(receipt: &str) -> &str {
    let summary = receipt
        .split("\n\n--- ")
        .next()
        .and_then(|head| head.split("hand's final message:").next())
        .unwrap_or_default();
    summary
        .lines()
        .find_map(|line| line.trim().strip_prefix("push: "))
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or_default()
}

/// The word and reason for a `built` envelope whose publish does not hold: an
/// empty attempt (`push: no_changes`) is `unchanged`, anything else `failed`.
fn unpublished(original: &PoolJob) -> (&'static str, String) {
    let publish_ref = original.publish_ref.as_deref().unwrap_or("none");
    let receipt = original
        .outcome
        .as_ref()
        .map(|outcome| outcome.receipt.as_str())
        .unwrap_or_default();
    let word = push_word(receipt);
    if word == NO_CHANGES {
        let base = original.from_ref.as_deref().unwrap_or("none");
        let reason = format!(
            "expected a commit on {publish_ref} from base {base}; actual: the runner saw no change (push {NO_CHANGES})"
        );
        return (UNCHANGED, reason);
    }
    let runner = original
        .outcome
        .as_ref()
        .and_then(|outcome| outcome.runner_publish())
        .map_or_else(
            || String::from("none"),
            |publish| format!("{} {}", publish.state, publish.reference),
        );
    let shown = if word.is_empty() { "none" } else { word };
    let reason = format!(
        "expected a publish of {publish_ref}; actual: push {shown}, runner publish {runner}"
    );
    (FAILED, reason)
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
