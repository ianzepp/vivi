//! The pump's decision seat and the inventory of actionable waits.
//!
//! `pump.decision_to` names the seat (by role name, alias, or this
//! mailspace's address for it) that judges, raises, or steers when a flow
//! reaches its decision point. The seat is resolved read-only, when a
//! decision is made: resolution never creates or amends a role and never
//! falls back to another recipient.
//!
//! [`inventory`] is the other read half: over the live runs it lists every
//! place a run sits stopped where a named control choice can move it —
//! waiting lock steps, waiting park steps, and waiting steps held with the
//! `operator-hold` verdict — with the evidence each report was read from and
//! an explicit statement where evidence is absent.
//!
//! [`delivery`] is the write half: the `decision` tick phase posts one
//! ordinary mail reply per wait on the waiting step's own thread, addressed
//! only to the decision seat.

use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::pump::config::PumpConfig;
use crate::pump::flow::build_front::park;
use crate::pump::flow::def::{FlowDef, FlowKind, NodeClass};
use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::step_inputs;
use crate::pump::flow::tick::OPERATOR_HOLD;
use crate::pump::flow::unlock;
use crate::pump::store::PumpStore;
use crate::pump::store::step_payload::StepPayloadRow;

mod delivery;

pub(crate) use delivery::register;

/// The decision seat: the canonical role name flow decisions route to.
///
/// Resolves `pump.decision_to` against the mailspace config and requires a
/// configured role of kind `mind`. A missing, blank, unknown, or wrong-kind
/// seat fails: there is no fallback to the operator, `pump.from`, the need
/// author, or `board.reply_to`. Read-only: no role is created and no config
/// is written, so an unsettled seat never blocks unrelated flow work.
///
/// # Errors
/// Returns [`VivariumError`] when the configured seat is blank, names no
/// known role, or names a role whose kind is not `mind`.
pub fn recipient(mailspace: &Mailspace, pump: &PumpConfig) -> Result<String, VivariumError> {
    let seat = pump.decision_to.trim();
    if seat.is_empty() {
        return Err(VivariumError::Message(String::from(
            "pump.decision_to is blank; set it to a role of kind `mind`",
        )));
    }
    let name = mailspace.resolve_identity(seat)?;
    let role = mailspace
        .config
        .role_named(&name)
        .ok_or_else(|| VivariumError::Message(format!("unknown local role '{name}'")))?;
    let kind = role.kind.as_deref().unwrap_or_default();
    if !kind.eq_ignore_ascii_case("mind") {
        let shown = if kind.is_empty() { "none" } else { kind };
        return Err(VivariumError::Message(format!(
            "pump.decision_to '{seat}' is role '{name}' of kind `{shown}`, not `mind`; \
             set it to a role of kind `mind`"
        )));
    }
    Ok(name)
}

/// What kind of actionable wait a live run sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitKind {
    /// A lock step: only `vivi pump unlock` releases it, the repair flow's
    /// `unrepaired` lock included.
    Lock,
    /// A park step: a reply on the need thread or an operator answer moves it.
    Park,
    /// A waiting step held with the `operator-hold` verdict, answered with
    /// `vivi pump answer`; slice rows count whatever their node is.
    Hold,
}

impl WaitKind {
    /// The classification word used in wait identities and reports.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Lock => "lock",
            Self::Park => "park",
            Self::Hold => "hold",
        }
    }
}

/// One actionable wait: a live run stopped where a named control choice can
/// move it, with the evidence the report was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wait {
    /// Stable identity: classification, need, run, node, task and the latest
    /// attempt (`a0` when the task has no job row). A changed attempt changes
    /// the identity.
    pub id: String,
    /// The wait's classification.
    pub kind: WaitKind,
    /// The need whose run waits.
    pub need: String,
    /// The run number.
    pub run: u32,
    /// The shipped flow the run follows.
    pub flow: String,
    /// The node the run waits at.
    pub node: String,
    /// The waiting step's task handle.
    pub task: String,
    /// The latest job attempt of the task, when the store has one.
    pub job: Option<StepJobRow>,
    /// The issue kind of the newest open issue raised about the task, its
    /// latest job, or the run, when the store has one.
    pub issue: Option<String>,
    /// Why the run waits, read from the strongest evidence available;
    /// absent evidence is stated, never guessed.
    pub reason: String,
    /// Where the reason was read from; states absence where there is none.
    pub evidence: Vec<String>,
    /// The control choices permitted here.
    pub controls: Vec<String>,
}

/// One issue raised about a unit, as the exception rows keep it.
struct Issue {
    kind: String,
    unit: String,
    text: String,
}

/// Inventory every actionable wait over `store`'s live runs: waiting lock
/// steps, waiting park steps, and every waiting step held with
/// `operator-hold` — slice rows included, whether or not their node is one
/// of the shipped flow's. Read-only: nothing is written, no alarm raised.
///
/// The waits come back ordered by need, run and task, so the same live state
/// always inventories the same way. A wait keeps the newest open issue
/// raised about its task, its latest job, or its run; where no issue and no
/// other evidence exists the reason says so instead of guessing.
///
/// # Errors
/// Returns [`VivariumError`] when the runs, steps, jobs, payloads or
/// exceptions cannot be read, or a live run's shipped flow fails to load.
pub fn inventory(store: &PumpStore) -> Result<Vec<Wait>, VivariumError> {
    let exceptions = store.exceptions()?;
    let mut waits = Vec::new();
    for run in store.live_runs()? {
        collect_run(store, &flow_def(&run)?, &exceptions, &run, &mut waits)?;
    }
    waits.sort_by(|a, b| (&a.need, a.run, &a.task).cmp(&(&b.need, b.run, &b.task)));
    Ok(waits)
}

/// The shipped flow a live run follows; a run must follow a shipped flow.
fn flow_def(run: &RunRow) -> Result<FlowDef, VivariumError> {
    let kind = FlowKind::parse(&run.kind).ok_or_else(|| {
        VivariumError::Other(format!(
            "need {need} r{run}: no shipped flow of kind `{kind}`",
            need = run.need,
            run = run.run,
            kind = run.kind
        ))
    })?;
    load(kind)
}

/// Add every actionable wait of one live run.
fn collect_run(
    store: &PumpStore,
    def: &FlowDef,
    exceptions: &[(u64, String, String, String)],
    run: &RunRow,
    waits: &mut Vec<Wait>,
) -> Result<(), VivariumError> {
    for step in store.steps_of(&run.need, run.run)? {
        if step.state != StepState::Waiting {
            continue;
        }
        let Some(kind) = wait_kind(def, &step) else {
            continue;
        };
        let job = store.latest_step_job(&step.task)?;
        let issue = newest_issue(exceptions, &step.task, &run.need, run.run, job.as_ref());
        waits.push(wait_of(store, def, run, &step, kind, job, issue)?);
    }
    Ok(())
}

/// The classification of a waiting step: a hold first (a held row is
/// answerable whatever its node), then the shipped node's class.
fn wait_kind(def: &FlowDef, step: &StepRow) -> Option<WaitKind> {
    if step.verdict.as_deref() == Some(OPERATOR_HOLD) {
        return Some(WaitKind::Hold);
    }
    match class_of(def, &step.node) {
        Some(NodeClass::Lock) => Some(WaitKind::Lock),
        Some(NodeClass::Park) => Some(WaitKind::Park),
        _ => None,
    }
}

/// The class of a node of the shipped flow.
fn class_of(def: &FlowDef, node: &str) -> Option<NodeClass> {
    Some(
        def.nodes
            .iter()
            .find(|entry| entry.id == node)?
            .class
            .clone(),
    )
}

/// The newest open issue raised about `task`, else its latest job, else the
/// run; resolved rows are not open issues.
fn newest_issue(
    exceptions: &[(u64, String, String, String)],
    task: &str,
    need: &str,
    run: u32,
    job: Option<&StepJobRow>,
) -> Option<Issue> {
    let units = [
        Some(task.to_string()),
        job.map(|row| row.job.clone()),
        Some(format!("{need} r{run}")),
    ];
    units.iter().flatten().find_map(|unit| {
        exceptions
            .iter()
            .rev()
            .find(|(_, kind, row_unit, _)| kind != "resolved" && row_unit == unit)
            .map(|(_, kind, unit, text)| Issue {
                kind: kind.clone(),
                unit: unit.clone(),
                text: text.clone(),
            })
    })
}

/// Build one wait's description from its evidence.
fn wait_of(
    store: &PumpStore,
    def: &FlowDef,
    run: &RunRow,
    step: &StepRow,
    kind: WaitKind,
    job: Option<StepJobRow>,
    issue: Option<Issue>,
) -> Result<Wait, VivariumError> {
    let (reason, evidence) = report(store, def, run, step, kind, issue.as_ref())?;
    let attempt = job.as_ref().map_or(0, |row| row.attempt);
    Ok(Wait {
        id: format!(
            "{} {need} r{run} {node} {task} a{attempt}",
            kind.name(),
            need = run.need,
            run = run.run,
            node = step.node,
            task = step.task
        ),
        kind,
        need: run.need.clone(),
        run: run.run,
        flow: run.flow.clone(),
        node: step.node.clone(),
        task: step.task.clone(),
        job,
        issue: issue.map(|found| found.kind),
        reason,
        evidence,
        controls: controls(kind, &run.need, &step.task, def.kind),
    })
}

/// The wait's reason and evidence, per classification.
fn report(
    store: &PumpStore,
    def: &FlowDef,
    run: &RunRow,
    step: &StepRow,
    kind: WaitKind,
    issue: Option<&Issue>,
) -> Result<(String, Vec<String>), VivariumError> {
    match kind {
        WaitKind::Lock if def.kind == FlowKind::Repair => Ok(repair_lock_report(def, step, issue)),
        WaitKind::Lock => build_lock_report(store, run, issue),
        WaitKind::Park => park_report(def, store, run, step, issue),
        WaitKind::Hold => Ok(hold_report(issue, &step.task)),
    }
}

/// The repair lock keeps the repair standing: the raised alarm when one
/// exists, else the shipped node's own words — the CTO question the build
/// lock answers is never asked on the repair flow.
fn repair_lock_report(
    def: &FlowDef,
    step: &StepRow,
    issue: Option<&Issue>,
) -> (String, Vec<String>) {
    let mut evidence = vec![format!(
        "flow {} node {}: {}",
        def.kind.name(),
        step.node,
        node_label(def, &step.node)
    )];
    match issue {
        Some(found) => {
            evidence.push(issue_reference(found));
            (found.text.clone(), evidence)
        }
        None => (
            format!(
                "no raised alarm; the repair run waits at `{node}` for the operator to \
                 unlock it or main to go green",
                node = step.node
            ),
            evidence,
        ),
    }
}

/// The build lock's reason and evidence: what the run's newest CTO answer
/// said, as the lock alarm reads it.
fn build_lock_report(
    store: &PumpStore,
    run: &RunRow,
    issue: Option<&Issue>,
) -> Result<(String, Vec<String>), VivariumError> {
    let rows = store.step_payloads_of_run(&run.need, run.run)?;
    let mut evidence = match cto_row(&rows) {
        Some(row) => vec![format!("payload of cto step {}", row.task)],
        None => vec![format!(
            "no cto step payload in {need} r{run}",
            need = run.need,
            run = run.run
        )],
    };
    if let Some(found) = issue {
        evidence.push(issue_reference(found));
    }
    Ok((unlock::cto_why(&rows), evidence))
}

/// The run's newest CTO payload, if any.
fn cto_row(rows: &[StepPayloadRow]) -> Option<&StepPayloadRow> {
    rows.iter().rfind(|row| row.class == NodeClass::Cto.name())
}

/// The park's reason and evidence: the planner word and reason that routed
/// the run here, the same words the park alarm reads.
fn park_report(
    def: &FlowDef,
    store: &PumpStore,
    run: &RunRow,
    step: &StepRow,
    issue: Option<&Issue>,
) -> Result<(String, Vec<String>), VivariumError> {
    let mut evidence =
        match step_inputs::predecessor_step(def, store, run, &step.node, &NodeClass::Planner)? {
            Some(planner) => vec![format!(
                "planner step {} verdict {}",
                planner.task,
                planner.verdict.as_deref().unwrap_or("none")
            )],
            None => vec![String::from("no planner step found")],
        };
    if let Some(found) = issue {
        evidence.push(issue_reference(found));
    }
    Ok((park::actual_text(def, store, run, step)?, evidence))
}

/// The hold's reason and evidence: the newest open issue raised about the
/// task, its latest job or its run, or a stated absence.
fn hold_report(issue: Option<&Issue>, task: &str) -> (String, Vec<String>) {
    match issue {
        Some(found) => (found.text.clone(), vec![issue_reference(found)]),
        None => (
            format!(
                "no issue was raised about task {task}; the hold carries no recorded diagnostic"
            ),
            vec![format!("no raised issue found for task {task}")],
        ),
    }
}

/// The label of a node of the shipped flow, or a stated absence.
fn node_label(def: &FlowDef, node: &str) -> String {
    def.nodes.iter().find(|entry| entry.id == node).map_or_else(
        || format!("no shipped node `{node}`"),
        |entry| entry.label.clone(),
    )
}

/// The evidence line for one raised issue.
fn issue_reference(issue: &Issue) -> String {
    format!("raised {} on {}", issue.kind, issue.unit)
}

/// The control choices permitted at a wait.
fn controls(kind: WaitKind, need: &str, task: &str, flow: FlowKind) -> Vec<String> {
    match kind {
        WaitKind::Lock => {
            let mut out = vec![format!("vivi pump unlock {need} [entry]")];
            if flow == FlowKind::Repair {
                out.push(String::from(
                    "make main green at a tip holding the tested tip; the run releases itself",
                ));
            }
            out
        }
        WaitKind::Park => vec![
            String::from("reply on the need thread to restart the run at plan"),
            format!("vivi pump answer {task}"),
        ],
        WaitKind::Hold => vec![format!("vivi pump answer {task}")],
    }
}

#[cfg(test)]
#[path = "decision_test.rs"]
mod tests;
