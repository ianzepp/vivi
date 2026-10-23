//! Node override handler for the build node rework1. N5 builds the rework job from the audit findings here.

use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::jobs::JobStore;

pub(crate) fn register<S: JobStore, M: FlowMail>(_registry: &mut Registry<S, M>) {}
