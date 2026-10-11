//! Claiming a free slot, safely against a second dispatcher.
//!
//! The claim is atomic on the provider side ([`PoolProvider::claim`]): the
//! provider's own exclusive primitive serialises the check-and-mark, and the
//! durable mark (`META_TASK`, `META_CLAIMED`) outlives the claim window. Two
//! dispatchers can both list the same slot as free; only one wins the claim,
//! and the other moves on. A slot with a mark is never claimed, whether its
//! job is running or finished and uncollected.

use super::provider::{Hold, OpenRequest, PoolProvider, SlotLifetime};
use super::slot::{self, PoolId, SlotStatus};
use crate::error::VivariumError;

/// How long a lease-based provider holds the claim window while it marks and
/// delivers.
pub const CLAIM_TTL_SECS: u32 = 120;

/// A claimed slot and the hold that protects the claim window.
#[derive(Debug, Clone)]
pub struct Claim {
    pub slot: SlotStatus,
    pub hold: Hold,
    /// What the host gave the slot (the hardware of a rented pod); empty for a
    /// resting slot.
    pub hardware: String,
}

/// Claim one free slot for `task`.
///
/// # Errors
/// Returns an error if the pool has no claimable slot (naming what each slot
/// is doing), or the provider fails mid-claim; a half-made claim is undone.
pub fn claim_slot(
    provider: &dyn PoolProvider,
    id: &PoolId,
    task: &str,
    now: u64,
) -> Result<Claim, VivariumError> {
    let slots = slot::pool_slots(id, &provider.list()?);
    for candidate in slots.iter().filter(|s| s.state == "free") {
        if let Some(hold) = provider.claim(&candidate.id, task, now)? {
            return Ok(Claim {
                slot: candidate.clone(),
                hold,
                hardware: String::new(),
            });
        }
    }
    Err(no_free_slot(&id.pool, &slots))
}

/// Make the slot of one job on a host whose slots live per dispatch, and claim
/// it. The host made, seeded and claimed it; nothing is left behind on error.
///
/// # Errors
/// Returns the host's reason no slot could be made.
pub fn open_slot(
    provider: &dyn PoolProvider,
    request: &OpenRequest,
) -> Result<Claim, VivariumError> {
    let opened = provider.open_for_dispatch(request)?;
    Ok(Claim {
        slot: SlotStatus {
            pool: request.pool.pool.clone(),
            name: opened.name,
            id: opened.id,
            state: "busy".to_string(),
            provider_state: "started".to_string(),
            index: Some(opened.index),
            base: None,
            task: Some(request.task.to_string()),
            claimed_at: Some(request.now),
            running_secs: None,
            budget_secs: None,
            read_only: None,
            created_at: opened.created_at,
            region: String::new(),
            arch: None,
        },
        hold: opened.hold,
        hardware: opened.hardware,
    })
}

/// Undo a claim: drop the marks and end the claim window. Every step is attempted; a
/// failure leaves a mark that `reconcile` clears as a stale claim. A slot that
/// lives per dispatch is destroyed.
///
/// # Errors
/// Returns the first error any step hit.
pub fn unclaim(provider: &dyn PoolProvider, claim: &Claim) -> Result<(), VivariumError> {
    let id = &claim.slot.id;
    if provider.slot_lifetime() == SlotLifetime::PerDispatch {
        return provider.destroy(id);
    }
    let steps = [provider.unclaim(id), provider.release(id, &claim.hold)];
    steps.into_iter().collect()
}

fn no_free_slot(pool: &str, slots: &[SlotStatus]) -> VivariumError {
    if slots.is_empty() {
        return VivariumError::Message(format!(
            "pool {pool} has no slots; run `vivi pool create --pool {pool}`"
        ));
    }
    let states = slots
        .iter()
        .map(|s| match &s.task {
            Some(task) => format!("{} {} ({task})", s.name, s.state),
            None => format!("{} {}", s.name, s.state),
        })
        .collect::<Vec<_>>()
        .join(", ");
    VivariumError::Message(format!("no free slot in pool {pool}: {states}"))
}

#[cfg(test)]
#[path = "claim_test.rs"]
mod tests;
