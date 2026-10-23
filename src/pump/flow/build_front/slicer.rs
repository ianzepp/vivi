//! The slicer step handler: a read-only slicer role job whose reply fans out into slices.

use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::jobs::JobStore;

/// Registry hook (filled by a later N4 slice).
pub(super) fn register<S: JobStore, M: FlowMail>(_registry: &mut Registry<S, M>) {}
