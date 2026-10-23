//! The shipped flow registry: `flows/*.mmd` compiled into the binary.

use super::def::FlowKind;

/// The version of the shipped flows: the crate version.
pub const FLOW_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The Mermaid source of a shipped flow.
#[must_use]
pub fn shipped(kind: FlowKind) -> &'static str {
    match kind {
        FlowKind::Build => include_str!("../../../flows/build.mmd"),
        FlowKind::Research => include_str!("../../../flows/research.mmd"),
        FlowKind::Release => include_str!("../../../flows/release.mmd"),
    }
}
