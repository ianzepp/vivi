//! Provider failure classification: which failures cool a provider down,
//! for how long, and the cooldown receipt that records one.

use serde::{Deserialize, Serialize};

/// The class of a provider failure, and how long it cools the provider
/// down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureClass {
    /// The provider throttled us: a short cooldown.
    #[serde(rename = "rate-limit")]
    RateLimit,
    /// The provider's budget is spent: a long cooldown.
    #[serde(rename = "quota")]
    Quota,
}

impl FailureClass {
    /// How long this class cools a provider down, in seconds.
    #[must_use]
    pub fn cooldown_secs(self) -> u64 {
        match self {
            FailureClass::RateLimit => 60,
            FailureClass::Quota => 300,
        }
    }
}

/// The provider a model slug belongs to: the prefix before the first slash,
/// or the whole slug when it carries none. `zai/glm-5.3-flash` and
/// `zai/glm-5` share the `zai` provider; `glm-5.3-flash` stands alone.
#[must_use]
pub fn provider_key(model: &str) -> &str {
    match model.split_once('/') {
        Some((provider, _)) => provider,
        None => model,
    }
}

/// A cooldown observed on a provider after a failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCooldown {
    /// The provider the failure keys to, per [`provider_key`].
    pub provider: String,
    /// The failure class observed.
    pub class: FailureClass,
    /// When the failure was observed, as unix seconds.
    pub observed_at: u64,
    /// When the cooldown lifts, as unix seconds.
    pub expires_at: u64,
}

#[cfg(test)]
#[path = "provider_failure_test.rs"]
mod tests;
