//! Park handling for the build front: holds a run that needs the operator.

use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::jobs::JobStore;

/// Registry hook (filled by a later N4 slice).
pub(super) fn register<S: JobStore, M: FlowMail>(_registry: &mut Registry<S, M>) {}
