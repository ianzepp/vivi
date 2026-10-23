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

use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::jobs::JobStore;

/// Register the slice merge hook and tick phase. N6 fills it.
pub(crate) fn register<S: JobStore, M: FlowMail>(_registry: &mut Registry<S, M>) {}
