//! Posts a landed research analysis on its want.
//!
//! The landing terminal can neither read the need body that holds the
//! `for: <want>` line nor see the job store, so the post is a tick phase over
//! `done` research runs. The pump.db `applied_actions` set is the durable
//! done-marker: the key is written only after the reply succeeded, so a failed
//! reply is tried again on the next tick.

use crate::error::VivariumError;
use crate::pump::attempt_record::hand_message;
use crate::pump::envelope::roles::{RoleKind, parse_for};
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::{RunRow, StepRow};
use crate::pump::flow::research::for_want;
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobStatus, JobStore, PoolJob};
use crate::pump::store::PumpStore;

/// Run state of a landed run.
const RUN_DONE: &str = "done";
/// Flow kind of a research run.
const KIND_RESEARCH: &str = "research";
/// Node id of the researcher step.
const RESEARCH_NODE: &str = "research";

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
        for run in store.runs_in_state(RUN_DONE)? {
            if run.kind != KIND_RESEARCH {
                continue;
            }
            let key = format!("research-post:{}:r{}", run.need, run.run);
            if applied.contains(&key) {
                continue;
            }
            post_run(ctx, st, store, mail, &run)?;
            store.mark_action_applied(&key, ctx.now())?;
        }
        Ok(())
    }
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
    let Some(want) = for_want(&mail.task_body(need)?) else {
        return Ok(());
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
        return Ok(());
    };
    let text = format!(
        "pump: research analysis for need {need} r{} (answer {word})\n\n{body}",
        run.run
    );
    mail.reply(&want, &text)
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
