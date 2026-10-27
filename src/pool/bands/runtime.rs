//! Shared band-runtime decision types and the availability-aware selection
//! that answers with them: what a selection answers, what a pool currently
//! has free, and which configured band choice is both served and free right
//! now.
//!
//! Session orchestration stays out: these types are the contract between
//! whoever resolves a band to a model-and-reasoning pair and whoever
//! orchestrates a dispatch session around it. Every count and deadline
//! below is already scoped to the one pool the snapshot was taken for:
//! nothing here crosses pools.
//!
//! Folding recorded evidence into a snapshot is [`Availability::from_evidence`];
//! gathering that evidence live is I/O that belongs to [`super::live`].

use std::collections::BTreeMap;

use rand_core::{OsRng, RngCore};

use crate::error::VivariumError;
use crate::pool::config::PoolConfig;
use crate::pool::config_bands::BandChoice;
use crate::pool::record::OpenDispatch;
use crate::pool::slot::SlotStatus;

use super::Choice;
use super::cooldown;

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

/// The model-and-reasoning pair `pool` offers for `band`, drawn only among
/// the band's choices that are free as of the snapshot: a choice whose
/// model has reached its configured pool cap is out, and so is every choice
/// whose provider is still cooling down. The survivors are sampled by
/// their configured relative weights, so a removed choice's share flows to
/// the rest, and duplicate model entries at different reasoning efforts
/// stay distinct choices. A band the pool configures nothing for stays a
/// configuration error — no amount of waiting serves it.
///
/// # Errors
/// As [`super::choose`]: a band the pool does not serve, a list the
/// configuration cap refuses, or an entropy failure of the system source
/// of randomness.
pub fn select_available(
    config: &PoolConfig,
    pool: &str,
    band: u8,
    availability: &Availability,
) -> Result<Decision<Choice>, VivariumError> {
    let mut rng = OsRng;
    select_available_with(config, pool, band, availability, &mut rng)
}

/// [`select_available`] with the source of randomness injected: the seam
/// the tests share, mirroring [`super::sample_pair`].
///
/// # Errors
/// As [`select_available`].
pub(crate) fn select_available_with<R: RngCore>(
    config: &PoolConfig,
    pool: &str,
    band: u8,
    availability: &Availability,
    rng: &mut R,
) -> Result<Decision<Choice>, VivariumError> {
    let key = format!("P{band}");
    let Some(list) = config
        .bands
        .entries
        .get(&key)
        .filter(|list| !list.is_empty())
    else {
        return Err(super::not_served(pool, band, &config.bands));
    };
    let free: Vec<BandChoice> = list
        .iter()
        .filter(|entry| !cap_blocks(config, &entry.model, availability))
        .filter(|entry| provider_deadline(&entry.model, availability).is_none())
        .cloned()
        .collect();
    if free.is_empty() {
        return Ok(Decision::Waiting(blocked_wait(config, list, availability)));
    }
    let entry = super::sample(&key, &free, rng)?;
    Ok(Decision::Ready(Choice {
        band,
        model: entry.model.clone(),
        reasoning: entry.reasoning.clone(),
    }))
}

/// Whether one dispatch of `model` is free to start: `Ready` when the
/// pool's cap for the model has room and the model's provider is not
/// cooling down. The judge is this model alone — its own cap, its own
/// provider's deadline — never another model's occupancy or a sibling
/// provider's cooldown, and no substitute choice answers for it.
#[must_use]
pub fn pinned_available(
    config: &PoolConfig,
    model: &str,
    availability: &Availability,
) -> Decision<()> {
    if cap_blocks(config, model, availability) {
        return Decision::Waiting(Wait {
            reason: WaitReason::ModelCap,
            retry_at: None,
        });
    }
    match provider_deadline(model, availability) {
        Some(until) => Decision::Waiting(Wait {
            reason: WaitReason::ProviderCooldown,
            retry_at: Some(until),
        }),
        None => Decision::Ready(()),
    }
}

/// Whether `model`'s cap in this pool is spent: its recorded occupancy plus
/// the pool's unresolved claims — counted conservatively against every
/// capped model — reaches the cap. An absent cap is no model limit at all;
/// a model capped at zero is spent with nothing running.
fn cap_blocks(config: &PoolConfig, model: &str, availability: &Availability) -> bool {
    let Some(cap) = config.caps.get(model) else {
        return false;
    };
    let occupancy = availability
        .model_counts
        .get(model)
        .copied()
        .unwrap_or_default()
        .saturating_add(availability.unresolved_claims);
    occupancy >= u64::from(*cap)
}

/// The cooldown deadline still running for `model`'s provider, if any: a
/// deadline at or before the snapshot's `now` has expired and blocks
/// nothing.
fn provider_deadline(model: &str, availability: &Availability) -> Option<u64> {
    availability
        .provider_until
        .get(cooldown::provider_key(model))
        .filter(|until| **until > availability.now)
        .copied()
}

/// The wait a served band owes when none of its choices are free: a
/// model-cap wait when only spent caps block (capacity is released by the
/// pool's own work finishing, which no snapshot can name a time for), a
/// provider-cooldown wait ending at the earliest deadline still running
/// when only cooldowns block, and a band-unavailable wait when the two
/// mix — including one choice doubly blocked by both at once.
fn blocked_wait(config: &PoolConfig, list: &[BandChoice], availability: &Availability) -> Wait {
    let mut capped = false;
    let mut cooling: Vec<u64> = Vec::new();
    for entry in list {
        capped |= cap_blocks(config, &entry.model, availability);
        if let Some(until) = provider_deadline(&entry.model, availability) {
            cooling.push(until);
        }
    }
    match (capped, cooling.is_empty()) {
        (true, true) => Wait {
            reason: WaitReason::ModelCap,
            retry_at: None,
        },
        (false, false) => Wait {
            reason: WaitReason::ProviderCooldown,
            retry_at: cooling.iter().min().copied(),
        },
        _ => Wait {
            reason: WaitReason::BandUnavailable,
            retry_at: None,
        },
    }
}

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;
