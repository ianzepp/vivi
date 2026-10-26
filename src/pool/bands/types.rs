//! Runtime availability vocabulary for the bands subsystem: the admission
//! envelope a resolver answers with, the reasons it may defer, the
//! provider-error classes a screen classifies, and the cooldown metadata
//! those classes mint.
//!
//! Band-string parsing is not repeated here. The `P` axis and the `S`
//! speed axis stay where they are ([`super::parse_band`]), parsed there
//! and unchanged; this module only names runtime outcomes, it never
//! re-reads a band string.
//!
//! Model provider identity comes from the model slug alone, via
//! [`provider_key`]. `PoolConfig.provider` names a harness, never a
//! model's provider, and is not consulted here.

use serde::{Deserialize, Serialize};

/// The collection-note metadata key a provider-error screen records a
/// cooldown under.
pub const COOLDOWN_NOTE_KEY: &str = "provider_cooldown";

/// The default cooldown duration: 60 seconds.
pub const DEFAULT_COOLDOWN_SECS: i64 = 60;

/// The outcome of an admission decision: a ready value of type `T`, or a
/// structured deferral.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Admission<T> {
    /// Admitted, carrying the resolved value.
    Ready(T),
    /// Refused for now, with the reason.
    Deferred(Deferred),
}

/// A deferral: why admission was refused and, when known, the absolute
/// epoch second after which it may be retried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Deferred {
    /// Why the admission was deferred.
    pub kind: DeferKind,
    /// The absolute epoch second the deferral expires, when known.
    pub until: Option<i64>,
}

/// Why an admission was deferred. Each kind has one stable word, its
/// kebab-case name, and that word is also its serde representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeferKind {
    /// The band's model is at its concurrency cap.
    ModelCap,
    /// The model's provider is cooling down after an error.
    ProviderCooldown,
    /// No choice in the pool serves the band.
    BandUnavailable,
    /// The dispatch lane is busy.
    DispatchBusy,
}

impl DeferKind {
    /// The stable word of this kind, as it travels in notes and wire forms.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::ModelCap => "model-cap",
            Self::ProviderCooldown => "provider-cooldown",
            Self::BandUnavailable => "band-unavailable",
            Self::DispatchBusy => "dispatch-busy",
        }
    }

    /// The kind with the given stable word, exact case.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "model-cap" => Some(Self::ModelCap),
            "provider-cooldown" => Some(Self::ProviderCooldown),
            "band-unavailable" => Some(Self::BandUnavailable),
            "dispatch-busy" => Some(Self::DispatchBusy),
            _ => None,
        }
    }
}

/// The provider-error classes a screen classifies a provider response as.
/// Each class has one stable word, its kebab-case name, and that word is
/// also its serde representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderErrorClass {
    /// The provider is rejecting requests for sending too many.
    RateLimit,
    /// The provider has no budget left for the credentials in use.
    Quota,
}

impl ProviderErrorClass {
    /// The stable word of this class.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::RateLimit => "rate-limit",
            Self::Quota => "quota",
        }
    }

    /// The class with the given stable word, exact case.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "rate-limit" => Some(Self::RateLimit),
            "quota" => Some(Self::Quota),
            _ => None,
        }
    }
}

/// Cooldown metadata: the provider-error class that started it, the model
/// provider it applies to, and the absolute epoch second it ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cooldown {
    /// The error class that started the cooldown.
    pub class: ProviderErrorClass,
    /// The model provider key the cooldown applies to.
    pub provider: String,
    /// The absolute epoch second the cooldown ends.
    pub until: i64,
}

/// The model provider key of a model slug: the part before the first slash
/// (`"anthropic/claude-x"` is `"anthropic"`), or the whole slug when it
/// carries no slash. Provider identity of a model comes from its slug
/// alone, never from `PoolConfig.provider`.
#[must_use]
pub fn provider_key(model: &str) -> &str {
    match model.split_once('/') {
        Some((provider, _)) => provider,
        None => model,
    }
}

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
