//! The build flow's front half: planner, admitter, slicer, fanout start and the phases around them.
//!
//! `register` runs the front's hooks in a fixed order, and phases run in
//! registration order, so this order is the N4 phase order.

mod admitter;
mod fanout_start;
mod park;
mod planner;
mod slicer;

use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::{
    follow_up, operator_actions, ready_slices, reslice, step_pending, thread_change,
};
use crate::pump::jobs::JobStore;

/// Registry hook: every build-front handler and phase, in phase order.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    planner::register(registry);
    admitter::register(registry);
    slicer::register(registry);
    fanout_start::register(registry);
    park::register(registry);
    operator_actions::register(registry);
    thread_change::register(registry);
    reslice::register(registry);
    follow_up::register(registry);
    ready_slices::register(registry);
    step_pending::register(registry);
}

#[cfg(test)]
#[path = "build_front_e2e_test.rs"]
mod e2e;
