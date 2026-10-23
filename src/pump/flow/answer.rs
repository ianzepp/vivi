//! The operator's answer to a flow step: `vivi pump answer <step-task> <word>`.
//!
//! The verb posts an `answer` action on the need's open pump record
//! ([`crate::mailspace::PumpActionRequest::answer`]). Each tick the flow
//! engine reads the answers it has not applied ([`apply_pending`]) and routes
//! each one through [`advance`](crate::pump::flow::advance) as the step's
//! outcome, so the step closes with that word as verdict by the normal path.
//! Closing the step task by hand instead is a foreign close, and the guard
//! reopens it.
//!
//! Every answer letter is applied or refused exactly once: its id goes into
//! `applied_actions` either way, and a refusal raises `flow-answer-refused`
//! naming the reason.

use std::path::Path;

use crate::error::VivariumError;
use crate::mailspace::{Mailspace, PumpAction, PumpActionKind};
use crate::pump::flow::advance::{self, route::Decide};
use crate::pump::flow::def::FlowDef;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter};
use crate::pump::flow::outcome::Outcome;
use crate::pump::flow::run::SENTINEL_NODE;
use crate::pump::host::Ctx;
use crate::pump::store::PumpStore;

/// The exception kind raised for an answer that cannot be applied.
pub const ANSWER_REFUSED: &str = "flow-answer-refused";

/// The run state of a run that is still being advanced.
const RUN_LIVE: &str = "live";

/// Loads the flow a run follows.
pub(crate) type DefFor<'a> = &'a dyn Fn(&RunRow) -> Result<FlowDef, VivariumError>;

/// Post the answer and print the posted action (the `vivi pump answer` verb).
///
/// # Errors
/// Returns a [`VivariumError`] when the task is not a pump step task, its need
/// has no open pump record, or the post fails.
pub fn run(
    project: &Path,
    step: &str,
    word: &str,
    note: Option<&str>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(Some(project))?;
    let posted = mailspace.pump_post_answer(None, step, word, note.unwrap_or_default())?;
    println!(
        "answer {} posted on {} for need {}; the pump applies it at its next tick",
        word.trim(),
        step,
        posted.need
    );
    Ok(())
}

/// Apply every answer action the engine has not applied yet.
///
/// `def_for` loads the flow a run follows; `decide` supplies the code
/// decisions routing may reach after the answer. A refusal raises
/// [`ANSWER_REFUSED`]; an error that is not a refusal (the mailspace or the
/// index cannot be read) fails the call and leaves the letters unapplied for
/// the next tick.
///
/// # Errors
/// Returns a [`VivariumError`] when pending actions or the applied set cannot
/// be read or saved.
pub(crate) fn apply_pending<M: MailWriter + MailReader>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    def_for: DefFor<'_>,
    decide: Decide<'_>,
) -> Result<(), VivariumError> {
    let Some(mailspace) = crate::pump::record::open_mailspace(ctx) else {
        return Ok(());
    };
    let applied = store.applied_action_ids()?;
    for action in mailspace.pump_pending_actions(&applied)? {
        if action.request.kind != PumpActionKind::Answer {
            continue;
        }
        let task = action.request.step.clone().unwrap_or_default();
        match apply_one(ctx, store, mail, def_for, decide, &action) {
            Ok(()) => ctx.log(&format!("answer applied: {task}")),
            Err(reason) => ctx.exception(ANSWER_REFUSED, &task, &reason),
        }
        store.mark_action_applied(&action.id, ctx.host.now())?;
    }
    Ok(())
}

/// Apply one answer; the error is the reason it was refused.
fn apply_one<M: MailWriter + MailReader>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    def_for: DefFor<'_>,
    decide: Decide<'_>,
    action: &PumpAction,
) -> Result<(), String> {
    let task = action.request.step.as_deref().unwrap_or_default();
    let word = action.request.word.as_deref().unwrap_or_default();
    let step = open_step(store, &action.need, task)?;
    let run = live_run(store, &action.need, step.run)?;
    let def =
        def_for(&run).map_err(|error| format!("cannot load the {} flow: {error}", run.kind))?;
    let node = def
        .nodes
        .iter()
        .find(|node| node.id == step.node)
        .ok_or_else(|| format!("node `{}` is not in the {} flow", step.node, run.kind))?;
    let words = node.class.words();
    if word.is_empty() || (!words.is_empty() && !words.contains(&word)) {
        return Err(format!(
            "`{word}` is not an answer of node `{}` (answers: {})",
            node.id,
            words.join(", ")
        ));
    }
    let outcome = Outcome {
        need: action.need.clone(),
        run: step.run,
        node: step.node.clone(),
        task: step.task.clone(),
        word: word.to_string(),
        payload: serde_json::json!({
            "source": "operator",
            "note": note_of(action),
        }),
    };
    match advance::advance(ctx, store, mail, &def, &outcome, decide) {
        Ok(advanced) => advanced.routed.rejected.map_or(Ok(()), Err),
        Err(error) => Err(format!("routing the answer failed: {error}")),
    }
}

/// The step row of `task` when it is open or held: not closed, not a sentinel.
fn open_step(store: &PumpStore, need: &str, task: &str) -> Result<StepRow, String> {
    let step = store
        .step_by_task(task)
        .map_err(|error| format!("cannot read the step index: {error}"))?
        .ok_or_else(|| format!("no flow step has task {task}"))?;
    let rows = store
        .steps_of(need, step.run)
        .map_err(|error| format!("cannot read the step index: {error}"))?;
    if !rows.iter().any(|row| row.task == task) {
        return Err(format!("task {task} is not a step of need {need}"));
    }
    if step.node == SENTINEL_NODE {
        return Err(format!(
            "task {task} is the run sentinel, which takes no answer; answer one of its steps"
        ));
    }
    match step.state {
        StepState::Done | StepState::Failed => Err(format!(
            "step {task} ({}) is already closed{}",
            step.node,
            step.verdict
                .as_deref()
                .map_or_else(String::new, |verdict| format!(" with `{verdict}`"))
        )),
        StepState::Pending | StepState::Running | StepState::Waiting => Ok(step),
    }
}

fn live_run(store: &PumpStore, need: &str, run: u32) -> Result<RunRow, String> {
    let row = store
        .runs_of(need)
        .map_err(|error| format!("cannot read the run index: {error}"))?
        .into_iter()
        .find(|row| row.run == run)
        .ok_or_else(|| format!("need {need} has no run r{run}"))?;
    if row.state == RUN_LIVE {
        Ok(row)
    } else {
        Err(format!(
            "run r{run} of need {need} is {}, not live",
            row.state
        ))
    }
}

/// The operator's note: the letter's body, unless it is only the default
/// text a note-less post carries.
fn note_of(action: &PumpAction) -> String {
    let reason = action.request.reason.as_str();
    if reason == format!("{} {}", PumpActionKind::Answer.as_str(), action.need) {
        String::new()
    } else {
        reason.to_string()
    }
}

#[cfg(test)]
#[path = "answer_test.rs"]
mod tests;
