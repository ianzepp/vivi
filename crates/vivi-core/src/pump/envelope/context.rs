//! Immutable, offline context carried into reply validation.

use serde::{Deserialize, Serialize};
use vivi_mail::letter::roles::{RoleKind, SlicerMode};

/// Requirements derived before a slicer reply is inspected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlicerRequirements {
    /// Requested mode, or `None` when that part of context is unavailable.
    #[serde(default)]
    pub mode: Option<SlicerMode>,
    /// Configured targets. `None` means unavailable; `Some(vec![])` means none.
    #[serde(default)]
    pub configured_targets: Option<Vec<String>>,
    /// Whether each slice target must be configured.
    #[serde(default)]
    pub check_targets: bool,
    /// Whether each slice must declare a write scope.
    #[serde(default)]
    pub check_scope: bool,
    /// Earlier slice IDs. `None` means unavailable; an empty list is known empty.
    #[serde(default)]
    pub earlier_slice_ids: Option<Vec<String>>,
}

/// Small immutable input to offline reply validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplyContract {
    /// Canonical `RoleKind::name()` spelling, never a seat alias.
    pub role: String,
    /// Optional run number, used by run-sensitive role rules.
    #[serde(default)]
    pub run: Option<u32>,
    /// Optional additional answer allow-list; `None` means unrestricted here.
    #[serde(default)]
    pub answers: Option<Vec<String>>,
    /// Slicer-only requirements; absent context is not an empty set.
    #[serde(default)]
    pub slicer: Option<SlicerRequirements>,
}

impl ReplyContract {
    /// Make a contract from a canonical role kind.
    #[must_use]
    pub fn for_role(role: RoleKind) -> Self {
        Self {
            role: role.name().to_owned(),
            run: None,
            answers: None,
            slicer: None,
        }
    }

    /// Resolve only the published canonical role names.
    #[must_use]
    pub fn role_kind(&self) -> Option<RoleKind> {
        canonical_role(&self.role)
    }
}

/// Convert a canonical role name to its kind; aliases are deliberately refused.
#[must_use]
pub fn canonical_role(name: &str) -> Option<RoleKind> {
    RoleKind::ALL.into_iter().find(|role| role.name() == name)
}

#[cfg(test)]
#[path = "context_test.rs"]
mod tests;
