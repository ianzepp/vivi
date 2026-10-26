//! Whether a pool's models can take work right now, at one instant.
//!
//! This is the runtime vocabulary for availability: capped model slugs and
//! provider cooldown deadlines, queried at an injected instant so the view
//! stays pure. Nothing here dispatches or mutates state; a later slice feeds
//! the view from provider-health observations (see [`super::health`]) and
//! the dispatch path consumes it.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::health;

/// The availability of a pool's models at one instant: which model slugs
/// are over their cap and which providers hold a cooldown deadline. `now`
/// is injected, so the same view answers for any moment it is given.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Availability {
    /// Model slugs over their cap, blocked outright.
    pub capped_models: BTreeSet<String>,
    /// The earliest known active cooldown deadline per provider key (see
    /// [`health::provider_key`]).
    pub provider_deadlines: BTreeMap<String, chrono::DateTime<chrono::Utc>>,
    /// The instant this view answers at.
    #[serde(default)]
    pub now: chrono::DateTime<chrono::Utc>,
}

/// Why (and until when) a set of model slugs cannot take work, at one
/// instant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unavailable {
    /// The blocked model slugs, sorted and deduplicated.
    pub blocked_models: Vec<String>,
    /// Why the models are blocked.
    pub reason: WaitingReason,
    /// When the earliest active provider cooldown ends, when one is among
    /// the blockers.
    pub until: Option<chrono::DateTime<chrono::Utc>>,
}

/// Why a set of model slugs is waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitingReason {
    /// A model slug is over its cap.
    ModelCap,
    /// A provider is cooling down.
    ProviderCooldown,
    /// The slugs are unavailable for mixed or unclassified reasons.
    ModelsUnavailable,
    /// The pool's admission is busy; the models themselves are fine.
    AdmissionBusy,
}

impl Availability {
    /// A view where nothing is blocked yet, answering at `now`.
    #[must_use]
    pub fn at(now: chrono::DateTime<chrono::Utc>) -> Self {
        Self {
            now,
            ..Self::default()
        }
    }

    /// Whether `model` can take work at this view's instant: it is not
    /// capped, and its provider holds no deadline still in the future. A
    /// deadline at or before `now` leaves the model available.
    #[must_use]
    pub fn model_available(&self, model: &str) -> bool {
        !self.capped_models.contains(model)
            && !self
                .provider_deadlines
                .get(health::provider_key(model))
                .is_some_and(|deadline| *deadline > self.now)
    }

    /// Aggregate `models` into one [`Unavailable`] when any of them is
    /// blocked at this view's instant. Cap-only blockers are
    /// [`WaitingReason::ModelCap`] with no `until`; cooldown-only blockers
    /// are [`WaitingReason::ProviderCooldown`] retaining the earliest
    /// active deadline; mixed blockers are [`WaitingReason::ModelsUnavailable`]
    /// retaining that same earliest deadline. `None` when every model is
    /// available.
    #[must_use]
    pub fn unavailable<I, S>(&self, models: I) -> Option<Unavailable>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut blocked = BTreeSet::new();
        let mut capped = false;
        let mut cooling = false;
        let mut until: Option<chrono::DateTime<chrono::Utc>> = None;
        for model in models {
            let model = model.as_ref();
            if self.capped_models.contains(model) {
                capped = true;
                blocked.insert(model.to_string());
                continue;
            }
            let Some(deadline) = self.provider_deadlines.get(health::provider_key(model)) else {
                continue;
            };
            if *deadline > self.now {
                cooling = true;
                blocked.insert(model.to_string());
                until = Some(until.map_or(*deadline, |earliest| earliest.min(*deadline)));
            }
        }
        if blocked.is_empty() {
            return None;
        }
        let reason = if capped && cooling {
            WaitingReason::ModelsUnavailable
        } else if capped {
            WaitingReason::ModelCap
        } else {
            WaitingReason::ProviderCooldown
        };
        Some(Unavailable {
            blocked_models: blocked.into_iter().collect(),
            reason,
            until,
        })
    }
}

#[cfg(test)]
#[path = "availability_test.rs"]
mod tests;
