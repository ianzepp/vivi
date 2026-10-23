//! Handler for the judge steps. N5 starts the judge job and maps its verdict to a flow word here.

use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::jobs::JobStore;

pub(crate) fn register<S: JobStore, M: FlowMail>(_registry: &mut Registry<S, M>) {}
