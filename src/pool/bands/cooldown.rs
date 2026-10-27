//! Cooldown evidence: what a provider failure is worth, in absolute time.
//!
//! Evidence records one observed failure as the provider it cools down,
//! the absolute epoch second the cooldown ends at, and the stable failure
//! class the failure was classified as. Grouping model slugs into
//! providers is [`provider_key`], so every slug a provider serves cools
//! down together. Classifying a live failure into evidence is live I/O
//! and lives in [`super::live`], not here.

use serde::{Deserialize, Serialize};

/// How a failure that produced cooldown evidence was classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureClass {
    /// The provider refused the request for sending too much, too fast.
    RateLimit,
    /// The provider refused the request for exhausted credit or quota.
    Quota,
}

impl FailureClass {
    /// The stable word this class answers as in reports and letters.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::RateLimit => "rate-limit",
            Self::Quota => "quota",
        }
    }
}

/// One observed provider failure, held as absolute time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    /// The provider the cooldown applies to, a [`provider_key`].
    pub provider: String,
    /// The absolute epoch second the cooldown ends at.
    pub until: u64,
    /// The failure class the failure was recorded as.
    pub class: FailureClass,
}

/// The provider a model slug belongs to: the prefix before the first
/// slash, or the entire slug when it is unqualified. `zai/glm-5.3-flash`
/// cools down `zai`; `glm-5.3-flash` is a provider of its own.
#[must_use]
pub fn provider_key(model: &str) -> &str {
    match model.split_once('/') {
        Some((provider, _)) => provider,
        None => model,
    }
}

#[cfg(test)]
#[path = "cooldown_test.rs"]
mod tests;
