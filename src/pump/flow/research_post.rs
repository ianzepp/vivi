//! Posts a landed research analysis on its want.
//!
//! The landing terminal can neither read the need body that holds the
//! `explores` front-matter key nor see the job store, so the post is a tick phase over
//! `done` research runs. The pump.db `applied_actions` set is the durable
//! done-marker: the key is written only after the reply succeeded, so a failed
//! reply is tried again on the next tick. One run that fails does not stop the
//! runs after it: the first error is returned once all of them were tried.

use crate::error::VivariumError;
use crate::pump::attempt_record::hand_message;
use crate::pump::envelope::roles::{RoleKind, parse_for};
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::run_state;
use crate::pump::flow::index::{RunRow, StepRow};
use crate::pump::flow::research::for_want;
use crate::pump::flow::step_error::{STEP_ERROR_LIMIT, note_failure, note_success};
use crate::pump::flow::tick::raise_once;
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobStatus, JobStore, PoolJob};
use crate::pump::store::PumpStore;

/// Flow kind of a research run.
const KIND_RESEARCH: &str = "research";
/// Node id of the researcher step.
const RESEARCH_NODE: &str = "research";
/// Exception kind raised once when a research run ended without an envelope.
pub(crate) const RESEARCH_FAILED: &str = "flow-research-failed";
/// Exception kind raised once when the research post is given up.
pub(crate) const RESEARCH_POST_FAILED: &str = "flow-research-post-failed";

struct ResearchPost;

impl<S: JobStore, M: FlowMail> TickPhase<S, M> for ResearchPost {
    fn name(&self) -> &'static str {
        "research-post"
    }

    fn run(
        &self,
        ctx: &Ctx,
        st: &mut S,
        store: &PumpStore,
        mail: &mut M,
    ) -> Result<(), VivariumError> {
        let applied = store.applied_action_ids()?;
        let mut first_error = None;
        for run in store.runs_in_state(run_state::DONE)? {
            if run.kind != KIND_RESEARCH {
                continue;
            }
            let key = format!("research-post:{}:r{}", run.need, run.run);
            if applied.contains(&key) {
                continue;
            }
            match post_run(ctx, st, store, mail, &run) {
                Ok(()) => {
                    note_success(ctx, &key);
                    store.mark_action_applied(&key, ctx.now())?;
                }
                Err(error) => {
                    ctx.log(&format!(
                        "research-post: {} r{} failed: {error}",
                        run.need, run.run
                    ));
                    let n = note_failure(ctx, &key);
                    if n >= STEP_ERROR_LIMIT {
                        give_up(ctx, mail, &run, &error, n);
                        store.mark_action_applied(&key, ctx.now())?;
                    } else {
                        first_error.get_or_insert(error);
                    }
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

/// Raise the give-up alarm for a research post that failed `n` ticks in a row.
fn give_up<M: FlowMail>(ctx: &Ctx, mail: &M, run: &RunRow, error: &VivariumError, n: u32) {
    let want = match mail.task_body(&run.need) {
        Ok(body) => match for_want(&body) {
            Ok(want) => want.unwrap_or_default(),
            Err(read) => format!("(unreadable: {read})"),
        },
        Err(read) => format!("(unreadable: {read})"),
    };
    raise_once(
        ctx,
        RESEARCH_POST_FAILED,
        &run.need,
        &format!(
            "research post for {} r{} to want {want}: expected the reply to send; actual: {error} on {n} ticks; given up",
            run.need, run.run
        ),
    );
}

/// Post the analysis of one landed research run. `Ok` means the key may be
/// marked: the reply was sent, or nothing is left to send.
fn post_run<S: JobStore, M: FlowMail>(
    ctx: &Ctx,
    st: &S,
    store: &PumpStore,
    mail: &mut M,
    run: &RunRow,
) -> Result<(), VivariumError> {
    let need = &run.need;
    let Some(stored) = for_want(&mail.task_body(need)?)? else {
        return Ok(());
    };
    let Some(want) = mail.resolve_want(&stored)? else {
        return Err(VivariumError::Message(format!(
            "want {stored} is not in this mailspace"
        )));
    };
    let steps = store.steps_of(need, run.run)?;
    let Some(step) = steps.iter().find(|step| step.node == RESEARCH_NODE) else {
        ctx.log(&format!(
            "research-post: {need} r{} has no research step",
            run.run
        ));
        return Ok(());
    };
    let Some((word, body)) = analysis(st, step) else {
        ctx.log(&format!(
            "research-post: no researcher envelope found for {need} r{}",
            run.run
        ));
        return post_failure(ctx, store, mail, run, step, &want);
    };
    let text = format!(
        "pump: research analysis for need {need} r{} (answer {word})\n\n{body}",
        run.run
    );
    mail.reply(&want, &text)
}

/// A run that ended with no researcher envelope tells its want and alarms once.
fn post_failure<M: FlowMail>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    run: &RunRow,
    step: &StepRow,
    want: &str,
) -> Result<(), VivariumError> {
    let verdict = step.verdict.as_deref().unwrap_or("none");
    let job = store
        .step_payload(&step.task)?
        .and_then(|row| {
            row.payload
                .get("job")
                .and_then(|v| v.as_str().map(str::to_owned))
        })
        .map(|id| format!(", job {id} from the step payload"))
        .unwrap_or_default();
    let text = format!(
        "pump: research for need {} r{} failed: expected a researcher envelope on task {}; actual: step verdict {verdict}{job}",
        run.need, run.run, step.task
    );
    mail.reply(want, &text)?;
    raise_once(ctx, RESEARCH_FAILED, &run.need, &text);
    Ok(())
}

/// The newest finished job of the step whose reply is a researcher envelope:
/// `(answer word, body below the envelope)`. Correction jobs count.
fn analysis(st: &impl JobStore, step: &StepRow) -> Option<(String, String)> {
    let mut finished: Vec<&PoolJob> = st
        .jobs()
        .values()
        .filter(|job| job.status == JobStatus::Finished)
        .filter(|job| job.task.as_deref() == Some(step.task.as_str()))
        .collect();
    finished.sort_by_key(|job| job.outcome.as_ref().map_or(0, |outcome| outcome.at));
    finished.iter().rev().find_map(|job| {
        let outcome = job.outcome.as_ref()?;
        let Ok(parsed) = parse_for(RoleKind::Researcher, &hand_message(&outcome.receipt)) else {
            return None;
        };
        Some((parsed.answer, parsed.body.trim().to_owned()))
    })
}

/// Registers this module's handlers.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.phase(Box::new(ResearchPost));
}

#[cfg(test)]
#[path = "research_post_test.rs"]
mod tests;
