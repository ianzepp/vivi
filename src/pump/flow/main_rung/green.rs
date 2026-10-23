//! Records a green main rung and answers the waiting repair step.
//!
//! A green main rung for a target whose repair need waits at `unrepaired`
//! answers that step `green` through `advance`, which lands the need.

use serde_json::json;

use crate::error::VivariumError;
use crate::pump::flow::advance::advance;
use crate::pump::flow::advance::route::JoinInput;
use crate::pump::flow::def::{FlowDef, FlowNode};
use crate::pump::flow::handlers::FlowMail;
use crate::pump::flow::index::StepState;
use crate::pump::flow::mail::MailReader;
use crate::pump::flow::outcome::Outcome;
use crate::pump::flow::tick::WORD_REJECTED;
use crate::pump::host::Ctx;
use crate::pump::store::PumpStore;

/// The repair flow node that waits for a green main rung.
pub(crate) const UNREPAIRED_NODE: &str = "unrepaired";

/// Answer `green` on every unrepaired step of a live repair run for `target`.
///
/// Returns the answered step tasks.
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
        for step in store.steps_of(&run.need, run.run)? {
            let open = matches!(step.state, StepState::Pending | StepState::Waiting);
            if step.node != UNREPAIRED_NODE || !open {
                continue;
            }
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
            }
            answered.push(step.task);
        }
    }
    Ok(answered)
}

fn records_target<M: FlowMail>(mail: &M, need: &str, target: &str) -> Result<bool, VivariumError> {
    let record = MailReader::open_record(mail, need)?;
    Ok(record.is_some_and(|(_, body)| super::record_target(&body) == Some(target)))
}

#[cfg(test)]
#[path = "green_test.rs"]
mod tests;
