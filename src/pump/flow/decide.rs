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
const REWORK_ENTRY: &str = "rework1";

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
    if a.word == FAILED && b.word == FAILED {
        return Ok(FAILED.into());
    }
    let (audit, rung) = if is_audit(&a.word) && is_rung_or_failed(&b.word) {
        (a, b)
    } else if is_audit(&b.word) && is_rung_or_failed(&a.word) {
        (b, a)
    } else if a.word == FAILED && is_rung(&b.word) {
        (a, b)
    } else if b.word == FAILED && is_rung(&a.word) {
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
    // Rows run top to bottom: a failed audit, then an auditor block (which beats a lost rung),
    // then a lost rung, then a failing rung, then a pass.
    let word = match (audit.word.as_str(), rung.word.as_str()) {
        (FAILED, _) => FAILED,
        ("block_range" | "block_ship", _) => "block",
        (_, FAILED) => FAILED,
        (_, "fail") => "judge",
        _ if audit_raise(store, &audit.task)?.is_some() => "raised",
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
    let raised = sticky_raise(store, &need, run)?.is_some();
    Ok(if raised { "raised" } else { "none" }.into())
}

fn is_audit(word: &str) -> bool {
    AUDIT_WORDS.contains(&word)
}

fn is_rung(word: &str) -> bool {
    RUNG_WORDS.contains(&word)
}

fn is_rung_or_failed(word: &str) -> bool {
    is_rung(word) || word == FAILED
}

/// The sticky raise for the run an audit task belongs to. The run comes from the step table, else
/// from the audit's own stored payload.
fn audit_raise(store: &PumpStore, task: &str) -> Result<Option<String>, VivariumError> {
    let place = match store.step_run_of(task)? {
        Some(place) => Some(place),
        None => store.step_payload(task)?.map(|row| (row.need, row.run)),
    };
    match place {
        Some((need, run)) => sticky_raise(store, &need, run),
        None => Ok(None),
    }
}

/// The rung a raise names for this run: the newest non-empty `raise_rung` among all of the run's
/// auditor payloads. A raise is sticky within a run, so a later audit without one does not clear
/// it. A run that entered at `rework1` also carries the raise of the run before it, the one the
/// CTO rework restarted.
///
/// # Errors
/// Returns [`VivariumError`] when the store cannot be read.
pub(crate) fn sticky_raise(
    store: &PumpStore,
    need: &str,
    run: u32,
) -> Result<Option<String>, VivariumError> {
    if let Some(rung) = newest_raise(store, need, run)? {
        return Ok(Some(rung));
    }
    let rework = store
        .runs_of(need)?
        .iter()
        .any(|row| row.run == run && row.entry == REWORK_ENTRY);
    match run.checked_sub(1) {
        Some(prior) if rework && prior > 0 => newest_raise(store, need, prior),
        _ => Ok(None),
    }
}

fn newest_raise(store: &PumpStore, need: &str, run: u32) -> Result<Option<String>, VivariumError> {
    Ok(store
        .step_payloads_of_run(need, run)?
        .into_iter()
        .rev()
        .filter(|row| row.class == AUDITOR_CLASS)
        .find_map(|row| rung_of(&row.payload)))
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
