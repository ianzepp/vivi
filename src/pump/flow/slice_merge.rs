//! Model-free merge of a built slice onto the need branch.
//!
//! A no-model job under the merge role merges the hand commit onto the
//! need-branch tip, runs `[pump.merge] format` and the `task` rung, and the
//! runner publishes `refs/heads/vivi/need/<need>` only on exit 0. One slice
//! merge per need is in flight at a time.

mod apply;
mod classify;
mod queue;
mod spec;

use crate::error::VivariumError;
use crate::pump::envelope::roles::Hand;
use crate::pump::flow::collect_slice::BuiltHook;
use crate::pump::flow::collect_step::RunCx;
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::host::Ctx;
use crate::pump::jobs::JobStore;
use crate::pump::store::PumpStore;

/// Queues a built slice for its merge onto the need branch.
struct SliceMergeHook;

impl<S: JobStore, M: FlowMail> BuiltHook<S, M> for SliceMergeHook {
    fn built(
        &self,
        cx: &mut RunCx<'_, S, M>,
        step: &StepRow,
        _envelope: &Hand,
    ) -> Result<(), VivariumError> {
        cx.store
            .set_step_state(&step.task, StepState::Waiting, Some(queue::MERGE_QUEUED))
    }
}

/// Applies finished slice merges, then starts the next one per need.
struct SliceMerges;

impl<S: JobStore, M: FlowMail> TickPhase<S, M> for SliceMerges {
    fn name(&self) -> &'static str {
        "slice-merges"
    }

    fn run(
        &self,
        ctx: &Ctx,
        st: &mut S,
        store: &PumpStore,
        mail: &mut M,
    ) -> Result<(), VivariumError> {
        apply::run(ctx, st, store, mail)?;
        queue::run(ctx, st, store, mail)
    }
}

/// Register the slice merge hook and tick phase.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.built_hook(Box::new(SliceMergeHook));
    registry.phase(Box::new(SliceMerges));
}

#[cfg(test)]
#[path = "slice_merge_test.rs"]
mod tests;
