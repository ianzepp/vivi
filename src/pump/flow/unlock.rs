//! The lock handler and the unlock tick phase. N5 holds a lock step until its release condition is met, then advances it.
//!
//! A lock step waits for the operator and nothing else: no reply and no poll
//! frees it. `vivi pump unlock` posts an `unlock` action
//! ([`crate::mailspace::PumpActionRequest::unlock`]); each tick the
//! [`Unlocks`] phase closes the need's locked step `unlocked` and restarts the
//! run at the letter's entry (the flow's first entry when none is named).
//!
//! Every unlock letter is applied or refused exactly once: its id goes into
//! `applied_actions` either way, and a refusal raises [`UNLOCK_REFUSED`].

use crate::error::VivariumError;
use crate::mailspace::{PumpAction, PumpActionKind};
use crate::pump::flow::advance::restart::restart;
use crate::pump::flow::def::{FlowDef, FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailReader, MailWriter};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::tick::raise_once;
use crate::pump::host::Ctx;
use crate::pump::jobs::JobStore;
use crate::pump::store::step_payload::StepPayloadRow;
use crate::pump::store::{PumpStore, RUN_LIVE};

/// The exception kind raised for an unlock that cannot be applied.
pub const UNLOCK_REFUSED: &str = "flow-unlock-refused";

/// The exception kind raised once when a run reaches its lock.
pub const NEED_LOCKED: &str = "flow-need-locked";

/// The verdict a released lock step closes with.
const UNLOCKED: &str = "unlocked";

/// A lock step: open the task, then wait for an unlock letter.
pub(crate) struct LockWait;

impl StepHandler for LockWait {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::OpenAndWait)
    }

    fn poll(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<Option<String>, VivariumError> {
        let runs = cx.store.runs_of(&cx.run.need)?.len();
        let why = cto_why(&cx.store.step_payloads_of_run(&cx.run.need, cx.run.run)?);
        let text = format!(
            "need {need} r{run} is locked (run count {runs}): {why}; expected an operator \
             decision; actual: the run waits at `{node}`; release with \
             `vivi pump unlock {need} [entry]`",
            need = cx.run.need,
            run = cx.run.run,
            node = step.node,
        );
        raise_once(cx.ctx, NEED_LOCKED, &step.task, &text);
        Ok(None)
    }
}

/// What the run's newest CTO step said, for the lock alarm.
fn cto_why(rows: &[StepPayloadRow]) -> String {
    let Some(row) = rows.iter().rfind(|row| row.class == NodeClass::Cto.name()) else {
        return "reached without a CTO answer".to_string();
    };
    let front = &row.payload["front"];
    let Some(word) = front["answer"].as_str() else {
        let job = row.payload["job"].as_str().unwrap_or("unknown");
        return format!("the CTO answered `failed`: no usable CTO envelope (job {job})");
    };
    let reason = front["reason"].as_str().unwrap_or_default();
    let mut why = format!("the CTO answered `{word}`: {reason}");
    if let Some(fact) = front["new_fact"].as_str().filter(|f| !f.trim().is_empty()) {
        why.push_str(&format!(" new_fact: {fact}"));
    }
    why
}

/// The tick phase that applies unlock letters.
struct Unlocks;

impl<S: JobStore, M: FlowMail> TickPhase<S, M> for Unlocks {
    fn name(&self) -> &'static str {
        "unlocks"
    }

    fn run(
        &self,
        ctx: &Ctx,
        _st: &mut S,
        store: &PumpStore,
        mail: &mut M,
    ) -> Result<(), VivariumError> {
        apply_pending(ctx, store, mail)
    }
}

pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    for kind in [FlowKind::Build, FlowKind::Repair] {
        registry.handler(kind, NodeClass::Lock, Box::new(LockWait));
    }
    registry.phase(Box::new(Unlocks));
}

/// Apply every unlock letter the engine has not applied yet.
///
/// # Errors
/// Returns a [`VivariumError`] when pending actions or the applied set cannot
/// be read or saved.
pub(crate) fn apply_pending<M: MailWriter + MailReader>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
) -> Result<(), VivariumError> {
    let Some(mailspace) = crate::pump::record::open_mailspace(ctx) else {
        return Ok(());
    };
    let applied = store.applied_action_ids()?;
    for action in mailspace.pump_pending_actions(&applied)? {
        if action.request.kind != PumpActionKind::Unlock {
            continue;
        }
        match apply_one(ctx, store, mail, &action) {
            Ok(()) => ctx.log(&format!("unlock applied: {}", action.need)),
            Err(reason) => ctx.exception(UNLOCK_REFUSED, &action.need, &reason),
        }
        store.mark_action_applied(&action.id, ctx.host.now())?;
    }
    Ok(())
}

/// Apply one unlock; the error is the reason it was refused.
fn apply_one<M: MailWriter + MailReader>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    action: &PumpAction,
) -> Result<(), String> {
    let need = action.need.as_str();
    let runs = store
        .runs_of(need)
        .map_err(|error| format!("cannot read the run index: {error}"))?;
    let run = runs
        .into_iter()
        .rfind(|row| row.state == RUN_LIVE)
        .ok_or_else(|| format!("need {need} has no live run"))?;
    let def = flow_of(&run)?;
    let row = locked_step(store, need, &run, &def)?;
    let entry = entry_of(action, &def)?;
    mail.close_task(&row.task, UNLOCKED, None, None)
        .map_err(|error| format!("closing the lock step failed: {error}"))?;
    store
        .set_step_state(&row.task, StepState::Done, Some(UNLOCKED))
        .map_err(|error| format!("cannot record the unlock: {error}"))?;
    restart(ctx, store, mail, &run, &entry)
        .map(|_| ())
        .map_err(|error| format!("restarting at `{entry}` failed: {error}"))
}

/// The flow a run follows; a kind with no shipped flow cannot be unlocked.
fn flow_of(run: &RunRow) -> Result<FlowDef, String> {
    let kind = FlowKind::parse(&run.kind)
        .ok_or_else(|| format!("run r{} has unknown flow kind `{}`", run.run, run.kind))?;
    load(kind).map_err(|error| format!("cannot load the {} flow: {error}", run.kind))
}

/// The run's waiting step whose node is a lock.
fn locked_step(
    store: &PumpStore,
    need: &str,
    run: &RunRow,
    def: &FlowDef,
) -> Result<StepRow, String> {
    let rows = store
        .steps_of(need, run.run)
        .map_err(|error| format!("cannot read the step index: {error}"))?;
    rows.into_iter()
        .find(|row| {
            row.state == StepState::Waiting
                && def
                    .nodes
                    .iter()
                    .any(|node| node.id == row.node && node.class == NodeClass::Lock)
        })
        .ok_or_else(|| format!("run r{} of need {need} has no locked step", run.run))
}

/// The entry to restart at: the letter's word, else the flow's first entry.
fn entry_of(action: &PumpAction, def: &FlowDef) -> Result<String, String> {
    let entry = match action.request.word.as_deref().map(str::trim) {
        Some(word) if !word.is_empty() => word.to_string(),
        _ => def
            .entries
            .first()
            .cloned()
            .ok_or_else(|| format!("the {} flow has no entries", def.kind.name()))?,
    };
    if def.entries.contains(&entry) {
        Ok(entry)
    } else {
        Err(format!(
            "`{entry}` is not an entry of the {} flow (entries: {})",
            def.kind.name(),
            def.entries.join(", ")
        ))
    }
}

#[cfg(test)]
#[path = "unlock_test.rs"]
mod tests;
