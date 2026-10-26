//! Eligibility contracts shared by band selection: what a pool answers when
//! asked whether it can serve a band right now, why it may decline, and the
//! occupancy snapshot that answer folds.
//!
//! Foundation only: folding live state into a [`Snapshot`] and deriving
//! [`Selection`]s from it arrive with later slices.

use std::collections::HashMap;

/// Why a pool cannot serve a band right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Every slot the pool offers for the band's model is taken.
    Capacity,
    /// A provider the band's model belongs to is cooling down.
    ProviderCooldown,
    /// The pool serves no model for the band at all.
    BandUnavailable,
    /// The pool's admission gate is at its in-flight limit.
    AdmissionBusy,
}

impl Reason {
    /// The stable code this reason reports under, fit for letters, logs and
    /// tests: `band-capacity`, `provider-cooldown`, `band-unavailable` or
    /// `admission-busy`.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Reason::Capacity => "band-capacity",
            Reason::ProviderCooldown => "provider-cooldown",
            Reason::BandUnavailable => "band-unavailable",
            Reason::AdmissionBusy => "admission-busy",
        }
    }
}

/// A pool's answer to whether it can serve a band right now: the choice, or
/// why not and when to ask again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection<T> {
    /// The pool serves the band with this choice.
    Selected(T),
    /// The pool cannot serve the band right now.
    Unavailable(Unavailable),
}

/// Why a pool declined a band, and when asking again could help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavailable {
    /// Why the band is not servable right now.
    pub reason: Reason,
    /// When to ask again, as unix seconds. `None` when retrying changes
    /// nothing, such as a band the pool simply does not serve.
    pub retry_at: Option<u64>,
}

/// Pool and model occupancy at one instant, plus the cooldown expiry of
/// every provider the pools depend on. Defaults to an empty snapshot: every
/// pool idle, no provider cooling down.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// Slots taken per pool and model slug, keyed `(pool, model)`.
    pub pool_model_occupancy: HashMap<(String, String), usize>,
    /// When each provider's cooldown lifts, as unix seconds, keyed by
    /// provider slug (see [`super::provider_failure::provider_key`]).
    pub provider_expiry: HashMap<String, u64>,
}

#[cfg(test)]
#[path = "eligibility_test.rs"]
mod tests;
