//! The shipped flow registry: `flows/*.mmd` compiled into the binary.

use super::def::FlowKind;

/// The version of the shipped flows: the crate version.
pub const FLOW_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The Mermaid source of a shipped flow, or `None` for a kind whose flow file
/// is not shipped (every kind ships one today).
#[must_use]
pub fn shipped(kind: FlowKind) -> Option<&'static str> {
    match kind {
        FlowKind::Build => Some(include_str!("../../../flows/build.mmd")),
        FlowKind::Research => Some(include_str!("../../../flows/research.mmd")),
        FlowKind::Release => Some(include_str!("../../../flows/release.mmd")),
        FlowKind::Repair => Some(include_str!("../../../flows/repair.mmd")),
    }
}
