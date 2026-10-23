//! Starting a flow run is a flow-node transition, so `advance` owns it: the
//! kind is fixed once, the sentinel goes first, then the entry step, both
//! bound in one letter.

use crate::error::VivariumError;
use crate::pump::flow::def::{FlowDef, FlowKind, SENTINEL_MARKER, STEP_MARKER};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::kind;
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailReader, MailWriter};
use crate::pump::host::Ctx;
use crate::pump::store::PumpStore;

/// The step-row node name of a run's sentinel task.
pub const SENTINEL_NODE: &str = "sentinel";

/// The run number at which the alarm fires (an alarm, not a limit).
const ALARM_RUN: u32 = 4;

/// The run state of a run still being advanced: the value of the store's
/// `RUN_LIVE`, whose module is private to the store.
const RUN_LIVE_STATE: &str = "live";

/// The run state of a run whose tasks are still being sent: [`resume`]
/// finishes it.
pub const RUN_STARTING: &str = "starting";

/// The flow version recorded in a run row.
const FLOW_VERSION_NUMBER: u32 = 2;

/// Start a new run on `need` at `entry` (the flow's first entry when `None`).
///
/// Idempotent step by step. A need that already has a `starting` run resumes
/// it (its recorded entry is kept and `entry` is ignored). Otherwise the run
/// row is written first, in state `starting`; then [`resume`] sends the
/// sentinel and the entry step (each step row is written right after its
/// send), binds both, posts the start note and sets the run `live`. A late
/// mark never changes a started run's kind.
///
/// # Errors
/// Returns a [`VivariumError`] when the entry is not one of the flow's
/// entries, or a mail or store call fails; the `starting` run is then left
/// for [`resume`].
pub fn start<M>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    need: &str,
    entry: Option<&str>,
) -> Result<RunRow, VivariumError>
where
    M: MailWriter + MailReader,
{
    let runs = store.runs_of(need)?;
    if let Some(row) = runs.iter().find(|row| row.state == RUN_STARTING) {
        return resume(ctx, store, mail, row);
    }
    let sent = mail.sentinel_count(need)?;
    let known = runs.iter().map(|row| row.run as usize).max().unwrap_or(0);
    let run = u32::try_from(sent.max(known) + 1)
        .map_err(|_| VivariumError::Message(format!("need {need}: run number out of range")))?;
    let kind = run_kind(ctx, mail, need)?;
    let def = load(kind)?;
    let entry = resolve_entry(&def, kind, entry)?;
    let row = RunRow {
        need: need.to_string(),
        run,
        flow: kind.name().to_string(),
        flow_version: FLOW_VERSION_NUMBER,
        kind: kind.name().to_string(),
        entry,
        state: RUN_STARTING.to_string(),
    };
    store.insert_run(&row)?;
    resume(ctx, store, mail, &row)
}

/// Finish a `starting` run: send the sentinel and the entry step unless
/// their step rows already exist, bind, post the start note, set the run
/// `live`. On a resume the start note may be posted a second time.
///
/// # Errors
/// Returns a [`VivariumError`] when a mail or store call fails.
pub fn resume<M>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    run: &RunRow,
) -> Result<RunRow, VivariumError>
where
    M: MailWriter + MailReader,
{
    let need = run.need.as_str();
    let number = run.run;
    let entry = run.entry.as_str();
    let rows = store.steps_of(need, number)?;
    let sentinel = send_missing(
        ctx,
        store,
        mail,
        &rows,
        (need, number, SENTINEL_NODE),
        (
            format!("{SENTINEL_MARKER} {need} r{number}"),
            StepState::Running,
        ),
    )?;
    let step = send_missing(
        ctx,
        store,
        mail,
        &rows,
        (need, number, entry),
        (
            format!("{STEP_MARKER} {need} r{number} {entry}"),
            StepState::Pending,
        ),
    )?;
    mail.bind(need, &[sentinel, step])?;
    // The Q7 remote rule: another member takes the pump's close only after
    // the pump has posted on the need's thread. Post to everyone who holds
    // the need, before any step can finish.
    mail.post_to_need_audience(
        need,
        &format!("pump: run r{number} started ({}, entry {entry})", run.kind),
    )?;
    store.set_run_state(need, number, RUN_LIVE_STATE)?;

    if number >= ALARM_RUN {
        ctx.exception(
            "flow-run-4",
            need,
            &format!("need {need} is on run {number}: a CTO should look before another restart"),
        );
    }
    Ok(RunRow {
        state: RUN_LIVE_STATE.to_string(),
        ..run.clone()
    })
}

/// The task of `node` in `rows`, or a fresh send followed at once by its step row.
fn send_missing<M: MailWriter>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    rows: &[StepRow],
    (need, run, node): (&str, u32, &str),
    (body, state): (String, StepState),
) -> Result<String, VivariumError> {
    if let Some(row) = rows.iter().find(|r| r.node == node) {
        return Ok(row.task.clone());
    }
    let from = ctx.cfg.reply_from.as_str();
    let task = mail.send_step(from, from, &format!("{need} r{run} {node}"), &body, &[])?;
    store.insert_step(need, &step_row(run, node, &task, state))?;
    Ok(task)
}

fn resolve_entry(
    def: &FlowDef,
    kind: FlowKind,
    entry: Option<&str>,
) -> Result<String, VivariumError> {
    match entry {
        Some(node) if def.entries.iter().any(|e| e == node) => Ok(node.to_string()),
        Some(node) => Err(VivariumError::Message(format!(
            "{} flow has no entry `{node}`",
            kind.name()
        ))),
        None => {
            def.entries.first().cloned().ok_or_else(|| {
                VivariumError::Message(format!("{} flow has no entries", kind.name()))
            })
        }
    }
}

fn step_row(run: u32, node: &str, task: &str, state: StepState) -> StepRow {
    StepRow {
        run,
        node: node.to_string(),
        task: task.to_string(),
        state,
        verdict: None,
    }
}

/// The run's kind: see [`kind::of`]. A record word that names no kind raises
/// `flow-kind-unknown` and the marks decide.
fn run_kind<M>(ctx: &Ctx, mail: &M, need: &str) -> Result<FlowKind, VivariumError>
where
    M: MailWriter + MailReader,
{
    let body = MailReader::open_record(mail, need)?.map(|(_, body)| body);
    let marks = mail.need_marks(need)?;
    let (kind, unknown) = kind::of(body.as_deref(), &marks);
    if let Some(word) = unknown {
        ctx.exception(
            "flow-kind-unknown",
            need,
            &format!("record body names kind `{word}`; using the marks"),
        );
    }
    Ok(kind)
}

#[cfg(test)]
#[path = "start_test.rs"]
mod tests;

#[cfg(test)]
#[path = "start_resume_test.rs"]
mod resume_tests;
