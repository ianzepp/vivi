//! Provider-health vocabulary: which provider a model belongs to and what
//! one rate-limit or quota observation records.
//!
//! A provider key is derived from the model slug alone, never from the
//! pool's configured provider: one pool may serve models from several
//! upstreams, so the slug is the truth. Nothing here classifies failures,
//! replays events, or performs I/O; a later slice wires the classifier and
//! the collected-note replay.

use chrono::Duration;
use serde::{Deserialize, Serialize};

/// The default cooldown, in seconds, applied to a provider after one
/// rate-limit or quota observation.
pub const COOLDOWN_SECS: u64 = 60;

/// The namespaced `pool_collected` note key that carries a serialized
/// [`HealthObservation`]. Dotted with a `pool.` prefix so it cannot collide
/// with a job's plain result fields.
pub const COLLECTED_NOTE_KEY: &str = "pool.health";

/// Which upstream failure an observation saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    /// The upstream answered "slow down": a transient rate limit.
    RateLimit,
    /// The upstream answered "no budget": the key's quota is spent.
    Quota,
}

/// One provider-health observation: which provider, what was seen, when,
/// and until when the provider is cooling down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthObservation {
    /// The provider key the failing model slug maps to (see
    /// [`provider_key`]).
    pub provider: String,
    /// What the upstream failure was.
    pub classification: Classification,
    /// When the failure was observed.
    pub observed_at: chrono::DateTime<chrono::Utc>,
    /// When the provider may be tried again: `observed_at` plus
    /// [`COOLDOWN_SECS`].
    pub cooldown_until: chrono::DateTime<chrono::Utc>,
}

impl HealthObservation {
    /// Record one observation of `provider` seen at `now`; the provider
    /// cools down for [`COOLDOWN_SECS`] seconds from then.
    #[must_use]
    pub fn at(
        provider: &str,
        classification: Classification,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            provider: provider.to_string(),
            classification,
            observed_at: now,
            cooldown_until: now + Duration::seconds(COOLDOWN_SECS as i64),
        }
    }
}

/// The provider key of a model slug: the component before the first slash,
/// or the entire slug when it carries no slash. `zai/glm-5.3-flash` keys to
/// `zai`; `glm-5.3-flash` keys to itself. Never the pool's configured
/// provider.
#[must_use]
pub fn provider_key(model: &str) -> &str {
    match model.split_once('/') {
        Some((provider, _rest)) => provider,
        None => model,
    }
}

#[cfg(test)]
#[path = "health_test.rs"]
mod tests;
