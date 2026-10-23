//! The step-pending phase: starts steps whose handlers are waiting on a precondition.

use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::jobs::JobStore;

/// Registry hook (filled by a later N4 slice).
pub(crate) fn register<S: JobStore, M: FlowMail>(_registry: &mut Registry<S, M>) {}
