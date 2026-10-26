//! Availability snapshots over the runtime vocabulary: the model capacity
//! one pool's durable dispatch record and live machines answer for at one
//! instant.
//!
//! A snapshot folds the mailspace's durable pool events into open
//! dispatches ([`crate::pool::record::fold_open`]) and reads the live
//! machine listing for claimed slots no record explains. Open dispatches
//! are counted by exact pool and model slug, so reasoning variants and
//! bands share one model cap; target and repository never change the
//! count. A claimed slot with no open dispatch behind it (an orphan
//! claim) is charged conservatively to every capped model of the pool,
//! since no record says which model it ran; its recorded age never
//! expires it, because reconcile owns cleanup.
//!
//! Provider cooldown folding is the dependent slice's hook point: this
//! module answers capacity only, never provider health.

use crate::mailspace::PoolEventRow;
use crate::pool::provider::MachineRecord;
use crate::pool::record::{OpenDispatch, fold_open};
use crate::pool::slot::{PoolId, pool_slots};

/// Point-in-time model capacity of one pool: its open dispatches and
/// unrecorded claims, read at one instant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Availability {
    /// The injected instant of the snapshot, epoch seconds.
    pub now: u64,
    /// Open dispatches recorded for this exact pool, oldest first. A
    /// finished-but-uncollected job is still open here.
    pub open: Vec<OpenDispatch>,
    /// Claimed slots of this pool with no matching open dispatch, by
    /// machine id.
    pub orphan_claims: Vec<String>,
}

impl Availability {
    /// The capacity of pool `id` at instant `now`: fold the durable pool
    /// `events` into open dispatches of this exact pool, and read the live
    /// `machines` for claimed slots no open dispatch explains. Machines of
    /// another mailspace or pool are never read.
    #[must_use]
    pub fn at(now: u64, events: &[PoolEventRow], id: &PoolId, machines: &[MachineRecord]) -> Self {
        let open: Vec<OpenDispatch> = fold_open(events)
            .into_iter()
            .filter(|d| d.record.pool == id.pool)
            .collect();
        let orphan_claims = pool_slots(id, machines)
            .into_iter()
            .filter(|slot| slot.task.is_some())
            .filter(|slot| !open.iter().any(|d| d.record.machine == slot.id))
            .map(|slot| slot.id)
            .collect();
        Self {
            now,
            open,
            orphan_claims,
        }
    }

    /// Open dispatches of this pool on `model` exactly. Reasoning variants
    /// and bands aggregate: a dispatch record carries the model slug alone.
    #[must_use]
    pub fn usage(&self, model: &str) -> usize {
        self.open.iter().filter(|d| d.record.model == model).count()
    }

    /// Whether `model` may take another dispatch in this pool under `cap`:
    /// an absent cap imposes no limit, and a zero cap always refuses. An
    /// orphan claim is charged to every capped model, so a capped model's
    /// usage is its recorded usage plus every unrecorded claim.
    #[must_use]
    pub fn admits(&self, model: &str, cap: Option<u32>) -> bool {
        let Some(cap) = cap else {
            return true;
        };
        self.usage(model) + self.orphan_claims.len() < cap as usize
    }
}

#[cfg(test)]
#[path = "availability_test.rs"]
mod tests;
