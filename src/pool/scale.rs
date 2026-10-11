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
        if slot.state == "free" {
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
