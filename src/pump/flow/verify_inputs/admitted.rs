//! The particular admitted plan of a run, within continuation boundaries.
//!
//! Local admission wins. A run that entered at `slice` or `rework1` has no
//! planner of its own, so the walk follows earlier runs of the same need
//! through repeated continuations and stops at a fresh plan or research run
//! without admission. Future runs and other needs are never read.

use crate::error::VivariumError;
use crate::pump::flow::def::{FlowDef, FlowKind, NodeClass};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::step_inputs;
use crate::pump::store::PumpStore;

/// The admitter word that admits a plan.
const ADMITTED: &str = "admitted";

/// The entries that start a fresh episode: an older run does not continue
/// into the plan they carry unless they admitted one.
const FRESH_ENTRIES: [&str; 2] = ["plan", "research"];

/// The plan body of the particular admission the run continues: the stored
/// payload body, correction output included, of the completed planner that
/// fed the admitter which answered `admitted`.
///
/// `None` means no admission along the chain; a newer planner revision that
/// no admitter admitted is not sufficient. Missing or invalid referenced
/// material — an admission without its feeding planner, a planner without a
/// stored payload body — is an error, not `None`.
///
/// # Errors
/// Returns [`VivariumError`] when the store cannot be read, the run's flow
/// kind is unknown, or referenced material is missing or invalid.
pub(crate) fn admitted_plan(
    store: &PumpStore,
    run: &RunRow,
) -> Result<Option<String>, VivariumError> {
    let def = load(flow_kind(run)?)?;
    let mut current = run.clone();
    loop {
        if let Some(plan) = local_admission(store, &def, &current)? {
            return Ok(Some(plan));
        }
        if FRESH_ENTRIES.contains(&current.entry.as_str()) {
            return Ok(None);
        }
        let Some(earlier) = previous_run(store, &current)? else {
            return Ok(None);
        };
        current = earlier;
    }
}

/// The flow kind a run row names.
fn flow_kind(run: &RunRow) -> Result<FlowKind, VivariumError> {
    FlowKind::parse(&run.kind).ok_or_else(|| {
        VivariumError::Parse(format!(
            "need {} run {}: unknown flow kind {}",
            run.need, run.run, run.kind
        ))
    })
}

/// The stored plan of the run's own admission, when the run admitted one.
fn local_admission(
    store: &PumpStore,
    def: &FlowDef,
    run: &RunRow,
) -> Result<Option<String>, VivariumError> {
    let Some(admitter) = admitted_admitter(store, run, def)? else {
        return Ok(None);
    };
    let planner = step_inputs::predecessor_step(
        def,
        store,
        run,
        &admitter.node,
        &NodeClass::Planner,
    )?
    .ok_or_else(|| {
        VivariumError::Message(format!(
            "need {} run {}: admitter {} answered admitted with no completed planner feeding it",
            run.need, run.run, admitter.node
        ))
    })?;
    plan_body(store, run, &planner).map(Some)
}

/// The newest done admitter step of the run whose verdict admits a plan.
fn admitted_admitter(
    store: &PumpStore,
    run: &RunRow,
    def: &FlowDef,
) -> Result<Option<StepRow>, VivariumError> {
    Ok(store
        .steps_of(&run.need, run.run)?
        .into_iter()
        .rev()
        .find(|row| {
            row.state == StepState::Done
                && row.verdict.as_deref() == Some(ADMITTED)
                && def
                    .nodes
                    .iter()
                    .any(|node| node.id == row.node && node.class == NodeClass::Admitter)
        }))
}

/// The newest run of the need below `run`, or `None` when the chain ends.
fn previous_run(store: &PumpStore, run: &RunRow) -> Result<Option<RunRow>, VivariumError> {
    Ok(store
        .runs_of(&run.need)?
        .into_iter()
        .rev()
        .find(|row| row.run < run.run))
}

/// The stored payload body of a planner step, correction output included.
fn plan_body(store: &PumpStore, run: &RunRow, planner: &StepRow) -> Result<String, VivariumError> {
    let row = store.step_payload(&planner.task)?.ok_or_else(|| {
        VivariumError::Message(format!(
            "need {} run {}: planner step {} (task {}) has no stored payload",
            run.need, run.run, planner.node, planner.task
        ))
    })?;
    row.payload["body"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| {
            VivariumError::Message(format!(
                "need {} run {}: planner step {} (task {}) has no stored plan body",
                run.need, run.run, planner.node, planner.task
            ))
        })
}

#[cfg(test)]
#[path = "admitted_test.rs"]
mod tests;
