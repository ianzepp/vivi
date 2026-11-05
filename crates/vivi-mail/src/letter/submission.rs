//! Serializable, versioned context supplied with a role reply.

use serde::{Deserialize, Serialize};

mod context;
pub use context::{
    NEW_FACT_RUN, built_reports_failed_check, cto_missing_new_fact, step_answer_compatible,
    topological_order, validate_slices, wrong_slicer_mode,
};

use super::roles::{RoleKind, SlicerMode};

/// Current reply-contract wire version.
pub const REPLY_CONTRACT_VERSION: u32 = 1;

/// Execution and slicing context kept separate from the transport body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyContract {
    pub version: u32,
    pub role: RoleKind,
    pub bound_step: String,
    pub run: u32,
    pub permitted_answers: Vec<String>,
    pub expected_slicer_mode: Option<SlicerMode>,
    pub target: Option<String>,
    pub scope: Vec<String>,
    pub dependencies: Vec<String>,
}

impl ReplyContract {
    /// Build a contract with the role's established answer vocabulary.
    #[must_use]
    pub fn new(role: RoleKind, bound_step: impl Into<String>, run: u32) -> Self {
        Self {
            version: REPLY_CONTRACT_VERSION,
            role,
            bound_step: bound_step.into(),
            run,
            permitted_answers: role
                .answer_words()
                .iter()
                .map(|word| (*word).to_string())
                .collect(),
            expected_slicer_mode: None,
            target: None,
            scope: Vec::new(),
            dependencies: Vec::new(),
        }
    }
}

#[cfg(test)]
#[path = "submission_test.rs"]
mod tests;
