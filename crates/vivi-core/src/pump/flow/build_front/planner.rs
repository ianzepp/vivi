//! The planner step handler (plan, plan2): a read-only pump-planner role job.

use std::fmt::Write as _;

use crate::mailspace::shipped::PUMP_PLANNER_ROLE;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::step_inputs::{
    done_step, need_thread, read_only_job, step_replies, targets,
};
use crate::pump::jobs::JobStore;
use vivi_error::VivariumError;

/// The node whose answer `plan2` builds on.
const FIRST_PLAN: &str = "plan";
/// The node whose reasons `plan2` reads.
const FIRST_ADMIT: &str = "admit1";
/// The node that gets the earlier plan and admitter reasons.
const SECOND_PLAN: &str = "plan2";
/// What a missing reply reads as.
const NONE: &str = "(none)";

/// Starts the planner as a read-only role job.
struct PlannerStep;

impl StepHandler for PlannerStep {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let brief = brief(cx, step)?;
        let spec = read_only_job(
            cx.ctx,
            cx.run,
            step,
            &NodeClass::Planner,
            PUMP_PLANNER_ROLE,
            &brief,
        )?;
        Ok(StartAction::Job(Box::new(spec)))
    }
}

/// The planner's brief for `step`.
fn brief(cx: &StartCx<'_>, step: &StepRow) -> Result<String, VivariumError> {
    let need = &cx.run.need;
    let mut text = format!("need: {need}\nrun: r{}\n", cx.run.run);
    let fmt_err = |e: std::fmt::Error| VivariumError::Other(e.to_string());
    writeln!(
        text,
        "targets: {}\nEvery unit or slice names exactly one of these targets.",
        targets(cx.ctx).join(", ")
    )
    .map_err(fmt_err)?;
    text.push_str(&front_lines(cx, need)?);
    writeln!(text, "thread:\n{}", need_thread(cx.ctx, need)?).map_err(fmt_err)?;
    if step.node == SECOND_PLAN {
        writeln!(
            text,
            "first plan:\n{}\nadmitter reasons:\n{}",
            newest_reply(cx, FIRST_PLAN)?,
            newest_reply(cx, FIRST_ADMIT)?
        )
        .map_err(fmt_err)?;
    }
    if cx.run.run > 1 {
        text.push_str(&prior_branch_note(cx)?);
    }
    Ok(text)
}

/// The front-matter facts of the need as brief lines: one `done_when:` line
/// per entry, `target:` when set, and `depends_on:` from the effective
/// prerequisites.
///
/// # Errors
/// Returns [`VivariumError`] naming the need when its body cannot be read or
/// is not a valid need letter, or when its prerequisites cannot be read.
fn front_lines(cx: &StartCx<'_>, need: &str) -> Result<String, VivariumError> {
    let body = cx.mail.task_body(need)?;
    let envelope = vivi_mail::letter::read::need_of(need, &body)
        .map_err(|error| VivariumError::Message(format!("need {need}: {error}")))?;
    let front = envelope.front;
    let mut lines: Vec<String> = front
        .done_when
        .iter()
        .map(|entry| format!("done_when: {entry}\n"))
        .collect();
    if let Some(target) = front.target {
        lines.push(format!("target: {target}\n"));
    }
    let prerequisites = cx.mail.need_prerequisites(need)?;
    if !prerequisites.is_empty() {
        lines.push(format!("depends_on: {}\n", prerequisites.join(", ")));
    }
    Ok(lines.concat())
}

/// The newest reply of the last Done step of `node`, or `(none)`.
fn newest_reply(cx: &StartCx<'_>, node: &str) -> Result<String, VivariumError> {
    let Some(row) = done_step(cx.store, cx.run, node)? else {
        return Ok(NONE.to_owned());
    };
    let mut replies = step_replies(cx.mail, &row.task)?;
    Ok(if replies.is_empty() {
        NONE.to_owned()
    } else {
        replies.swap_remove(0)
    })
}

/// A line naming the last prior need branch, or nothing when there is none.
fn prior_branch_note(cx: &StartCx<'_>) -> Result<String, VivariumError> {
    let last = cx
        .store
        .need_branch(&cx.run.need)?
        .and_then(|row| row.priors.last().cloned());
    Ok(last.map_or_else(String::new, |name| {
        format!("prior need branch: {name}\nThe prior need branch is readable for reference.\n")
    }))
}

/// Registry hook: the planner handler for plan and plan2.
pub(super) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.handler(FlowKind::Build, NodeClass::Planner, Box::new(PlannerStep));
}

#[cfg(test)]
#[path = "planner_test.rs"]
mod tests;
