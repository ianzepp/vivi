//! The join table and the raise check for decision nodes. N5 registers them in the registry's decide slot.

use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::jobs::JobStore;

pub(crate) fn register<S: JobStore, M: FlowMail>(_registry: &mut Registry<S, M>) {}
