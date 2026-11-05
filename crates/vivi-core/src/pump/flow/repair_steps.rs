//! Repair rung-main and merge-main steps.
//!
//! Both run at the tip of the attempt's repair branch. The merge's CAS base is
//! the main tip the attempt was cut from, so a moved main is a conflict at once.

use serde_json::Value;

use crate::mailspace::pump_note::note_body;
use crate::pump::flow::collect_step::{RunCx, StepCollector};
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::{StepJobRow, StepRow};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::repair::{self, AttemptFacts};
use crate::pump::flow::rung_step::{self, RungCollector};
use crate::pump::jobs::{JobStatus, JobStore, PoolJob, merge_main, rung_result};
use vivi_error::VivariumError;
use vivi_mail::letter::records::NoteWord;

mod merge;

/// The rung a repair rung step runs.
const MAIN_RUNG: &str = "main";

/// The attempt number a repair rung or merge step node carries.
fn attempt_for(step: &StepRow) -> Result<u32, VivariumError> {
    repair::attempt_of(&step.node).ok_or_else(|| {
        VivariumError::Parse(format!("repair step node `{}` has no attempt", step.node))
    })
}

/// The target and the attempt facts a repair rung or merge step works from.
fn inputs(cx: &StartCx<'_>, step: &StepRow) -> Result<(String, AttemptFacts), VivariumError> {
    let need = &cx.run.need;
    let attempt = attempt_for(step)?;
    let (_, body) = cx
        .mail
        .open_record(need)?
        .ok_or_else(|| VivariumError::Other(format!("need {need} has no open record")))?;
    let rec = repair::record(&body)?;
    let facts = repair::latest_facts(
        cx.store,
        cx.mail,
        &cx.ctx.cfg.reply_from,
        need,
        cx.run.run,
        attempt,
    )?
    .ok_or_else(|| {
        VivariumError::Other(format!(
            "repair step {} of need {need} r{}: expected a published attempt {attempt} \
             (a pump-repair-attempt: reply on fix{attempt}); actual: none; \
             no published repair attempt",
            step.node, cx.run.run
        ))
    })?;
    Ok((rec.target, facts))
}

/// Starts the main rung on the attempt's repair branch tip.
struct RepairRung;

impl StepHandler for RepairRung {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let (target, facts) = inputs(cx, step)?;
        let spec = rung_step::job(cx.ctx, &target, MAIN_RUNG, &facts.commit, step, 1)?;
        Ok(StartAction::Job(Box::new(spec)))
    }
}

/// Starts the merge of the commit the rung step tested into main, cut from the
/// attempt's base. The newest facts are not re-read for the commit.
struct RepairMerge;

impl StepHandler for RepairMerge {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let (target, facts) = inputs(cx, step)?;
        let attempt = attempt_for(step)?;
        let tested = repair::tested_commit(
            cx.store,
            cx.mail,
            &cx.ctx.cfg.reply_from,
            &cx.run.need,
            cx.run.run,
            attempt,
        )?
        .ok_or_else(|| {
            VivariumError::Other(format!(
                "expected a tested commit for merge{attempt} (a pump-repair-rung: reply on \
                 rung{attempt}); actual: none; no tested commit for merge{attempt}"
            ))
        })?;
        let spec = merge_main::spec(
            cx.ctx,
            &target,
            &cx.run.need,
            &tested,
            &facts.base,
            &step.task,
        )
        .ok_or_else(|| {
            VivariumError::Config("expected a merge role in the routing; actual: none".to_owned())
        })?;
        Ok(StartAction::Job(Box::new(spec)))
    }
}

/// Posts the rung verdict for the next attempt, then collects like any rung.
/// An environment failure posts no verdict: the rung holds and says nothing
/// about the code.
struct RepairRungCollector;

impl<S: JobStore, M: FlowMail> StepCollector<S, M> for RepairRungCollector {
    fn collect(
        &self,
        cx: &mut RunCx<'_, S, M>,
        job: &PoolJob,
        row: &StepJobRow,
        step: &StepRow,
        class: &NodeClass,
    ) -> Result<(), VivariumError> {
        if job.status == JobStatus::Finished && rung_result::env_failure(job).is_none() {
            let tip = job.payload.get("tip").and_then(Value::as_str);
            let text = verdict_text(tip, &rung_step::answer(job).1);
            cx.mail
                .reply(&step.task, &note_body(NoteWord::RepairRung, None, &text)?)?;
        }
        RungCollector.collect(cx, job, row, step, class)
    }
}

/// `tested=<tip>` as the first word of the prose when the rung job carries a
/// `tip`, then [`rung_step::render_diagnostic`] of the enriched payload: the
/// rung, exact saved command, target, tested commit, exit, source job and
/// dispatch task, the canonical failing ids, and the bounded excerpt with its
/// own selection counts and cuts. Multiline evidence is kept as written, so the
/// source context is never flattened or cut a second time; the `tested=` word
/// stays parseable by [`repair::parse_tested`].
fn verdict_text(tip: Option<&str>, payload: &Value) -> String {
    let diagnostic = rung_step::render_diagnostic(payload);
    match tip {
        Some(tip) => format!("tested={tip}\n{diagnostic}"),
        None => diagnostic,
    }
}

/// Registers this module's handlers.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.handler(FlowKind::Repair, NodeClass::RungMain, Box::new(RepairRung));
    registry.handler(
        FlowKind::Repair,
        NodeClass::MergeMain,
        Box::new(RepairMerge),
    );
    registry.collector(
        FlowKind::Repair,
        NodeClass::RungMain,
        Box::new(RepairRungCollector),
    );
    registry.collector(
        FlowKind::Repair,
        NodeClass::MergeMain,
        Box::new(merge::RepairMergeMain),
    );
}

#[cfg(test)]
#[path = "repair_steps_test.rs"]
mod tests;
