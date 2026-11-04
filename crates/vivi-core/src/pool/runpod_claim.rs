//! Who gets a slot when two dispatchers race, on a host with no labels, no
//! leases and no way to make a name unique.
//!
//! A dispatcher lists the pool's pods, picks the lowest unused slot index and
//! makes a pod named for it. Two dispatchers can pick the same index (or both
//! see room for one more pod) because neither pod existed when the other
//! looked. So after the pod exists, and again just before the job starts, each
//! dispatcher looks again and applies one rule to what it sees:
//!
//! - Pods are ordered by the provider's own creation time (then by id).
//! - A pod keeps its slot only if no pod made before it holds the same index.
//! - A pod stays only if fewer than `slots` pods were made before it.
//! - A pod that cannot be ordered against a pod it competes with gives way.
//!
//! The order is the provider's, not either dispatcher's clock, so a pod made
//! later always sees the one made earlier once that one is listed, and gives
//! way to it. The rule is a function of the list alone, so any two
//! dispatchers that see both pods agree who won. What it cannot do is show a
//! pod the listing does not show yet: a pod made earlier but not yet listed
//! when the later one looks. The second look, taken after the seed has run
//! (minutes later), is what closes that gap in practice. The consequence of
//! the rare miss is one pod over the cap for the minutes until `reconcile` or
//! `doctor` finds it; two jobs never share a machine, because every dispatch
//! has a pod of its own.

use super::runpod_name::PodName;

/// A pod of the pool as the rule sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub index: u32,
    /// The provider's creation time (ms), when it said.
    pub created_ms: Option<i64>,
}

impl Entry {
    #[must_use]
    pub fn of(id: &str, name: &PodName, created_ms: Option<i64>) -> Self {
        Self {
            id: id.to_string(),
            index: name.index,
            created_ms,
        }
    }
}

/// What a dispatcher does with the pod it made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Keep it: it holds its index and is within the cap.
    Keep,
    /// An earlier pod holds the same slot index.
    IndexTaken { by: String },
    /// `slots` or more pods were made before it.
    OverCap { before: usize },
    /// It cannot be ordered against a pod it competes with.
    Unordered { with: String },
}

impl Verdict {
    /// Why the pod must go, in words; `None` when it stays.
    #[must_use]
    pub fn reason(&self, index: u32, slots: u32) -> Option<String> {
        match self {
            Self::Keep => None,
            Self::IndexTaken { by } => Some(format!(
                "another dispatch took slot {index} first (pod {by})"
            )),
            Self::OverCap { before } => Some(format!(
                "{before} pod(s) were made before it and the pool allows {slots}"
            )),
            Self::Unordered { with } => Some(format!(
                "it cannot be ordered against pod {with} (the provider gave no creation time)"
            )),
        }
    }
}

/// True when `other` was made before `mine`: `Some(true/false)`, or `None`
/// when either has no creation time.
fn before(other: &Entry, mine: &Entry) -> Option<bool> {
    let (a, b) = (other.created_ms?, mine.created_ms?);
    Some((a, other.id.as_str()) < (b, mine.id.as_str()))
}

/// The rule, for the pod `mine` among `pods` (which may include `mine`).
#[must_use]
pub fn verdict(mine: &Entry, pods: &[Entry], slots: u32) -> Verdict {
    let others: Vec<&Entry> = pods.iter().filter(|p| p.id != mine.id).collect();
    let mut earlier = 0_usize;
    for other in &others {
        match before(other, mine) {
            Some(true) => {
                earlier += 1;
                if other.index == mine.index {
                    return Verdict::IndexTaken {
                        by: other.id.clone(),
                    };
                }
            }
            Some(false) => {}
            None => {
                // Unorderable pods matter only when they could win something:
                // the same index, or a pool that is already full without them.
                let full = others.len() >= slots as usize;
                if other.index == mine.index || full {
                    return Verdict::Unordered {
                        with: other.id.clone(),
                    };
                }
            }
        }
    }
    if earlier >= slots as usize {
        return Verdict::OverCap { before: earlier };
    }
    Verdict::Keep
}

/// The lowest slot index from 1 that no pod in `used` holds.
#[must_use]
pub fn lowest_free(used: &[u32]) -> u32 {
    (1..=u32::MAX)
        .find(|i| !used.contains(i))
        .unwrap_or(u32::MAX)
}

#[cfg(test)]
#[path = "runpod_claim_test.rs"]
mod tests;
