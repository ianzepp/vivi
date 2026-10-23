//! The join table and the raise check for decision nodes, registered in the registry's decide slot.
//!
//! A `join` node waits for one audit and one rung step and answers from their words. A
//! `raisecheck` node follows a judge and says whether this pass's audit asked for a higher rung.
//! Any other class, a missing input or an unknown word is an error that names the node: a word is
//! never guessed.

use serde_json::Value;

use crate::error::VivariumError;
use crate::pump::flow::advance::route::JoinInput;
use crate::pump::flow::def::{FlowNode, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::jobs::JobStore;
use crate::pump::store::PumpStore;

const AUDIT_WORDS: [&str; 4] = ["clean_pass", "residual", "block_range", "block_ship"];
const RUNG_WORDS: [&str; 2] = ["pass", "fail"];
const FAILED: &str = "failed";
const AUDITOR_CLASS: &str = "auditor";

fn bad(node: &FlowNode, why: impl std::fmt::Display) -> VivariumError {
    VivariumError::Parse(format!("decision node `{}`: {why}", node.id))
}

/// The answer word of a code decision node.
///
/// # Errors
/// Returns [`VivariumError`] for a class without a rule, a wrong input count, an unknown word, or
/// an unreadable store.
pub(crate) fn decide(
    store: &PumpStore,
    node: &FlowNode,
    inputs: &[JoinInput],
) -> Result<String, VivariumError> {
    match node.class {
        NodeClass::Join => join(store, node, inputs),
        NodeClass::Raisecheck => raisecheck(store, node, inputs),
        _ => Err(bad(
            node,
            format!("class `{}` has no decision rule", node.class.name()),
        )),
    }
}

fn join(store: &PumpStore, node: &FlowNode, inputs: &[JoinInput]) -> Result<String, VivariumError> {
    let [a, b] = inputs else {
        return Err(bad(
            node,
            format!("a join needs two inputs, got {}", inputs.len()),
        ));
    };
    if a.word == FAILED || b.word == FAILED {
        return Ok(FAILED.into());
    }
    let (audit, rung) = if is_audit(&a.word) && is_rung(&b.word) {
        (a, b)
    } else if is_audit(&b.word) && is_rung(&a.word) {
        (b, a)
    } else {
        return Err(bad(
            node,
            format!(
                "inputs `{}` and `{}` are not one audit word and one rung word",
                a.word, b.word
            ),
        ));
    };
    let word = match (audit.word.as_str(), rung.word.as_str()) {
        ("block_range" | "block_ship", _) => "block",
        (_, "fail") => "judge",
        _ if raise_rung(store, &audit.task)?.is_some() => "raised",
        _ => "verified",
    };
    Ok(word.into())
}

fn raisecheck(
    store: &PumpStore,
    node: &FlowNode,
    inputs: &[JoinInput],
) -> Result<String, VivariumError> {
    let [input] = inputs else {
        return Err(bad(
            node,
            format!("a raise check needs one input, got {}", inputs.len()),
        ));
    };
    let Some((need, run)) = store.step_run_of(&input.task)? else {
        return Err(bad(node, format!("unknown step task `{}`", input.task)));
    };
    let audit = store
        .step_payloads_of_run(&need, run)?
        .into_iter()
        .rev()
        .find(|row| row.class == AUDITOR_CLASS);
    let raised = audit.is_some_and(|row| rung_of(&row.payload).is_some());
    Ok(if raised { "raised" } else { "none" }.into())
}

fn is_audit(word: &str) -> bool {
    AUDIT_WORDS.contains(&word)
}

fn is_rung(word: &str) -> bool {
    RUNG_WORDS.contains(&word)
}

fn raise_rung(store: &PumpStore, task: &str) -> Result<Option<String>, VivariumError> {
    Ok(store
        .step_payload(task)?
        .and_then(|row| rung_of(&row.payload)))
}

fn rung_of(payload: &Value) -> Option<String> {
    payload["front"]["raise_rung"]
        .as_str()
        .filter(|rung| !rung.is_empty())
        .map(str::to_owned)
}

pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.set_decide(decide);
}

#[cfg(test)]
#[path = "decide_test.rs"]
mod tests;
