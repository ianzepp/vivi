//! Starting a flow run is a flow-node transition, so `advance` owns it: the
//! kind is fixed once, the sentinel goes first, then the entry step, both
//! bound in one letter.

use crate::error::VivariumError;
use crate::pump::flow::def::{FlowDef, FlowKind, SENTINEL_MARKER, STEP_MARKER};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
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

/// The flow version recorded in a run row.
const FLOW_VERSION_NUMBER: u32 = 2;

/// Start a new run on `need` at `entry` (the flow's first entry when `None`).
///
/// The sentinel task is sent before the entry step task; both are bound with
/// one `bind`, then the run row and both step rows are written. A late mark
/// never changes a started run's kind.
///
/// # Errors
/// Returns a [`VivariumError`] when the entry is not one of the flow's
/// entries, or a mail or store call fails.
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
    let run = u32::try_from(mail.sentinel_count(need)? + 1)
        .map_err(|_| VivariumError::Message(format!("need {need}: run number out of range")))?;
    let kind = run_kind(ctx, mail, need)?;
    let def = load(kind)?;
    let entry = resolve_entry(&def, kind, entry)?;

    let from = ctx.cfg.reply_from.clone();
    let sentinel = mail.send_step(
        &from,
        &from,
        &format!("{need} r{run} sentinel"),
        &format!("{SENTINEL_MARKER} {need} r{run}"),
        &[],
    )?;
    let step = mail.send_step(
        &from,
        &from,
        &format!("{need} r{run} {entry}"),
        &format!("{STEP_MARKER} {need} r{run} {entry}"),
        &[],
    )?;
    mail.bind(need, &[sentinel.clone(), step.clone()])?;
    // The Q7 remote rule: another member takes the pump's close only after
    // the pump has posted on the need's thread. Post to everyone who holds
    // the need, before any step can finish.
    mail.post_to_need_audience(
        need,
        &format!("pump: run r{run} started ({}, entry {entry})", kind.name()),
    )?;

    let row = record_run(store, need, run, kind, &entry, [&sentinel, &step])?;

    if run >= ALARM_RUN {
        ctx.exception(
            "flow-run-4",
            need,
            &format!("need {need} is on run {run}: a CTO should look before another restart"),
        );
    }
    Ok(row)
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

/// Write the run row, the sentinel's step row and the entry step's row.
fn record_run(
    store: &PumpStore,
    need: &str,
    run: u32,
    kind: FlowKind,
    entry: &str,
    [sentinel, step]: [&str; 2],
) -> Result<RunRow, VivariumError> {
    let row = RunRow {
        need: need.to_string(),
        run,
        flow: kind.name().to_string(),
        flow_version: FLOW_VERSION_NUMBER,
        kind: kind.name().to_string(),
        entry: entry.to_string(),
        state: RUN_LIVE_STATE.to_string(),
    };
    store.insert_run(&row)?;
    store.insert_step(
        need,
        &step_row(run, SENTINEL_NODE, sentinel, StepState::Running),
    )?;
    store.insert_step(need, &step_row(run, entry, step, StepState::Pending))?;
    Ok(row)
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

/// The run's kind: a `kind: <word>` line in the open record's body wins,
/// then the need's marks (`research`, `release`), else build.
fn run_kind<M>(ctx: &Ctx, mail: &M, need: &str) -> Result<FlowKind, VivariumError>
where
    M: MailWriter + MailReader,
{
    if let Some((_, body)) = MailReader::open_record(mail, need)?
        && let Some(word) = kind_line(&body)
    {
        match FlowKind::parse(word) {
            Some(kind) => return Ok(kind),
            None => ctx.exception(
                "flow-kind-unknown",
                need,
                &format!("record body names kind `{word}`; using the marks"),
            ),
        }
    }
    let marks = mail.need_marks(need)?;
    let has = |mark: &str| marks.iter().any(|m| m == mark);
    Ok(if has("research") {
        FlowKind::Research
    } else if has("release") {
        FlowKind::Release
    } else {
        FlowKind::Build
    })
}

fn kind_line(body: &str) -> Option<&str> {
    body.lines()
        .find_map(|line| line.trim().strip_prefix("kind:"))
        .map(str::trim)
        .filter(|word| !word.is_empty())
}
