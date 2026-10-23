//! Repair rung-main and merge-main steps.
//!
//! Both run at the tip of the attempt's repair branch. The merge's CAS base is
//! the main tip the attempt was cut from, so a moved main is a conflict at once.

use serde_json::Value;

use crate::error::VivariumError;
use crate::mailspace::marked_line;
use crate::pump::flow::collect_step::{RunCx, StepCollector};
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::{StepJobRow, StepRow};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::repair::{self, AttemptFacts};
use crate::pump::flow::rung_step::{self, RungCollector};
use crate::pump::jobs::{JobStatus, JobStore, PoolJob, merge_main};

/// The rung a repair rung step runs.
const MAIN_RUNG: &str = "main";
/// Characters of the rung tail a verdict reply keeps.
const TAIL_CHARS: usize = 2000;

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
    .ok_or_else(|| VivariumError::Other("no published repair attempt".to_owned()))?;
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
        .ok_or_else(|| VivariumError::Other(format!("no tested commit for merge{attempt}")))?;
        let spec = merge_main::spec(
            cx.ctx,
            &target,
            &cx.run.need,
            &tested,
            &facts.base,
            &step.task,
        )
        .ok_or_else(|| VivariumError::Config("no merge role".to_owned()))?;
        Ok(StartAction::Job(Box::new(spec)))
    }
}

/// Posts the rung verdict for the next attempt, then collects like any rung.
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
        if job.status == JobStatus::Finished {
            let tip = job.payload.get("tip").and_then(Value::as_str);
            let text = verdict_text(tip, &rung_step::answer(job).1);
            cx.mail
                .reply(&step.task, &marked_line(repair::RUNG_MARKER, &text))?;
        }
        RungCollector.collect(cx, job, row, step, class)
    }
}

/// `tested=<tip>; exit <code>; failing <ids>; tail: <first 2000 chars>` on one
/// line; the `tested=` part is left out when the rung job has no `tip`.
fn verdict_text(tip: Option<&str>, payload: &Value) -> String {
    let exit = payload
        .get("exit")
        .and_then(Value::as_i64)
        .map_or_else(|| "none".to_owned(), |code| code.to_string());
    let failing: Vec<&str> = payload
        .get("failing")
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let tail: String = payload
        .get("tail")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .chars()
        .take(TAIL_CHARS)
        .collect();
    let tested = tip.map_or_else(String::new, |tip| format!("tested={tip}; "));
    format!(
        "{tested}exit {exit}; failing {}; tail: {}",
        failing.join(" "),
        tail.replace('\n', " | ")
    )
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
}

#[cfg(test)]
#[path = "repair_steps_test.rs"]
mod tests;
