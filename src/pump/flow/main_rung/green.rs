//! Records a green main rung and answers the waiting repair step.
//!
//! A green main rung for a target whose repair need waits at `unrepaired`
//! answers that step `green` through `advance`, which lands the need. A repair
//! run at any other node whose tested tip is already on main is landed outside
//! the flow: its need closes and the run is withdrawn.

use serde_json::json;

use crate::error::VivariumError;
use crate::pump::flow::advance::route::JoinInput;
use crate::pump::flow::advance::{advance, withdraw};
use crate::pump::flow::def::{FlowDef, FlowNode};
use crate::pump::flow::handlers::FlowMail;
use crate::pump::flow::index::{RunRow, StepState};
use crate::pump::flow::mail::MailReader;
use crate::pump::flow::outcome::Outcome;
use crate::pump::flow::repair;
use crate::pump::flow::tick::{OPERATOR_HOLD, WORD_REJECTED};
use crate::pump::flow::verify;
use crate::pump::host::Ctx;
use crate::pump::store::PumpStore;

/// The repair flow node that waits for a green main rung.
pub(crate) const UNREPAIRED_NODE: &str = "unrepaired";

/// Answer `green` on every unrepaired step of a live repair run for `target`.
/// A repair run with no open unrepaired step whose `tested:` tip main already
/// contains is landed outside the flow: the need closes with a note naming the
/// green tip and the run is withdrawn (an in-flight job's result is ignored).
///
/// Returns the answered step tasks and the needs closed outside the flow.
///
/// # Errors
/// Returns a [`VivariumError`] from the store, mail port or `advance`.
pub(crate) fn answer_green<M: FlowMail>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    def: &FlowDef,
    target: &str,
    tip: &str,
) -> Result<Vec<String>, VivariumError> {
    let mut answered = Vec::new();
    for run in store.live_runs()? {
        if run.kind != "repair" || !records_target(&*mail, &run.need, target)? {
            continue;
        }
        let mut at_unrepaired = false;
        for step in store.steps_of(&run.need, run.run)? {
            let open = matches!(step.state, StepState::Pending | StepState::Waiting);
            if step.node != UNREPAIRED_NODE || !open {
                continue;
            }
            at_unrepaired = true;
            let outcome = Outcome {
                need: run.need.clone(),
                run: run.run,
                node: step.node.clone(),
                task: step.task.clone(),
                word: String::from("green"),
                payload: json!({"source": "main-rung", "tip": tip}),
            };
            let decide = |_: &FlowNode, _: &[JoinInput]| {
                Err(VivariumError::Parse(String::from(
                    "no decision on the green path",
                )))
            };
            let advanced = advance(ctx, store, mail, def, &outcome, &decide)?;
            if let Some(reason) = advanced.routed.rejected {
                ctx.exception(WORD_REJECTED, &step.task, &reason);
                store.set_step_state(&step.task, StepState::Waiting, Some(OPERATOR_HOLD))?;
            }
            answered.push(step.task);
        }
        if !at_unrepaired && land_outside_flow(ctx, store, mail, &run, target, tip)? {
            answered.push(run.need.clone());
        }
    }
    Ok(answered)
}

/// Close the repair need and withdraw its run when main contains the tested
/// tip. Returns true when the need was closed.
fn land_outside_flow<M: FlowMail>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    run: &RunRow,
    target: &str,
    tip: &str,
) -> Result<bool, VivariumError> {
    let Some((_, body)) = MailReader::open_record(&*mail, &run.need)? else {
        return Ok(false);
    };
    let Some(tested) = repair::record(&body)?.tested else {
        return Ok(false);
    };
    let Some(tctx) = ctx.for_target(target) else {
        return Ok(false);
    };
    match verify::ref_contains(ctx, target, &tctx.cfg.main, &tested) {
        Ok(Some(_)) => {
            let note = format!("main green at {tip} outside the flow (tested {tested})");
            mail.close_need(&run.need, &note)?;
            withdraw(store, mail, run)?;
            Ok(true)
        }
        Ok(None) => Ok(false),
        Err(reason) => {
            ctx.log(&format!(
                "main rung: cannot tell if main holds {tested} for {}: {reason}",
                run.need
            ));
            Ok(false)
        }
    }
}

fn records_target<M: FlowMail>(mail: &M, need: &str, target: &str) -> Result<bool, VivariumError> {
    let record = MailReader::open_record(mail, need)?;
    Ok(record.is_some_and(|(_, body)| super::record_target(&body) == Some(target)))
}

#[cfg(test)]
#[path = "green_test.rs"]
mod tests;
