//! Rung steps: start rung jobs and collect them with one retry.

use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::jobs::JobStore;

/// Registry hook: N4a-28 registers the rung step handlers here.
pub(crate) fn register<S: JobStore, M: FlowMail>(_registry: &mut Registry<S, M>) {}
