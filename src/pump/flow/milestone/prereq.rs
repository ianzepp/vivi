//! Classifies each prerequisite of a release need: landed, still open (or held
//! by an operator-only state), or closed without landing.

use crate::error::VivariumError;
use crate::pump::flow::def::{FlowDef, FlowKind, NodeClass};
use crate::pump::flow::index::run_state;
use crate::pump::flow::index::{RunRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailReader, NeedStatus};
use crate::pump::flow::tick::OPERATOR_HOLD;
use crate::pump::store::PumpStore;

/// The pump record outcome of a need that landed.
const LANDED: &str = "landed";

/// Where one prerequisite stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PrereqState {
    /// Landed, or closed done with no pump record (the operator's statement).
    Landed,
    /// Not done and moving.
    Open,
    /// Not done and waiting on an operator: a lock, a park or an operator hold.
    Held(String),
    /// Closed without landing; the record outcome (`closed` when it is empty).
    ClosedUnlanded(String),
}

/// One prerequisite need of a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Prereq {
    pub handle: String,
    pub state: PrereqState,
}

/// Every prerequisite of `need`, classified.
///
/// # Errors
/// Returns [`VivariumError`] when the mailspace or the flow index cannot be read.
pub(crate) fn prerequisites<M: MailReader>(
    store: &PumpStore,
    mail: &M,
    need: &str,
) -> Result<Vec<Prereq>, VivariumError> {
    mail.need_prerequisites(need)?
        .into_iter()
        .map(|handle| {
            let state = classify(store, mail, &handle)?;
            Ok(Prereq { handle, state })
        })
        .collect()
}

fn classify<M: MailReader>(
    store: &PumpStore,
    mail: &M,
    handle: &str,
) -> Result<PrereqState, VivariumError> {
    if mail.need_state(handle)? == NeedStatus::Done {
        return Ok(match mail.last_closed_record(handle)? {
            None => PrereqState::Landed,
            Some(outcome) if outcome == LANDED => PrereqState::Landed,
            Some(outcome) if outcome.is_empty() => PrereqState::ClosedUnlanded("closed".into()),
            Some(outcome) => PrereqState::ClosedUnlanded(outcome),
        });
    }
    held(store, handle)
}

/// `Held` when the need's newest live run waits on an operator, else `Open`.
fn held(store: &PumpStore, handle: &str) -> Result<PrereqState, VivariumError> {
    let Some(run) = store
        .runs_of(handle)?
        .into_iter()
        .rev()
        .find(|run| run.state == run_state::LIVE)
    else {
        return Ok(PrereqState::Open);
    };
    let def = flow_of(&run);
    for row in store.steps_of(handle, run.run)? {
        if row.state != StepState::Waiting {
            continue;
        }
        let operator_hold = row.verdict.as_deref() == Some(OPERATOR_HOLD);
        if operator_hold || operator_only(def.as_ref(), &row.node) {
            let what = row.verdict.as_deref().unwrap_or("waiting");
            return Ok(PrereqState::Held(format!("{} {what}", row.node)));
        }
    }
    Ok(PrereqState::Open)
}

/// The run's flow definition; none when the kind has no shipped flow.
fn flow_of(run: &RunRow) -> Option<FlowDef> {
    let kind = FlowKind::parse(&run.kind)?;
    match load(kind) {
        Ok(def) => Some(def),
        // A kind with no shipped flow has no lock or park nodes to look for.
        Err(_) => None,
    }
}

fn operator_only(def: Option<&FlowDef>, node: &str) -> bool {
    def.and_then(|def| def.nodes.iter().find(|n| n.id == node))
        .is_some_and(|n| matches!(n.class, NodeClass::Lock | NodeClass::Park))
}

#[cfg(test)]
#[path = "prereq_test.rs"]
mod tests;
