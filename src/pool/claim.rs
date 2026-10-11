//! Claiming a free slot, safely against a second dispatcher.
//!
//! The claim is atomic on the provider side: a machine lease serialises the
//! check-and-mark, and the durable mark is machine metadata (`META_TASK`), so
//! it outlives the lease. Two dispatchers can both list the same slot as free;
//! only one wins the lease, and the other, whenever it gets the lease, re-reads
//! the machine, sees the mark, and moves on. A slot with a mark is never
//! claimed, whether its job is running or finished and uncollected.

use super::provider::{Lease, PoolProvider};
use super::slot::{self, META_CLAIMED, META_TASK, SlotStatus};
use crate::error::VivariumError;

/// How long a claim holds the lease while it marks and delivers.
pub const CLAIM_TTL_SECS: u32 = 120;

/// A claimed slot and the lease that protects the claim window.
#[derive(Debug, Clone)]
pub struct Claim {
    pub slot: SlotStatus,
    pub lease: Lease,
}

/// Claim one free slot for `task`.
///
/// # Errors
/// Returns an error if the pool has no claimable slot (naming what each slot
/// is doing), or the provider fails mid-claim; a half-made claim is undone.
pub fn claim_slot(
    provider: &dyn PoolProvider,
    pool: &str,
    task: &str,
    now: u64,
) -> Result<Claim, VivariumError> {
    let slots = slot::pool_slots(pool, &provider.list()?);
    for candidate in slots.iter().filter(|s| s.state == "free") {
        let Some(lease) = provider.lease(&candidate.id, CLAIM_TTL_SECS)? else {
            continue;
        };
        // The list may be stale: re-read under the lease before marking.
        let fresh = provider.machine(&candidate.id);
        if !fresh.as_ref().is_ok_and(slot::is_free) {
            provider.release(&candidate.id, &lease)?;
            fresh?;
            continue;
        }
        let marked = provider
            .set_metadata(&candidate.id, META_TASK, task)
            .and_then(|()| provider.set_metadata(&candidate.id, META_CLAIMED, &now.to_string()));
        if let Err(e) = marked {
            let claim = Claim {
                slot: candidate.clone(),
                lease,
            };
            if let Err(undo) = unclaim(provider, &claim) {
                eprintln!("warning: could not fully undo the claim: {undo}");
            }
            return Err(e);
        }
        return Ok(Claim {
            slot: candidate.clone(),
            lease,
        });
    }
    Err(no_free_slot(pool, &slots))
}

/// Undo a claim: drop the marks and the lease. Every step is attempted; a
/// failure leaves a mark that `reconcile` clears as a stale claim.
///
/// # Errors
/// Returns the first error any step hit.
pub fn unclaim(provider: &dyn PoolProvider, claim: &Claim) -> Result<(), VivariumError> {
    let id = &claim.slot.id;
    let steps = [
        provider.clear_metadata(id, META_TASK),
        provider.clear_metadata(id, META_CLAIMED),
        provider.release(id, &claim.lease),
    ];
    steps.into_iter().collect()
}

fn no_free_slot(pool: &str, slots: &[SlotStatus]) -> VivariumError {
    if slots.is_empty() {
        return VivariumError::Message(format!("pool {pool} has no slots; run `vivi pool create`"));
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
