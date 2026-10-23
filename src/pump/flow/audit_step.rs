//! Handler for the auditor steps audit1 and audit2. N5 starts the audit job and reads its verdict here.

use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::jobs::JobStore;

pub(crate) fn register<S: JobStore, M: FlowMail>(_registry: &mut Registry<S, M>) {}
