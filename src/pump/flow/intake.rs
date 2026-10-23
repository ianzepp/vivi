//! Intake: start a run for each need with an open pump record.

use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::jobs::JobStore;

/// Registry hook: N4a-22 registers the intake phase here.
pub(crate) fn register<S: JobStore, M: FlowMail>(_registry: &mut Registry<S, M>) {}
