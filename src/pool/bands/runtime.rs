//! Shared band-runtime decision types: what a selection answers and what a
//! pool currently has free.
//!
//! These types are the contract between whoever resolves a band to a
//! model-and-reasoning pair and whoever orchestrates a dispatch session;
//! neither resolution nor session orchestration lives in this module. Every
//! count and deadline below is already scoped to the one pool the snapshot
//! was taken for: nothing here crosses pools.

use std::collections::BTreeMap;

/// The answer to one dispatch-selection question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision<T> {
    /// A choice was resolved; the payload carries it.
    Ready(T),
    /// Nothing can be dispatched yet, for the reason the [`Wait`] names.
    Waiting(Wait),
}

/// Why a dispatch must wait, and when asking again may succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wait {
    /// What blocks the dispatch.
    pub reason: WaitReason,
    /// The absolute epoch second after which asking again may succeed,
    /// when the block has a deadline and `None` when it does not.
    pub retry_at: Option<u64>,
}

/// What blocks a dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    /// The pool's per-model cap is spent.
    ModelCap,
    /// A provider is cooling down after a failure.
    ProviderCooldown,
    /// The pool offers no choice for the band right now.
    BandUnavailable,
    /// The dispatch session is already busy elsewhere.
    DispatchBusy,
}

impl WaitReason {
    /// The stable word this reason answers as in reports and letters.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::ModelCap => "model-cap",
            Self::ProviderCooldown => "provider-cooldown",
            Self::BandUnavailable => "band-unavailable",
            Self::DispatchBusy => "dispatch-busy",
        }
    }
}

/// What one pool currently has free, as of `now`.
///
/// Model counts and unresolved claims are already scoped to the one pool
/// the snapshot was taken for. Provider deadlines are absolute epoch
/// seconds keyed by [`super::cooldown::provider_key`], so every slug a
/// provider serves cools down together.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Availability {
    /// Live model slots per model slug within the pool.
    pub model_counts: BTreeMap<String, u64>,
    /// Claims the pool handed out that no attempt has settled yet.
    pub unresolved_claims: u64,
    /// The absolute epoch second each provider cools down until.
    pub provider_until: BTreeMap<String, u64>,
    /// The instant the snapshot was taken at, in epoch seconds.
    pub now: u64,
}

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;
