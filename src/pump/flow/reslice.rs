//! Reslice: split, rework and follow-up slicer jobs on an existing slice.

use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::jobs::JobStore;

/// Node-id prefix of a reslice step row.
pub const RESLICE_NODE_PREFIX: &str = "reslice:";

/// Job kind of a reslice slicer job.
pub const KIND: &str = "reslice";

/// What a reslice job asks the slicer to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Split,
    Rework,
    FollowUp,
}

impl Mode {
    /// The slicer-envelope mode word (a follow-up job is the slicer in rework mode).
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Split => "split",
            Self::Rework | Self::FollowUp => "rework",
        }
    }

    /// The mode's own name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Split => "split",
            Self::Rework => "rework",
            Self::FollowUp => "follow-up",
        }
    }
}

/// Registry hook (filled by a later N4 slice).
pub(crate) fn register<S: JobStore, M: FlowMail>(_registry: &mut Registry<S, M>) {}
