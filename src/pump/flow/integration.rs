//! Build-flow merge to main and sync.

mod collect;
mod start;

use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::jobs::JobStore;

/// Register the build flow's merge-main and sync handlers and the merge-main
/// collector. The `st1` rung already runs through `rung_step`.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.handler(
        FlowKind::Build,
        NodeClass::MergeMain,
        Box::new(start::MergeMainStart),
    );
    registry.handler(
        FlowKind::Build,
        NodeClass::Merge,
        Box::new(start::SyncStart),
    );
    registry.collector(
        FlowKind::Build,
        NodeClass::MergeMain,
        Box::new(collect::BuildMergeMain),
    );
}

#[cfg(test)]
#[path = "integration/order_test.rs"]
mod order_tests;
