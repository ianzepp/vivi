//! Shared band-runtime decision types: what a selection answers and what a
//! pool currently has free.
//!
//! These types are the contract between whoever resolves a band to a
//! model-and-reasoning pair and whoever orchestrates a dispatch session;
//! neither resolution nor session orchestration lives in this module. Every
//! count and deadline below is already scoped to the one pool the snapshot
//! was taken for: nothing here crosses pools.
//!
//! Folding recorded evidence into a snapshot is [`Availability::from_evidence`];
//! gathering that evidence live is I/O that belongs to [`super::live`].

use std::collections::BTreeMap;

use crate::pool::record::OpenDispatch;
use crate::pool::slot::SlotStatus;

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

impl Availability {
    /// Fold one pool's authoritative evidence into a snapshot: every open
    /// dispatch record of `pool` counts its model, every census slot claimed
    /// by a task with no matching open record counts as an unresolved claim,
    /// and the provider deadlines and `now` are kept as given.
    ///
    /// Counts never filter on role or kind, target, band or reasoning: a
    /// model's cap is spent by the model alone, and pools never share caps —
    /// only records naming `pool` count. A claimed census slot whose task
    /// holds no matching open record (a dispatch recorded by a binary too
    /// old to write one, or a lost record) is an unresolved occupancy,
    /// counted conservatively against every capped model later. A recorded
    /// open job stays counted even when the census does not list its slot.
    ///
    /// `deadlines` carry each provider's cooldown end, keyed by
    /// [`super::cooldown::provider_key`]. This folds evidence only: the
    /// caller passes a successful census and must never substitute an empty
    /// one for a listing that failed.
    #[must_use]
    pub fn from_evidence(
        pool: &str,
        open: &[OpenDispatch],
        slots: &[SlotStatus],
        deadlines: BTreeMap<String, u64>,
        now: u64,
    ) -> Self {
        let mut model_counts: BTreeMap<String, u64> = BTreeMap::new();
        for dispatch in open.iter().filter(|d| d.record.pool == pool) {
            *model_counts
                .entry(dispatch.record.model.clone())
                .or_default() += 1;
        }
        let unresolved_claims = slots
            .iter()
            .filter(|slot| slot.pool == pool && slot.task.is_some())
            .filter(|slot| !claim_is_recorded(open, pool, slot))
            .count() as u64;
        Self {
            model_counts,
            unresolved_claims,
            provider_until: deadlines,
            now,
        }
    }
}

/// True when an open record of `pool` is the recorded side of this claimed
/// census slot: same pool, same machine, same task. Such a claim is that
/// dispatch, already counted in its model's occupancy — never twice.
fn claim_is_recorded(open: &[OpenDispatch], pool: &str, slot: &SlotStatus) -> bool {
    open.iter().any(|d| {
        d.record.pool == pool
            && d.record.machine == slot.id
            && Some(d.task.as_str()) == slot.task.as_deref()
    })
}

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;
