//! Conversational pauses: the exact step a pause belongs to, and which held
//! steps a continuation may serve.
//!
//! A conversational pause is a step parked while someone answers it: the
//! planner word that routed a run to the park node, or a role step held at
//! `operator-hold` for `vivi pump answer`. Routing, park diagnostics and the
//! later thread continuation all ask the same two questions — which step does
//! this pause belong to, and may this held step be served — so the lookups
//! live here once.
//!
//! The durable park-origin record (`store::recovery::pause`) wins whenever
//! one exists. A historical park recorded before that table answers only
//! when the run's step index, the flow's decision-edge words and the stored
//! completed outcomes identify exactly one originating step; the newest
//! planner or plan is never assumed. An ambiguous or missing historical
//! origin is a diagnostic, and the park stays parked.
//!
//! Staged like `flow::recovery`: the routing, diagnostics and continuation
//! collectors that consume these helpers land in later slices.
#![cfg_attr(not(test), allow(dead_code))]

use crate::error::VivariumError;
use crate::pump::envelope::roles::RoleKind;
use crate::pump::flow::def::{FlowDef, NodeClass};
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::scope::ScopeState;
use crate::pump::flow::tick::{OPERATOR_HOLD, SIDE_EFFECT_UNCLEAR};
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::PumpStore;

// ---- the origin of a pause -------------------------------------------------

/// How a pause's origin was identified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginSource {
    /// The durable park-origin record named it.
    Recorded,
    /// The run index, the flow's decision edges and the stored completed
    /// outcomes identified it.
    Inferred,
}

/// The exact step a conversational pause belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginStep {
    /// The originating step.
    pub step: StepRow,
    /// How it was identified.
    pub source: OriginSource,
}

/// The answer of [`origin_of`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// The pause has exactly one originating step.
    Found(OriginStep),
    /// No unambiguous origin: the diagnostic to raise, and the park stays.
    Parked(String),
}

/// The step the pause at `gate` belongs to.
///
/// A pause boundary is recorded on its origin's own coordinates, so there
/// are two discovery routes. A pause recorded for the gate itself names the
/// origin verbatim — a held role step is its own origin. Otherwise the only
/// accepted evidence is completed steps of the run that reach the gate
/// through a decision node on the step's own answer word; when exactly one
/// candidate carries its own recorded pause, the record wins over the
/// topology. Zero candidates, several, or several recorded ones are a
/// diagnostic, never a guess at the newest planner or plan.
///
/// # Errors
/// Returns [`VivariumError`] when the store cannot be read.
pub fn origin_of(
    store: &PumpStore,
    def: &FlowDef,
    need: &str,
    gate: &StepRow,
) -> Result<Origin, VivariumError> {
    if let Some(pause) = store.latest_pause_boundary(need, gate.run, &gate.node, &gate.task)? {
        return recorded_origin(store, &pause.origin_task, gate);
    }
    let routed: Vec<StepRow> = store
        .steps_of(need, gate.run)?
        .into_iter()
        .filter(|step| {
            step.state == StepState::Done
                && routes_through_decision(def, &step.node, &gate.node, step.verdict.as_deref())
        })
        .collect();
    let recorded = recorded_candidates(store, need, &routed)?;
    match (recorded.as_slice(), routed.as_slice()) {
        ([one], _) => Ok(Origin::Found(OriginStep {
            step: one.clone(),
            source: OriginSource::Recorded,
        })),
        ([], [one]) => Ok(Origin::Found(OriginStep {
            step: one.clone(),
            source: OriginSource::Inferred,
        })),
        ([], []) => Ok(Origin::Parked(format!(
            "no completed step routed {} through a decision on its answer; the park stays",
            gate.node
        ))),
        ([..], many) => Ok(Origin::Parked(format!(
            "{} both routed {} through a decision; name the origin, the park stays",
            names(many),
            gate.node
        ))),
    }
}

/// The candidates whose own pause is recorded.
fn recorded_candidates(
    store: &PumpStore,
    need: &str,
    candidates: &[StepRow],
) -> Result<Vec<StepRow>, VivariumError> {
    let mut recorded = Vec::new();
    for candidate in candidates {
        if store
            .latest_pause_boundary(need, candidate.run, &candidate.node, &candidate.task)?
            .is_some()
        {
            recorded.push(candidate.clone());
        }
    }
    Ok(recorded)
}

/// The origin the recorded pause names, or a diagnostic when its step row is
/// gone.
fn recorded_origin(
    store: &PumpStore,
    origin_task: &str,
    gate: &StepRow,
) -> Result<Origin, VivariumError> {
    match store.step_by_task(origin_task)? {
        Some(step) => Ok(Origin::Found(OriginStep {
            step,
            source: OriginSource::Recorded,
        })),
        None => Ok(Origin::Parked(format!(
            "pause of {} at {} names origin {origin_task:?}, which has no step row; \
             the park stays",
            gate.task, gate.node
        ))),
    }
}

/// The task handles of `steps`, for diagnostics.
fn names(steps: &[StepRow]) -> String {
    steps
        .iter()
        .map(|step| step.task.as_str())
        .collect::<Vec<_>>()
        .join(" and ")
}

/// Whether a step of `from` reaches `gate` through one decision node: the
/// verdict must be one of the decision's words to the gate, and one of the
/// step's own words to the decision when that edge is labeled. A word the
/// shipped flow does not draw routes nothing.
fn routes_through_decision(def: &FlowDef, from: &str, gate: &str, verdict: Option<&str>) -> bool {
    let Some(verdict) = verdict else {
        return false;
    };
    let class_of = |id: &str| {
        def.nodes
            .iter()
            .find(|node| node.id == id)
            .map(|node| &node.class)
    };
    def.edges.iter().any(|into_gate| {
        into_gate.to == gate
            && class_of(&into_gate.from) == Some(&NodeClass::Decision)
            && into_gate.words.iter().any(|word| word == verdict)
            && def.edges.iter().any(|into_decision| {
                into_decision.to == into_gate.from
                    && into_decision.from == from
                    && (into_decision.words.is_empty()
                        || into_decision.words.iter().any(|word| word == verdict))
            })
    })
}

// ---- the holds a continuation may serve ------------------------------------

/// The original role request of a step task: `step-<task>-a1`, the id the
/// step's first dispatch enqueued. Transient retries (`-tr<n>`) and envelope
/// corrections (`-c<n>`) hang off it and never replace it.
#[must_use]
pub fn step_request(task: &str) -> String {
    format!("step-{task}-a1")
}

/// The original role request a continuation of `step` may serve, or `None`
/// when `step` is not a servable conversational hold.
///
/// A servable hold is a role step parked at [`OPERATOR_HOLD`] whose original
/// request ([`step_request`]) was a read-only model dispatch. Everything
/// else stays out: a command job has no conversation to continue; a scope
/// wait, a milestone or a lock is pump bookkeeping, not an open question; a
/// hold raised because a side effect could not be settled waits for
/// evidence, not for an answer; and an effectful request must never be
/// re-sent by a continuation.
///
/// # Errors
/// Returns [`VivariumError`] when the store cannot be read.
pub fn held_request<S: JobStore>(
    store: &PumpStore,
    def: &FlowDef,
    jobs: &S,
    step: &StepRow,
) -> Result<Option<String>, VivariumError> {
    if step.state != StepState::Waiting || step.verdict.as_deref() != Some(OPERATOR_HOLD) {
        return Ok(None);
    }
    let Some(node) = def.nodes.iter().find(|node| node.id == step.node) else {
        return Ok(None);
    };
    if RoleKind::from_class(&node.class).is_none() {
        return Ok(None);
    }
    if store.scope_state(&step.task)? == Some(ScopeState::Wait) {
        return Ok(None);
    }
    if unclear_effect(store, &step.task)? {
        return Ok(None);
    }
    let Some(job) = jobs.jobs().get(&step_request(&step.task)) else {
        return Ok(None);
    };
    servable_request(job, &step.task)
}

/// Whether the original request is a read-only model dispatch: never a
/// command, never a publication or a deletion.
fn servable_request(job: &PoolJob, task: &str) -> Result<Option<String>, VivariumError> {
    if job.command.is_some() || job.publish_ref.is_some() || job.delete_ref.is_some() {
        return Ok(None);
    }
    if !job.read_only {
        return Ok(None);
    }
    Ok(Some(step_request(task)))
}

/// Whether an unresolved side-effect exception is standing against `task`:
/// such a hold waits for evidence, not for an answer.
fn unclear_effect(store: &PumpStore, task: &str) -> Result<bool, VivariumError> {
    Ok(store
        .exceptions()?
        .iter()
        .any(|(_, kind, unit, _)| kind == SIDE_EFFECT_UNCLEAR && unit == task))
}

#[cfg(test)]
#[path = "continuation_test.rs"]
mod tests;
