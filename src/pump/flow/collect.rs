//! Collecting a finished role-step job into an outcome.
//!
//! Facts come from machines, judgment from models. A judgment class
//! (researcher, planner, admitter, slicer, auditor, judge, cto) takes its
//! answer word from the reply envelope. The `merge` class takes its word from
//! the runner publish receipt; its envelope is read only for judgment
//! fields and for the disagreement alarm.

use serde_json::json;

use crate::pump::attempt_record::hand_message;
use crate::pump::envelope::correct::{self, correction_n};
use crate::pump::envelope::roles::{Parsed, RoleKind, parse_for};
use crate::pump::envelope::{EnvelopeError, split};
use crate::pump::flow::budget_retry;
use crate::pump::flow::def::NodeClass;
use crate::pump::flow::index::StepJobRow;
use crate::pump::flow::mail::MailWriter;
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobStatus, PoolJob};

/// A step is retried once: the second failed attempt answers `failed`.
const MAX_ATTEMPTS: u32 = 2;
/// Corrections a step may spend on one broken envelope chain.
pub const MAX_CORRECTIONS: u32 = 2;
/// From this run on, a head-cto answer other than `lock` needs a `new_fact`.
const NEW_FACT_RUN: u32 = 3;

/// Exception kind: the envelope never parsed.
pub const ENVELOPE_FAILED: &str = "flow-envelope-failed";
/// Exception kind: the envelope's answer contradicts the machine receipt.
pub const ENVELOPE_DISAGREES: &str = "flow-envelope-disagrees";
/// Exception kind: an unspent budget kill has no legal raised budget to
/// retry with (the stored budget is unusable by the pool or already at the
/// ceiling, so no strict raise at or above it exists).
pub const BUDGET_UNRAISABLE: &str = "flow-budget-unraisable";

/// What a finished role-step job amounts to.
#[derive(Debug, Clone, PartialEq)]
pub enum Collected {
    /// The step answered `word`; the payload carries the facts and judgment
    /// (`{front, body}` for a judgment class, `{sha, ...}` for a merge).
    Outcome(String, serde_json::Value),
    /// No usable answer yet: run the step's job again.
    Retry,
    /// The envelope did not parse: run a correction job (it uses no attempt).
    Correct { error: EnvelopeError },
    /// A side-effect step with no decisive receipt: check git before answering.
    Verify,
    /// A budget kill: run the step's job again once at a raised budget
    /// before the step counts as failed.
    BudgetRetry { budget: u64 },
    /// The attempts or corrections are spent: the step answers `failed`.
    Failed,
}

/// Read what `job`, recorded by `step`, amounts to for a step of `class`.
///
/// `step.attempt` counts this job among the task's attempts (1-based); a
/// correction job's row carries the attempt of the job it corrects, so
/// corrections never count as attempts. `expected_ref` is the ref a merge
/// step must have published; other classes ignore it. Alarms go to the
/// exceptions log and parse errors on a merge go to the step task as a reply.
#[must_use]
pub fn collect<M: MailWriter>(
    ctx: &Ctx,
    mail: &mut M,
    job: &PoolJob,
    step: &StepJobRow,
    class: &NodeClass,
    expected_ref: &str,
) -> Collected {
    let Some(kind) = RoleKind::from_class(class) else {
        return Collected::Failed;
    };
    if kind == RoleKind::Merge {
        return merge(ctx, mail, job, step, expected_ref);
    }
    // Machine evidence decides before any envelope is read: an eligible
    // thinking-step budget kill takes the one raised retry or fails.
    if budget_retry::is_thinking(kind) && budget_retry::is_budget_kill(job) {
        return budget_kill(ctx, job, step);
    }
    if job.status != JobStatus::Finished {
        return lost(step);
    }
    let message = final_message(job);
    match parse_for(kind, &message) {
        Ok(parsed) => {
            judged(class, parsed, step.run).unwrap_or_else(|error| broken(ctx, job, step, error))
        }
        Err(error) => broken(ctx, job, step, error),
    }
}

/// The attempt rule for a job that timed out or was lost.
fn lost(step: &StepJobRow) -> Collected {
    if step.attempt < MAX_ATTEMPTS {
        Collected::Retry
    } else {
        Collected::Failed
    }
}

/// A thinking-step budget kill: the stored budget's strict raise, once.
/// A killed correction has its own budget policy and answers `failed` for
/// both kill representations, a spent marker has no raise left, and an
/// unspent historical budget the pool cannot strictly raise fails through
/// the existing route with a recorded reason instead of an invalid rerun.
fn budget_kill(ctx: &Ctx, job: &PoolJob, step: &StepJobRow) -> Collected {
    if job.kind == correct::KIND || budget_retry::spent(job) {
        return Collected::Failed;
    }
    match budget_retry::retry_budget(job) {
        Some(budget) => Collected::BudgetRetry { budget },
        None => {
            ctx.exception(
                BUDGET_UNRAISABLE,
                &step.task,
                &format!(
                    "{} job {}: budget {} has no strict raise at or above the pool ceiling",
                    step.node, job.id, job.budget
                ),
            );
            Collected::Failed
        }
    }
}

fn final_message(job: &PoolJob) -> String {
    job.outcome
        .as_ref()
        .map(|outcome| hand_message(&outcome.receipt))
        .unwrap_or_default()
}

/// A parsed envelope whose word the class accepts is the outcome. From run
/// `NEW_FACT_RUN` on, a head-cto answer other than `lock` must also carry a
/// non-empty `new_fact` (the field is checked for presence, not for truth).
fn judged(class: &NodeClass, parsed: Parsed, run: u32) -> Result<Collected, EnvelopeError> {
    if !class.words().contains(&parsed.answer.as_str()) {
        return Err(EnvelopeError::Invalid(format!(
            "answer {:?} is not one of this step's words",
            parsed.answer
        )));
    }
    if *class == NodeClass::Cto
        && run >= NEW_FACT_RUN
        && parsed.answer != "lock"
        && !has_new_fact(&parsed.front)
    {
        return Err(EnvelopeError::Invalid(format!(
            "run {run}: every head-cto answer but `lock` needs a non-empty `new_fact` \
             (expected a new fact since the last run; actual: none)"
        )));
    }
    let payload = json!({"front": parsed.front, "body": parsed.body});
    Ok(Collected::Outcome(parsed.answer, payload))
}

fn has_new_fact(front: &serde_json::Value) -> bool {
    front
        .get("new_fact")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|fact| !fact.trim().is_empty())
}

/// A broken envelope: a correction while some remain, then `failed`.
fn broken(ctx: &Ctx, job: &PoolJob, step: &StepJobRow, error: EnvelopeError) -> Collected {
    if correction_n(&job.id) < MAX_CORRECTIONS {
        return Collected::Correct { error };
    }
    ctx.exception(
        ENVELOPE_FAILED,
        &step.task,
        &format!("{} job {}: {error}", step.node, job.id),
    );
    Collected::Failed
}

/// The `merge` class: the runner publish receipt decides.
fn merge<M: MailWriter>(
    ctx: &Ctx,
    mail: &mut M,
    job: &PoolJob,
    step: &StepJobRow,
    expected_ref: &str,
) -> Collected {
    let Some(outcome) = job
        .outcome
        .as_ref()
        .filter(|_| job.status == JobStatus::Finished)
    else {
        return Collected::Verify;
    };
    let message = hand_message(&outcome.receipt);
    let (word, mut payload) = match outcome.runner_publish() {
        Some(p) if p.succeeded() && p.reference == expected_ref => {
            ("merged", json!({"sha": p.remote_sha}))
        }
        Some(_) => return Collected::Verify,
        None if outcome.agent_success() == Some(true) => ("conflict", json!({})),
        None => return Collected::Verify,
    };
    let parsed = parse_for(RoleKind::Merge, &message);
    let claimed = match &parsed {
        Ok(parsed) => Some(parsed.answer.clone()),
        Err(_) => claimed_answer(&message),
    };
    match parsed {
        Ok(parsed) => {
            payload["front"] = parsed.front;
            payload["body"] = json!(parsed.body);
        }
        Err(error) => {
            report_envelope_error(ctx, mail, step, job, &error);
        }
    }
    if let Some(claimed) = claimed.filter(|claimed| claimed != word) {
        ctx.exception(
            ENVELOPE_DISAGREES,
            &step.task,
            &format!(
                "{} job {}: envelope says {claimed}, runner receipt says {word}; receipt wins",
                step.node, job.id
            ),
        );
    }
    Collected::Outcome(word.to_owned(), payload)
}

/// A merge step's broken envelope never corrects (the machine decided): post
/// the parse error on the step task and raise the alarm.
fn report_envelope_error<M: MailWriter>(
    ctx: &Ctx,
    mail: &mut M,
    step: &StepJobRow,
    job: &PoolJob,
    error: &EnvelopeError,
) {
    if let Err(err) = mail.reply(&step.task, &format!("pump-envelope-error: {error}")) {
        ctx.log(&format!(
            "envelope error reply on {} failed: {err}",
            step.task
        ));
    }
    ctx.exception(
        ENVELOPE_FAILED,
        &step.task,
        &format!("{} job {}: {error}", step.node, job.id),
    );
}

/// The `answer` key of the front matter alone, whatever else is wrong with
/// the envelope; used only to detect a disagreement with the machine.
fn claimed_answer(message: &str) -> Option<String> {
    let Ok((front, _)) = split(message) else {
        return None;
    };
    let Ok(table) = toml::from_str::<toml::Table>(front) else {
        return None;
    };
    table.get("answer")?.as_str().map(str::to_owned)
}

#[cfg(test)]
#[path = "collect_test.rs"]
mod tests;
