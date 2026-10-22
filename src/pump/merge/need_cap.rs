//! Per-target cap on concurrently active need-merge jobs.
//!
//! Every need-merge job runs the shared checks (a full build and test), so
//! starting one per ready need at once can overload the worker pool. The cap
//! counts only those jobs. Need branch init and landing are other job kinds
//! and are cheap git operations, so they never take a slot.

use std::collections::{BTreeMap, VecDeque};

use crate::pump::config::PumpTarget;
use crate::pump::host::Ctx;
use crate::pump::state::State;

use super::need_dispatch::is_need_merge_job;

type Group = ((String, String), VecDeque<String>);

struct Slot {
    active: usize,
    cap: usize,
    waiting: usize,
}

/// Free need-merge slots per target for one dispatch tick.
pub(super) struct NeedSlots {
    targets: BTreeMap<String, Slot>,
}

impl NeedSlots {
    /// Count each target's need-merge jobs that are still queued or running.
    /// A finished job whose receipt is not yet applied has stopped running
    /// its checks and no longer holds a slot.
    pub(super) fn new(st: &State, targets: &BTreeMap<String, PumpTarget>) -> Self {
        let targets = targets
            .iter()
            .map(|(id, target)| {
                let cap = target
                    .merge
                    .as_ref()
                    .map_or(4, |merge| merge.need_parallel as usize)
                    .max(1);
                let active = st
                    .jobs
                    .values()
                    .filter(|job| {
                        is_need_merge_job(job)
                            && job.target.as_deref() == Some(id.as_str())
                            && !job.status.is_terminal()
                    })
                    .count();
                (
                    id.clone(),
                    Slot {
                        active,
                        cap,
                        waiting: 0,
                    },
                )
            })
            .collect();
        Self { targets }
    }

    /// Whether `target` has a free slot; a group refused here counts as waiting.
    pub(super) fn has_room(&mut self, target: &str) -> bool {
        let Some(slot) = self.targets.get_mut(target) else {
            return true;
        };
        if slot.active < slot.cap {
            return true;
        }
        slot.waiting += 1;
        false
    }

    /// Record that a need-merge job was queued for `target`.
    pub(super) fn started(&mut self, target: &str) {
        if let Some(slot) = self.targets.get_mut(target) {
            slot.active += 1;
        }
    }

    /// Log once per target whose cap deferred groups this tick.
    pub(super) fn log_deferred(&self, ctx: &Ctx) {
        for (target, slot) in &self.targets {
            if slot.waiting > 0 {
                ctx.log(&format!(
                    "need-merge cap {target}: {}/{} active, {} waiting",
                    slot.active, slot.cap, slot.waiting
                ));
            }
        }
    }
}

/// Order need groups by the unit ranking (fix units first, then higher need
/// priority levels, then the rest; stable), so the groups that reach a free
/// slot first are the ones that rank highest.
pub(super) fn rank_groups(ctx: &Ctx, st: &State, mut grouped: Vec<Group>) -> Vec<Group> {
    let all: Vec<String> = grouped
        .iter()
        .flat_map(|(_, units)| units.iter().cloned())
        .collect();
    let order: BTreeMap<String, usize> = crate::pump::ready::ranked(ctx, st, all)
        .into_iter()
        .enumerate()
        .map(|(index, uid)| (uid, index))
        .collect();
    grouped.sort_by_key(|(_, units)| {
        units
            .iter()
            .filter_map(|uid| order.get(uid))
            .min()
            .copied()
            .unwrap_or(usize::MAX)
    });
    grouped
}
