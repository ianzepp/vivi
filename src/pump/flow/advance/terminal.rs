//! Terminal handling: a `done` node lands the need, a `restart-<entry>` node
//! starts a new run, and a withdrawal closes what is left of a run.

use crate::error::VivariumError;
use crate::pump::flow::advance::restart::restart;
use crate::pump::flow::advance::route::Terminal;
use crate::pump::flow::advance::start::SENTINEL_NODE;
use crate::pump::flow::def::{FlowDef, NodeClass, VERDICT_WITHDRAWN};
use crate::pump::flow::index::run_state;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, NeedStatus, TaskStatus};
use crate::pump::flow::outcome::Outcome;
use crate::pump::host::Ctx;
use crate::pump::store::PumpStore;

/// Verdict word the sentinel closes with when the run delivered.
const VERDICT_DELIVERED: &str = "delivered";
/// The pump record outcome of a landed need.
const RECORD_LANDED: &str = "landed";
/// The pump record outcome of a withdrawn run.
const RECORD_WITHDRAWN: &str = "withdrawn";
/// The note a need is closed with when its run lands.
const LANDED_NOTE: &str = "landed by the pump flow";

/// What a terminal did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Finished {
    /// The need was closed and its record closed `landed`.
    Landed,
    /// A new run was started; the old one was superseded.
    Restarted(RunRow),
}

/// Handle one terminal reached from `outcome`'s step. The finished step was
/// already closed by routing.
///
/// # Errors
/// Returns a [`VivariumError`] when the run has no sentinel, the restart
/// entry is not an entry of the flow, or a mail or store call fails.
pub(crate) fn finish<M: MailWriter + MailReader>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    def: &FlowDef,
    outcome: &Outcome,
    terminal: &Terminal,
) -> Result<Finished, VivariumError> {
    match &terminal.class {
        NodeClass::Done => {
            let sentinel = sentinel_of(store, &outcome.need, outcome.run)?;
            land(store, mail, &outcome.need, outcome.run, &sentinel)?;
            Ok(Finished::Landed)
        }
        NodeClass::Restart(name) => {
            // The new run binds its sentinel while the old one is still open,
            // so the need never looks complete in between.
            let entry = entry_for(def, name);
            let old = store
                .runs_of(&outcome.need)?
                .into_iter()
                .find(|row| row.run == outcome.run)
                .ok_or_else(|| {
                    VivariumError::Parse(format!(
                        "need {} r{}: no run row to restart",
                        outcome.need, outcome.run
                    ))
                })?;
            let row = restart(ctx, store, mail, &old, &entry)?;
            Ok(Finished::Restarted(row))
        }
        other => Err(VivariumError::Parse(format!(
            "{} flow: node `{}` ({other:?}) is not a terminal",
            def.kind.name(),
            terminal.node
        ))),
    }
}

/// Complete a run left `finishing` (a tick that found it after a failed
/// `finish`): re-run the `done` sequence, skipping each step already done, then
/// set the run `done`. Safe to call on any number of ticks.
///
/// # Errors
/// Returns a [`VivariumError`] when the run has no sentinel or a mail or
/// store call fails.
pub(crate) fn resume<M: MailWriter + MailReader>(
    _ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    run: &RunRow,
) -> Result<(), VivariumError> {
    let sentinel = sentinel_of(store, &run.need, run.run)?;
    land(store, mail, &run.need, run.run, &sentinel)
}

/// The `done` sequence, idempotent step by step: finishing state, the need
/// close, the sentinel close, the record close, the done state.
fn land<M: MailWriter + MailReader>(
    store: &PumpStore,
    mail: &mut M,
    need: &str,
    run: u32,
    sentinel: &StepRow,
) -> Result<(), VivariumError> {
    // The run is no longer live, but it is not withdrawn either: a tick
    // that sees the need closed mid-finish must not read a withdrawal.
    store.set_run_state(need, run, run_state::FINISHING)?;
    // The travelling close goes first, while the record is open. The
    // sentinel closes after it, so the join finds the need already
    // done and the pump's letter is the close every member folds.
    if mail.need_state(need)? != NeedStatus::Done {
        mail.close_need(need, LANDED_NOTE)?;
    }
    if mail.task_state(&sentinel.task)?.status == TaskStatus::Open {
        close_row(store, mail, sentinel, VERDICT_DELIVERED)?;
    } else if sentinel.state != StepState::Done {
        store.set_step_state(&sentinel.task, StepState::Done, Some(VERDICT_DELIVERED))?;
    }
    if MailReader::open_record(mail, need)?.is_some() {
        mail.close_record(need, RECORD_LANDED)?;
    }
    store.set_run_state(need, run, run_state::DONE)
}

/// Withdraw a run: close every open task of it, the sentinel last, with
/// `withdrawn`; close the pump record `withdrawn`; mark the run withdrawn.
/// Nothing is ever reopened.
///
/// # Errors
/// Returns a [`VivariumError`] when a mail or store call fails.
pub fn withdraw<M: MailWriter + MailReader>(
    store: &PumpStore,
    mail: &mut M,
    run: &RunRow,
) -> Result<(), VivariumError> {
    let rows = store.steps_of(&run.need, run.run)?;
    let (sentinels, steps): (Vec<StepRow>, Vec<StepRow>) =
        rows.into_iter().partition(|r| r.node == SENTINEL_NODE);
    for row in steps.iter().chain(&sentinels) {
        if mail.task_state(&row.task)?.status == TaskStatus::Open {
            close_row(store, mail, row, VERDICT_WITHDRAWN)?;
        }
    }
    if MailReader::open_record(mail, &run.need)?.is_some() {
        mail.close_record(&run.need, RECORD_WITHDRAWN)?;
    }
    store.set_run_state(&run.need, run.run, run_state::WITHDRAWN)
}

fn close_row<M: MailWriter>(
    store: &PumpStore,
    mail: &mut M,
    row: &StepRow,
    verdict: &str,
) -> Result<(), VivariumError> {
    mail.close_task(&row.task, verdict, None, None)?;
    store.set_step_state(&row.task, StepState::Done, Some(verdict))
}

fn sentinel_of(store: &PumpStore, need: &str, run: u32) -> Result<StepRow, VivariumError> {
    store
        .steps_of(need, run)?
        .into_iter()
        .find(|r| r.node == SENTINEL_NODE)
        .ok_or_else(|| VivariumError::Parse(format!("need {need} r{run}: no sentinel step row")))
}

/// The entry a `restart-<name>` terminal starts: the flow's entry of that
/// name, else the numbered one (`restart-rework` starts `rework1`).
fn entry_for(def: &FlowDef, name: &str) -> String {
    if def.entries.iter().any(|e| e == name) {
        return name.to_string();
    }
    let numbered = format!("{name}1");
    if def.entries.contains(&numbered) {
        return numbered;
    }
    name.to_string()
}

#[cfg(test)]
#[path = "terminal_test.rs"]
mod tests;
