//! Continuation of conversational pauses: the shared origin lookup of a park
//! and the eligibility of a held step.
//!
//! Every continuation side asks the same two questions, so the answers live
//! here once:
//!
//! * Where does a run's park come from? [`park_origin`] resolves a run's live
//!   park to the exact step that parked. The durable park-origin record wins
//!   when one names a candidate; a historical park without one is accepted
//!   only when the run's completed steps, the flow's decision-edge words and
//!   the stored outcomes identify one origin unambiguously — never the newest
//!   planner or plan. An ambiguous or missing historical origin is a
//!   diagnostic, and the park stays.
//! * May a reply continue this held step? [`hold_block`] names the first
//!   reason a `Waiting(OPERATOR_HOLD)` step cannot be continued: scope waits,
//!   milestones, locks, uncertain-effect holds, command jobs and effectful
//!   requests all stay with the operator. [`eligible_hold`] turns an eligible
//!   task into its exact step and its original role request, never an
//!   envelope-fix correction.
//!
//! Ownership only: nothing here reopens a park, retires a gate or enqueues a
//! replacement request; the lifecycle verbs stay with the store and the later
//! continuation collector.

use std::collections::BTreeSet;

use crate::error::VivariumError;
use crate::pump::envelope::correct;
use crate::pump::envelope::roles::RoleKind;
use crate::pump::flow::def::{FlowDef, FlowKind, NodeClass};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::scope::ScopeState;
use crate::pump::flow::tick::{OPERATOR_HOLD, SIDE_EFFECT_UNCLEAR};
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::PumpStore;
use crate::pump::store::recovery::pause::{PauseBoundaryRow, pause_state};

// ---- the origin of a park -------------------------------------------------

/// How a park's origin was established.
#[derive(Debug, Clone, PartialEq)]
pub enum OriginSource {
    /// The durable park-origin record named the origin, and here it is.
    Recorded(Box<PauseBoundaryRow>),
    /// The run index, the flow's decision-edge words and the stored outcomes
    /// named the origin; no record exists.
    Legacy,
}

/// The exact origin of a run's park: the step that parked and the decision
/// gate its park word traveled through.
#[derive(Debug, Clone, PartialEq)]
pub struct PauseOrigin {
    /// The need whose flow run parked.
    pub need: String,
    /// The flow run that parked.
    pub run: u32,
    /// The node of the step that parked.
    pub node: String,
    /// The task of the step that parked.
    pub origin_task: String,
    /// The decision gate whose park-word edge carried the run into the park.
    pub gate: String,
    /// How the origin was established.
    pub source: OriginSource,
}

impl PauseOrigin {
    /// The durable park-origin record behind an
    /// [`OriginSource::Recorded`] origin; `None` for a legacy one.
    #[must_use]
    pub fn record(&self) -> Option<&PauseBoundaryRow> {
        match &self.source {
            OriginSource::Recorded(row) => Some(row),
            OriginSource::Legacy => None,
        }
    }
}

/// The resolution of a run's live park.
#[derive(Debug, Clone, PartialEq)]
pub enum ParkResolution {
    /// The run is not parked: no `Waiting` row sits at a Park node.
    NotParked,
    /// The exact origin of the park.
    Origin(PauseOrigin),
    /// The park stays: its origin is missing or ambiguous, and the text is
    /// the diagnostic for the park report or the exception.
    Parked(String),
}

/// The run's live park rows: `Waiting` rows at Park-class nodes.
#[must_use]
pub fn park_steps<'a>(def: &FlowDef, steps: &'a [StepRow]) -> Vec<&'a StepRow> {
    let parks: BTreeSet<&str> = def
        .nodes
        .iter()
        .filter(|node| node.class == NodeClass::Park)
        .map(|node| node.id.as_str())
        .collect();
    steps
        .iter()
        .filter(|row| row.state == StepState::Waiting && parks.contains(row.node.as_str()))
        .collect()
}

/// Resolve a run's live park to its exact originating step.
///
/// # Errors
/// Returns [`VivariumError`] when a park-origin record cannot be read.
pub fn park_origin(
    store: &PumpStore,
    def: &FlowDef,
    run: &RunRow,
    steps: &[StepRow],
) -> Result<ParkResolution, VivariumError> {
    let parks = park_steps(def, steps);
    let Some(park) = parks.first() else {
        return Ok(ParkResolution::NotParked);
    };
    let where_at = format!(
        "need {} r{} parks at `{}` (task {})",
        run.need, run.run, park.node, park.task
    );
    let candidates = candidates_at(def, &park.node, steps);
    if let Some(resolved) = recorded_origin(store, run, &where_at, &candidates)? {
        return Ok(resolved);
    }
    Ok(legacy_origin(run, &where_at, &candidates))
}

/// [`park_origin`] for a stored run: loads the run's flow and steps.
///
/// # Errors
/// Returns [`VivariumError`] when the run's flow or steps cannot be read.
pub fn run_park(store: &PumpStore, run: &RunRow) -> Result<ParkResolution, VivariumError> {
    let def = run_def(&run.need, run.run, &run.kind)?;
    let steps = store.steps_of(&run.need, run.run)?;
    park_origin(store, &def, run, &steps)
}

/// The origin a durable park-origin record names, when exactly one candidate
/// carries an open record; several open records are an ambiguity of their
/// own. `Ok(None)` when no candidate carries one and the legacy evidence
/// decides.
fn recorded_origin(
    store: &PumpStore,
    run: &RunRow,
    where_at: &str,
    candidates: &[Candidate],
) -> Result<Option<ParkResolution>, VivariumError> {
    let mut open: Vec<(PauseBoundaryRow, &Candidate)> = Vec::new();
    for candidate in candidates {
        let Some(boundary) = store.latest_pause_boundary(
            &run.need,
            run.run,
            &candidate.row.node,
            &candidate.row.task,
        )?
        else {
            continue;
        };
        if boundary.state == pause_state::OPEN {
            open.push((boundary, candidate));
        }
    }
    if open.len() > 1 {
        let names = open
            .iter()
            .map(|(boundary, _)| format!("{} ({})", boundary.node, boundary.origin_task))
            .collect::<Vec<_>>()
            .join(" and ");
        return Ok(Some(ParkResolution::Parked(format!(
            "{where_at}: open park-origin records name {names}; the origin stays unresolved"
        ))));
    }
    if let Some((boundary, _)) = open.first() {
        return Ok(Some(ParkResolution::Origin(PauseOrigin {
            need: run.need.clone(),
            run: run.run,
            node: boundary.node.clone(),
            origin_task: boundary.origin_task.clone(),
            gate: boundary.gate.clone(),
            source: OriginSource::Recorded(Box::new(boundary.clone())),
        })));
    }
    Ok(None)
}

/// The origin the run's own evidence names, when it is unambiguous.
fn legacy_origin(run: &RunRow, where_at: &str, candidates: &[Candidate]) -> ParkResolution {
    if let [only] = candidates {
        return ParkResolution::Origin(PauseOrigin {
            need: run.need.clone(),
            run: run.run,
            node: only.row.node.clone(),
            origin_task: only.row.task.clone(),
            gate: only.gate.clone(),
            source: OriginSource::Legacy,
        });
    }
    ParkResolution::Parked(match candidates {
        [] => format!(
            "{where_at}: no completed step of the run took a park word into it; \
             the origin stays unresolved"
        ),
        more => format!(
            "{where_at}: completed steps {} all match the park words; \
             the origin stays unresolved",
            candidate_names(more)
        ),
    })
}

/// A completed step whose verdict a decision edge carried into the park: one
/// candidate origin of the park.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    row: StepRow,
    gate: String,
}

/// Every candidate origin of a park at `park`: completed steps of `steps`
/// whose verdict reaches the park through decision edges that all take it,
/// with the gate the verdict entered the park through. The same word travels
/// every hop (`advance::route`), so the verdict must sit on every edge
/// between the step and the park.
fn candidates_at(def: &FlowDef, park: &str, steps: &[StepRow]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for edge in def.edges.iter().filter(|edge| edge.to == park) {
        if node_class(def, &edge.from) != Some(&NodeClass::Decision) {
            continue;
        }
        for row in steps.iter().filter(|row| row.state == StepState::Done) {
            let Some(verdict) = row.verdict.as_deref() else {
                continue;
            };
            if takes(&edge.words, verdict) && word_reaches(def, &row.node, verdict, &edge.from) {
                out.push(Candidate {
                    row: row.clone(),
                    gate: edge.from.clone(),
                });
            }
        }
    }
    out.sort_by(|a, b| {
        (&a.row.node, &a.row.task, &a.gate).cmp(&(&b.row.node, &b.row.task, &b.gate))
    });
    out.dedup();
    out
}

/// The candidates as `node (task)` names for a diagnostic.
fn candidate_names(candidates: &[Candidate]) -> String {
    candidates
        .iter()
        .map(|candidate| format!("{} ({})", candidate.row.node, candidate.row.task))
        .collect::<Vec<_>>()
        .join(" and ")
}

/// Whether `word` travels from `from` into `gate` along edges that all take
/// it: `from` feeds the gate directly, or feeds a decision that does,
/// recursively.
fn word_reaches(def: &FlowDef, from: &str, word: &str, gate: &str) -> bool {
    let mut pending = vec![gate];
    let mut seen: BTreeSet<String> = BTreeSet::new();
    while let Some(id) = pending.pop() {
        for edge in def
            .edges
            .iter()
            .filter(|edge| edge.to == id && takes(&edge.words, word))
        {
            if edge.from == from {
                return true;
            }
            if node_class(def, &edge.from) == Some(&NodeClass::Decision)
                && seen.insert(edge.from.clone())
            {
                pending.push(edge.from.as_str());
            }
        }
    }
    false
}

/// Whether an edge takes `word`: an unlabelled edge takes any word, a
/// labelled one only its words (the rule `advance::route` walks with).
fn takes(words: &[String], word: &str) -> bool {
    words.is_empty() || words.iter().any(|candidate| candidate == word)
}

fn node_class<'d>(def: &'d FlowDef, id: &str) -> Option<&'d NodeClass> {
    def.nodes.iter().find(|node| node.id == id).map(|node| &node.class)
}

/// The flow definition of one run's kind.
fn run_def(subject: &str, run: u32, kind: &str) -> Result<FlowDef, VivariumError> {
    let parsed = FlowKind::parse(kind).ok_or_else(|| {
        VivariumError::Other(format!(
            "{subject} r{run}: expected a known flow kind; actual: {kind}"
        ))
    })?;
    load(parsed)
}

// ---- the eligibility of a held step ---------------------------------------

/// Why a held step cannot be continued from a reply on its thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldBlock {
    /// The step is not `Waiting`.
    NotWaiting,
    /// The step waits on something other than the operator.
    NotOperatorHold,
    /// The step's node is no role step (a rung, a slice row, a code job).
    NotRole,
    /// The step waits at a milestone.
    Milestone,
    /// The step waits at a lock.
    Lock,
    /// The step waits on blocked-scope resolution, not the operator.
    ScopeWait,
    /// The step's newest exception is an uncertain-effect hold: the effect of
    /// its job may or may not have landed, so only the operator decides.
    UncertainEffect,
    /// No durable role request backs the step.
    Unbacked,
    /// The backing request runs a shell command, not a role.
    Command,
    /// The original role request may write.
    Effectful,
}

impl HoldBlock {
    /// The block's diagnostic word.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::NotWaiting => "not-waiting",
            Self::NotOperatorHold => "not-operator-hold",
            Self::NotRole => "not-role",
            Self::Milestone => "milestone",
            Self::Lock => "lock",
            Self::ScopeWait => "scope-wait",
            Self::UncertainEffect => "uncertain-effect",
            Self::Unbacked => "unbacked",
            Self::Command => "command",
            Self::Effectful => "effectful",
        }
    }
}

/// A held step eligible for continuation: its exact step and the original
/// role request a continuation would replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EligibleHold {
    /// The need whose flow run holds the step.
    pub need: String,
    /// The flow run that holds the step.
    pub run: u32,
    /// The flow node of the held step.
    pub node: String,
    /// The held step task.
    pub task: String,
    /// The original role request (the step's attempt-1 job), never an
    /// envelope-fix correction.
    pub request: PoolJob,
}

/// The first reason `step` cannot be continued from a reply, or `None` when
/// it is an eligible held role step.
///
/// # Errors
/// Returns [`VivariumError`] when the store cannot be read.
pub fn hold_block(
    store: &PumpStore,
    st: &impl JobStore,
    def: &FlowDef,
    step: &StepRow,
) -> Result<Option<HoldBlock>, VivariumError> {
    Ok(classify(store, st, def, step)?.0)
}

/// The held step `task` as a continuation target: its exact step and its
/// original role request. `None` when the task is unknown, not indexed as a
/// step, or excluded — see [`hold_block`] for the reason.
///
/// # Errors
/// Returns [`VivariumError`] when the store cannot be read or the run's flow
/// cannot load.
pub fn eligible_hold(
    store: &PumpStore,
    st: &impl JobStore,
    task: &str,
) -> Result<Option<EligibleHold>, VivariumError> {
    let Some(step) = store.step_by_task(task)? else {
        return Ok(None);
    };
    let Some((need, run)) = store.step_run_of(task)? else {
        return Ok(None);
    };
    let row = store.run_row(&need, run)?;
    let def = run_def(&need, run, &row.kind)?;
    let Some(request) = classify(store, st, &def, &step)?.1 else {
        return Ok(None);
    };
    Ok(Some(EligibleHold {
        need,
        run,
        node: step.node,
        task: task.to_owned(),
        request,
    }))
}

/// Classify one step: the first reason it cannot be continued, or its
/// original role request when it can.
fn classify(
    store: &PumpStore,
    st: &impl JobStore,
    def: &FlowDef,
    step: &StepRow,
) -> Result<(Option<HoldBlock>, Option<PoolJob>), VivariumError> {
    let block = |why: HoldBlock| (Some(why), None::<PoolJob>);
    if step.state != StepState::Waiting {
        return Ok(block(HoldBlock::NotWaiting));
    }
    if step.verdict.as_deref() != Some(OPERATOR_HOLD) {
        return Ok(block(HoldBlock::NotOperatorHold));
    }
    match node_class(def, &step.node) {
        Some(NodeClass::Milestone) => return Ok(block(HoldBlock::Milestone)),
        Some(NodeClass::Lock) => return Ok(block(HoldBlock::Lock)),
        class if class.is_none_or(|class| RoleKind::from_class(class).is_none()) => {
            return Ok(block(HoldBlock::NotRole));
        }
        _ => {}
    }
    if matches!(
        store.scope_state(&step.task)?,
        Some(ScopeState::Wait | ScopeState::Rework)
    ) {
        return Ok(block(HoldBlock::ScopeWait));
    }
    if uncertain_effect(store, &step.task)? {
        return Ok(block(HoldBlock::UncertainEffect));
    }
    let Some(request) = original_request(store, st, &step.task)? else {
        return Ok(block(HoldBlock::Unbacked));
    };
    if request.command.is_some() {
        return Ok(block(HoldBlock::Command));
    }
    if !request.read_only {
        return Ok(block(HoldBlock::Effectful));
    }
    Ok((None, Some(request)))
}

/// Whether the step's newest exception is an uncertain-effect hold. A newer
/// exception of another kind supersedes it, and another unit's exceptions
/// never count.
fn uncertain_effect(store: &PumpStore, task: &str) -> Result<bool, VivariumError> {
    Ok(store
        .exceptions()?
        .into_iter()
        .rev()
        .find(|(_, _, unit, _)| unit == task)
        .is_some_and(|(_, kind, _, _)| kind == SIDE_EFFECT_UNCLEAR))
}

/// The original role request of step `task`: its attempt-1 job, identified by
/// the `step-<task>-a1` id the role handlers mint, or the lowest-attempt
/// non-correction job the index records for the step. An envelope-fix
/// correction is never it.
fn original_request<S: JobStore>(
    store: &PumpStore,
    st: &S,
    task: &str,
) -> Result<Option<PoolJob>, VivariumError> {
    if let Some(job) = st.jobs().get(&format!("step-{task}-a1")) {
        return Ok(Some(job.clone()));
    }
    let mut best: Option<(u32, PoolJob)> = None;
    for job in st.jobs().values() {
        let Some(index_row) = store.step_job(&job.id)? else {
            continue;
        };
        if index_row.task != task || job.kind == correct::KIND {
            continue;
        }
        if best.as_ref().is_none_or(|(attempt, _)| index_row.attempt < *attempt) {
            best = Some((index_row.attempt, job.clone()));
        }
    }
    Ok(best.map(|(_, job)| job))
}
