//! Build-flow merge to main and sync.

mod collect;
mod start;

use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::jobs::JobStore;

/// Register the build flow's merge-main and sync handlers. N6 fills it.
pub(crate) fn register<S: JobStore, M: FlowMail>(_registry: &mut Registry<S, M>) {}
