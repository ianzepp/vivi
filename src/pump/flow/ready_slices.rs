//! The ready-slices phase: starts slices whose dependencies are built.
//!
//! A pending slice is ready when every task it depends on is closed merged or
//! no-defect and no scope request holds it. Ready slices get their first hand
//! attempt through [`slice_dispatch::dispatch`], which records the need
//! branch base and sets the row `Running`.

use crate::error::VivariumError;
use crate::pump::flow::fanout::{self, VERDICT_MERGED, VERDICT_NO_DEFECT};
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, TaskStatus};
use crate::pump::flow::need_branch::{DELETING, LIVE, RENAMING};
use crate::pump::flow::scope::ScopeState;
use crate::pump::flow::slice_dispatch;
use crate::pump::host::Ctx;
use crate::pump::jobs::JobStore;
use crate::pump::store::PumpStore;

/// Registry hook: the ready-slices phase.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.phase(Box::new(ReadySlices));
}

/// Dispatch attempt 1 of every pending slice whose dependencies are built.
struct ReadySlices;

impl<S: JobStore, M: FlowMail> TickPhase<S, M> for ReadySlices {
    fn name(&self) -> &'static str {
        "ready-slices"
    }

    fn run(
        &self,
        ctx: &Ctx,
        st: &mut S,
        store: &PumpStore,
        mail: &mut M,
    ) -> Result<(), VivariumError> {
        let mut first_error = None;
        for run in store.live_runs()? {
            if let Err(error) = dispatch_ready(ctx, st, store, &*mail, &run) {
                ctx.log(&format!(
                    "ready slices of {} r{} failed: {error}",
                    run.need, run.run
                ));
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

/// The need branch is still being moved for this run: the branch-restarts
/// phase owns it and has its own alarm.
fn branch_moving(store: &PumpStore, run: &RunRow) -> Result<bool, VivariumError> {
    Ok(store.need_branch(&run.need)?.is_some_and(|row| {
        row.status == RENAMING
            || row.status == DELETING
            || (row.status == LIVE && row.run < run.run)
    }))
}

fn dispatch_ready<S: JobStore, M: FlowMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &M,
    run: &RunRow,
) -> Result<(), VivariumError> {
    if branch_moving(store, run)? {
        return Ok(());
    }
    let mut first_error = None;
    for row in store.steps_of(&run.need, run.run)? {
        if !fanout::is_slice_row(&row) || row.state != StepState::Pending {
            continue;
        }
        if let Err(error) = start_if_ready(ctx, st, store, mail, run, &row) {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn start_if_ready<S: JobStore, M: FlowMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &M,
    run: &RunRow,
    row: &StepRow,
) -> Result<(), VivariumError> {
    if matches!(
        store.scope_state(&row.task)?,
        Some(ScopeState::Wait | ScopeState::Rework)
    ) || !deps_built(store, mail, &row.task)?
    {
        return Ok(());
    }
    slice_dispatch::dispatch(ctx, st, store, mail, &run.need, row, 1)?;
    Ok(())
}

/// Every dependency of `task` is closed merged or no-defect. No link means no
/// dependencies.
fn deps_built<M: MailReader>(
    store: &PumpStore,
    mail: &M,
    task: &str,
) -> Result<bool, VivariumError> {
    let Some(link) = store.slice_link(task)? else {
        return Ok(true);
    };
    for dep in &link.deps {
        let state = mail.task_state(dep)?;
        let built = state.status == TaskStatus::Done
            && matches!(
                state.verdict.as_deref(),
                Some(VERDICT_MERGED | VERDICT_NO_DEFECT)
            );
        if !built {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
#[path = "ready_slices_test.rs"]
mod tests;
