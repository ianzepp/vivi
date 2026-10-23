//! The admitter step handler (admit1, admit2): a read-only admitter role job.

use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::jobs::JobStore;

/// Registry hook (filled by a later N4 slice).
pub(super) fn register<S: JobStore, M: FlowMail>(_registry: &mut Registry<S, M>) {}
