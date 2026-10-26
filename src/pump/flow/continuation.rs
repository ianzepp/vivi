//! Park continuation origins: which executable task a park continued from.
//!
//! The resolver answers one question for a park step: which completed
//! executable task's actual verdict routed the run here? The park alarm and
//! the thread-change phase both ask it. A recorded continuation link wins;
//! park tasks written before the link existed are resolved from the run's
//! completed steps and the flow topology. Only a unique answer is a result:
//! missing or ambiguous evidence is an explicit error, never a fallback to
//! a planner class or the newest task.

use crate::error::VivariumError;
use crate::pump::flow::def::{FlowDef, FlowEdge, NodeClass};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::store::{ContinuationLink, PumpStore};

/// The origin of park step `park`: the executable task whose verdict routed
/// the run here.
///
/// A recorded continuation link wins when it belongs to this need and run
/// and names a step on an executable node of the flow. Without a link, the
/// run's completed executable steps are checked against the topology: a step
/// counts when its recorded verdict can follow the edges from its node to
/// the park node through decision nodes. Exactly one such step is the
/// origin; none or several are errors.
///
/// # Errors
/// Returns [`VivariumError`] when the store cannot be read, a recorded link
/// is invalid (another need or run, or a non-executable source), or the
/// topology evidence is missing or ambiguous.
pub fn origin(
    def: &FlowDef,
    store: &PumpStore,
    run: &RunRow,
    park: &StepRow,
) -> Result<String, VivariumError> {
    if let Some(link) = store.continuation_of(&park.task)? {
        return linked_origin(def, store, run, &link);
    }
    historical_origin(def, store, run, park)
}

/// Resolve through a recorded link: it must belong to this need and run and
/// name a step on an executable node of the flow.
fn linked_origin(
    def: &FlowDef,
    store: &PumpStore,
    run: &RunRow,
    link: &ContinuationLink,
) -> Result<String, VivariumError> {
    if link.need != run.need || link.run != run.run {
        return Err(VivariumError::Other(format!(
            "park task {}: the recorded continuation names need {} r{}, not need {} r{}",
            link.task, link.need, link.run, run.need, run.run
        )));
    }
    let Some(step) = store.step_by_task(&link.source)? else {
        return Err(VivariumError::Other(format!(
            "park task {}: the recorded continuation names source {}, which is not a step",
            link.task, link.source
        )));
    };
    if !class_of(def, &step.node).is_some_and(NodeClass::is_step) {
        return Err(VivariumError::Other(format!(
            "park task {}: the recorded continuation names source {} on node {}, \
             which is not an executable node of the {} flow",
            link.task,
            link.source,
            step.node,
            def.kind.name()
        )));
    }
    Ok(link.source.clone())
}

/// Resolve from the run's completed executable steps and the topology.
fn historical_origin(
    def: &FlowDef,
    store: &PumpStore,
    run: &RunRow,
    park: &StepRow,
) -> Result<String, VivariumError> {
    let mut found: Vec<String> = Vec::new();
    for step in store.steps_of(&run.need, run.run)? {
        let Some(verdict) = step.verdict.as_deref() else {
            continue;
        };
        if step.state != StepState::Done {
            continue;
        }
        if !class_of(def, &step.node).is_some_and(NodeClass::is_step) {
            continue;
        }
        if routes_to(def, &step.node, verdict, &park.node) {
            found.push(step.task);
        }
    }
    match found.as_slice() {
        [task] => Ok(task.clone()),
        [] => Err(VivariumError::Other(format!(
            "park task {} (node {}): no completed executable step of need {} r{} \
             has a verdict that routes to it",
            park.task, park.node, run.need, run.run
        ))),
        tasks => Err(VivariumError::Other(format!(
            "park task {} (node {}): the completed steps {} all route to it; \
             the origin is ambiguous",
            park.task,
            park.node,
            tasks.join(", ")
        ))),
    }
}

/// Whether a verdict recorded at executable node `from` can follow the edges
/// to `target`. The path may pass only through decision nodes, and an edge
/// with answer words only passes when the verdict is one of them.
fn routes_to(def: &FlowDef, from: &str, verdict: &str, target: &str) -> bool {
    let mut seen: Vec<String> = vec![from.to_string()];
    follow(def, from, verdict, target, &mut seen)
}

/// One depth-first step of [`routes_to`], with `seen` breaking cycles.
fn follow(def: &FlowDef, node: &str, verdict: &str, target: &str, seen: &mut Vec<String>) -> bool {
    for edge in def.edges.iter().filter(|edge| edge.from == node) {
        if !edge_takes(edge, verdict) {
            continue;
        }
        if edge.to == target {
            return true;
        }
        let decision = class_of(def, &edge.to).is_some_and(|class| *class == NodeClass::Decision);
        if decision && !seen.iter().any(|id| id == &edge.to) {
            seen.push(edge.to.clone());
            if follow(def, &edge.to, verdict, target, seen) {
                return true;
            }
        }
    }
    false
}

/// Whether `edge` passes `verdict`: yes when it has no answer words.
fn edge_takes(edge: &FlowEdge, verdict: &str) -> bool {
    edge.words.is_empty() || edge.words.iter().any(|word| word == verdict)
}

/// The class of flow node `id`, when the node exists.
fn class_of<'a>(def: &'a FlowDef, id: &str) -> Option<&'a NodeClass> {
    def.nodes
        .iter()
        .find(|node| node.id == id)
        .map(|node| &node.class)
}

#[cfg(test)]
#[path = "continuation/origin_test.rs"]
mod tests;
