//! Handler for the cto steps. N5 starts the CTO job with the run history and the
//! block-ship context; the CTO word is mapped to a flow word elsewhere.

use crate::error::VivariumError;
use crate::mailspace::shipped::HEAD_CTO_ROLE;
use crate::pump::flow::advance::start::SENTINEL_NODE;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler, role_job};
use crate::pump::flow::verify_inputs::{capped, latest_of_class};
use crate::pump::jobs::JobStore;
use crate::pump::store::step_payload::StepPayloadRow;

/// Classes whose stored reply the CTO reads as a key reply.
const KEY_CLASSES: [&str; 4] = ["admitter", "auditor", "judge", "cto"];
/// The audit answer that carries the block-ship context.
const BLOCK_SHIP: &str = "block_ship";
/// Largest key reply front the brief carries.
const FRONT_CAP: usize = 4_000;
/// Largest block-ship audit body the brief carries.
const BLOCK_CAP: usize = 30_000;
/// The run count from which every answer but `lock` needs a new fact.
const FACT_RUN: usize = 3;

/// What each CTO word does.
const WORDS: &str = "## Your words\n\n\
- research: restart the need at the research entry\n\
- plan: restart the need at the plan entry\n\
- slice: restart the need at the slice entry\n\
- rework: restart the need at the rework entry\n\
- lock: lock the need for the operator\n";

/// Starts a cto step as a head-cto job.
struct CtoStart;

impl StepHandler for CtoStart {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let brief = brief(cx)?;
        let spec = role_job(cx.ctx, cx.run, step, "flow-cto", HEAD_CTO_ROLE, &brief);
        Ok(StartAction::Job(Box::new(spec)))
    }
}

fn brief(cx: &StartCx<'_>) -> Result<String, VivariumError> {
    let need = &cx.run.need;
    let runs = cx.store.runs_of(need)?;
    let mut out = format!("CTO ruling\n\nneed: {need}\nruns: {}\n", runs.len());
    if runs.len() >= FACT_RUN {
        out.push_str(
            "\nThis is run 3 or later: every answer but `lock` needs a `new_fact` in the envelope.\n",
        );
    }
    out.push_str("\n## Run history\n");
    for run in &runs {
        out.push_str(&format!(
            "\nrun {}: entry {}, state {}\n",
            run.run, run.entry, run.state
        ));
        for row in cx.store.steps_of(need, run.run)? {
            if row.node == SENTINEL_NODE {
                continue;
            }
            out.push_str(&format!(
                "- {} {} {} {}\n",
                row.node,
                row.task,
                format!("{:?}", row.state).to_lowercase(),
                row.verdict.as_deref().unwrap_or("none")
            ));
        }
    }
    let payloads = cx.store.step_payloads_of_need(need)?;
    out.push_str("\n## Key replies\n\n");
    let mut any = false;
    for row in payloads
        .iter()
        .filter(|row| KEY_CLASSES.contains(&row.class.as_str()))
    {
        any = true;
        out.push_str(&format!(
            "run {} {} ({}):\n{}\n\n",
            row.run,
            row.node,
            row.class,
            capped(&front_text(row)?, FRONT_CAP)
        ));
    }
    if !any {
        out.push_str("(none recorded)\n\n");
    }
    out.push_str(&block_ship(&payloads));
    out.push_str(WORDS);
    Ok(out)
}

/// A reply front as JSON text.
fn front_text(row: &StepPayloadRow) -> Result<String, VivariumError> {
    serde_json::to_string(&row.payload["front"])
        .map_err(|error| VivariumError::Message(format!("encode reply front: {error}")))
}

/// The full body of the newest audit, when it answered `block_ship`.
fn block_ship(payloads: &[StepPayloadRow]) -> String {
    let Some(row) = latest_of_class(payloads, "auditor") else {
        return String::new();
    };
    if row.payload["front"]["answer"].as_str() != Some(BLOCK_SHIP) {
        return String::new();
    }
    let body = row.payload["body"].as_str().unwrap_or("");
    format!(
        "## Block-ship audit\n\nThe newest audit ({}) answered block_ship. Its full report:\n\n{}\n\n",
        row.node,
        capped(body, BLOCK_CAP)
    )
}

pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.handler(FlowKind::Build, NodeClass::Cto, Box::new(CtoStart));
}

#[cfg(test)]
#[path = "cto_step_test.rs"]
mod tests;
