//! Handler for the cto steps. N5 starts the CTO job with the run history and the
//! block-ship context; the CTO word is mapped to a flow word elsewhere.

use crate::error::VivariumError;
use crate::mailspace::shipped::HEAD_CTO_ROLE;
use crate::pump::envelope::roles::RoleKind;
use crate::pump::flow::advance::start::SENTINEL_NODE;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::need_branch;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler, role_job};
use crate::pump::flow::step_inputs::make_read_only;
use crate::pump::flow::verify_inputs::{capped, latest_of_class, rung_report};
use crate::pump::host::Ctx;
use crate::pump::jobs::JobStore;
use crate::pump::store::PumpStore;
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

/// Largest rung failure report the brief carries per rung row.
const RUNG_CAP: usize = 4_000;
/// The fanout answer that merged no slice.
const UNCHANGED: &str = "unchanged";
/// The nodes of the second verify pass.
const SECOND_PASS: [&str; 5] = ["build2", "audit2", "test2", "judge2", "raise2"];

/// What each CTO word does.
const WORDS: &str = "## Your words\n\n\
- research: restart the need at the research entry\n\
- plan: restart the need at the plan entry\n\
- slice: restart the need at the slice entry\n\
- rework: restart the need at the rework1 entry on the existing need branch\n\
- lock: lock the need for the operator\n";

/// Starts a cto step as a head-cto job.
struct CtoStart;

impl StepHandler for CtoStart {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let brief = brief(cx)?;
        let mut spec = role_job(
            cx.ctx,
            cx.run,
            step,
            "flow-cto",
            RoleKind::Cto,
            HEAD_CTO_ROLE,
            &brief,
        );
        make_read_only(cx.ctx, &mut spec);
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
    out.push_str(&why_reached(cx)?);
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
    out.push_str("## Failing rung output\n\n");
    out.push_str(&failing_rungs(&payloads));
    out.push_str(&block_ship(&payloads));
    out.push_str(&branch_availability(cx)?);
    out.push_str(WORDS);
    Ok(out)
}

/// The availability of rework, judged only by the stored need-branch row.
fn branch_availability(cx: &StartCx<'_>) -> Result<String, VivariumError> {
    let mut out = String::from("\n## Rework availability\n\n");
    match cx.store.need_branch(&cx.run.need)? {
        Some(row) => out.push_str(&format!(
            "A need-branch row is stored: rework restarts the need at rework1 on the existing \
             branch {} for target {}. The row itself is not evidence that any slice merged.\n",
            row.branch, row.target
        )),
        None => out.push_str(
            "No need-branch row exists, so rework is unavailable for this need. Research, \
             plan, slice and lock remain the choices.\n",
        ),
    }
    Ok(out)
}

/// The step that routed to the cto, and whether it is the block-ship case.
fn why_reached(cx: &StartCx<'_>) -> Result<String, VivariumError> {
    let rows = cx.store.steps_of(&cx.run.need, cx.run.run)?;
    let routed = rows.iter().rev().find(|row| {
        row.state == StepState::Done && row.node != SENTINEL_NODE && !is_cto(cx, &row.node)
    });
    let mut out = String::from("\n## Why the CTO was reached\n\n");
    let Some(row) = routed else {
        out.push_str("No finished step precedes the cto in this run.\n");
        return Ok(out);
    };
    out.push_str(&format!(
        "{} answered {}\n",
        row.node,
        row.verdict.as_deref().unwrap_or("none")
    ));
    if row.verdict.as_deref() == Some(UNCHANGED) {
        out.push_str(&unchanged_line(cx, row)?);
    } else if SECOND_PASS.contains(&row.node.as_str()) {
        out.push_str("Two verify passes failed: this is the automatic block-ship of the need.\n");
    }
    Ok(out)
}

/// The expected-against-actual line of a fanout that merged nothing.
fn unchanged_line(cx: &StartCx<'_>, row: &StepRow) -> Result<String, VivariumError> {
    let text = unchanged_text(
        cx.ctx,
        cx.store,
        &cx.run.need,
        cx.run.run,
        &row.node,
        &row.task,
    )?;
    Ok(format!("{text}\n"))
}

/// The expected-against-actual text of a fanout that merged nothing, without
/// the trailing newline. It states the real relation of the two tips.
pub(crate) fn unchanged_text(
    ctx: &Ctx,
    store: &PumpStore,
    need: &str,
    run: u32,
    fanout_node: &str,
    fanout_task: &str,
) -> Result<String, VivariumError> {
    let slices = store
        .slice_links_of_run(need, run)?
        .iter()
        .filter(|link| link.fanout.as_deref() == Some(fanout_task))
        .count();
    let current = need_branch::tip(ctx, store, need)?;
    let start = store.fanout_start(fanout_task)?.flatten();
    let actual = match (current.as_deref(), start.as_deref()) {
        (Some(t), Some(s)) if t == s => format!("need tip {t} == fanout start {s}"),
        (Some(t), Some(s)) => {
            format!("need tip {t} != fanout start {s} (the branch moved with no merged slice)")
        }
        (Some(t), None) => format!("need tip {t}, no fanout start recorded"),
        (None, Some(s)) => format!("need tip none, fanout start {s}"),
        (None, None) => "no need branch yet (need tip none, fanout start none)".to_owned(),
    };
    Ok(format!(
        "expected a change on vivi/need/{need} in fanout {fanout_node} ({slices} slices); actual: 0 merged, {actual}"
    ))
}

/// Whether `node` is a cto node of the flow.
fn is_cto(cx: &StartCx<'_>, node: &str) -> bool {
    cx.def
        .nodes
        .iter()
        .any(|n| n.id == node && n.class == NodeClass::Cto)
}

/// The stored failure facts of every rung that did not exit 0.
fn failing_rungs(payloads: &[StepPayloadRow]) -> String {
    let mut out = String::new();
    for row in payloads
        .iter()
        .filter(|row| row.class.starts_with("rung-") && row.payload["exit"].as_i64() != Some(0))
    {
        out.push_str(&format!(
            "run {} {}:\n{}\n\n",
            row.run,
            row.node,
            capped(&rung_report(row), RUNG_CAP)
        ));
    }
    if out.is_empty() {
        out.push_str("(none)\n\n");
    }
    out
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
