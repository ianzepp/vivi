//! Pruning a landed need's branches, one cleanup job per ref.

use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::jobs::JobStore;

/// Registry hook: N4a-26 registers the prune phase here.
pub(crate) fn register<S: JobStore, M: FlowMail>(_registry: &mut Registry<S, M>) {}
