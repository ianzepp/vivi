//! `vivi pool scale`: bring a pool to a number of slots.
//!
//! Going up creates and seeds what is missing, like `create --slots N`. Going
//! down destroys free slots, highest index first, and only free ones: a slot
//! that is busy, seeding, finished but uncollected, or unseeded is never
//! destroyed here, and is reported as skipped.

use super::slot::SlotStatus;
use crate::error::VivariumError;

/// What a scale would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalePlan {
    /// The slots the pool holds now.
    pub have: usize,
    /// How many slots to create (their indices come from `create`).
    pub create: usize,
    /// Free slots to destroy, highest index first.
    pub destroy: Vec<SlotStatus>,
    /// Slots that would have been destroyed but are not free, highest index
    /// first.
    pub skipped: Vec<SlotStatus>,
}

impl ScalePlan {
    /// The slot count the pool ends with if the plan runs as made.
    #[must_use]
    pub fn ends_with(&self) -> usize {
        self.have + self.create - self.destroy.len()
    }
}

/// The error for `create` and a scale-up on a host whose slots are made per
/// dispatch.
#[must_use]
pub fn no_resting_slots(pool: &str, provider: &str) -> VivariumError {
    VivariumError::Message(format!(
        "pool {pool} uses the {provider} provider: its slots are made per dispatch, so there are none to create, seed or scale up (`slots` is the most that may exist at once). Run `vivi pool dispatch --pool {pool} <task>`; `vivi pool scale` and `vivi pool destroy` only remove what exists"
    ))
}

/// Refuse a target above the pool's configured maximum.
///
/// # Errors
/// Returns an error when `target` is above `max`.
pub fn check_target(target: u32, max: u32) -> Result<(), VivariumError> {
    if target > max {
        return Err(VivariumError::Message(format!(
            "cannot scale to {target}: the pool is configured for at most {max} slot(s) (`slots` in its table)"
        )));
    }
    Ok(())
}

/// Plan the move of a pool holding `slots` to `target` slots.
///
/// # Errors
/// Returns an error when `target` is above the configured maximum `max`.
pub fn plan(slots: &[SlotStatus], target: u32, max: u32) -> Result<ScalePlan, VivariumError> {
    plan_with(slots, target, max, |slot| slot.state == "free")
}

/// Like [`plan`], with `removable` deciding which slots may be destroyed
/// (a per-dispatch host has no free slot: a slot with no open dispatch is the
/// one that may go).
///
/// # Errors
/// Returns an error when `target` is above the configured maximum `max`.
pub fn plan_with(
    slots: &[SlotStatus],
    target: u32,
    max: u32,
    removable: impl Fn(&SlotStatus) -> bool,
) -> Result<ScalePlan, VivariumError> {
    check_target(target, max)?;
    let want = target as usize;
    let have = slots.len();
    let mut plan = ScalePlan {
        have,
        create: want.saturating_sub(have),
        destroy: Vec::new(),
        skipped: Vec::new(),
    };
    let mut excess = have.saturating_sub(want);
    let mut highest_first: Vec<&SlotStatus> = slots.iter().collect();
    highest_first.sort_by(|a, b| {
        b.index
            .unwrap_or(0)
            .cmp(&a.index.unwrap_or(0))
            .then_with(|| b.name.cmp(&a.name))
    });
    for slot in highest_first {
        if excess == 0 {
            break;
        }
        if removable(slot) {
            plan.destroy.push(slot.clone());
            excess -= 1;
        } else {
            plan.skipped.push(slot.clone());
        }
    }
    Ok(plan)
}

#[cfg(test)]
#[path = "scale_test.rs"]
mod tests;
