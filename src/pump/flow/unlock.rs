//! The lock handler and the unlock tick phase. N5 holds a lock step until its release condition is met, then advances it.

use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::jobs::JobStore;

pub(crate) fn register<S: JobStore, M: FlowMail>(_registry: &mut Registry<S, M>) {}
