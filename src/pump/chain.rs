//! Continuation chains.
//!
//! A worker that cannot finish its whole change reports `VERDICT: blocked`.
//! The pump then continues the work from that unit's own branch through
//! follow-up units that form one chain: each unit starts from its
//! predecessor's branch (`Unit::continues`), and only the last unit of the
//! chain is merged, once, with the whole chain in its branch. These helpers
//! hold the chain rules the other pump modules share.

use crate::pump::state::{State, Status, Unit};

/// Longest `continues` walk; a loop in a hand-edited state ends here.
const MAX_WALK: usize = 64;

/// The units that start from `uid`'s branch, in creation order.
#[must_use]
pub fn continuers(st: &State, uid: &str) -> Vec<String> {
    st.order
        .iter()
        .filter(|id| {
            st.units
                .get(*id)
                .is_some_and(|u| u.continues.as_deref() == Some(uid))
        })
        .cloned()
        .collect()
}

/// The chain a unit continues, nearest first: its predecessor, that unit's
/// predecessor, and so on.
#[must_use]
pub fn ancestors<'a>(st: &'a State, unit: &'a Unit) -> Vec<&'a str> {
    let mut found = Vec::new();
    let mut next = unit.continues.as_deref();
    while let Some(uid) = next {
        if found.len() >= MAX_WALK || found.contains(&uid) {
            break;
        }
        found.push(uid);
        next = st.units.get(uid).and_then(|u| u.continues.as_deref());
    }
    found
}

/// True when the unit may be dispatched as far as its start point goes: it
/// continues nothing, or its predecessor has delivered a branch (it is
/// `delivered`, being merged, merged, or already handed on to a unit that
/// continues from it).
#[must_use]
pub fn base_ready(st: &State, unit: &Unit) -> bool {
    let Some(pred) = unit.continues.as_deref() else {
        return true;
    };
    st.units.get(pred).is_some_and(|p| {
        p.branch.is_some()
            && matches!(
                p.status,
                Status::Delivered | Status::Merging | Status::Merged | Status::Replaced
            )
    })
}

/// The branch a unit starts from when it continues another unit.
#[must_use]
pub fn base_branch<'a>(st: &'a State, unit: &Unit) -> Option<&'a str> {
    let pred = unit.continues.as_deref()?;
    st.units.get(pred)?.branch.as_deref()
}

/// The write scope a merge of `uid` covers: its own plus that of every unit
/// in the chain before it, since the merged branch holds all of that work.
#[must_use]
pub fn chain_scope(st: &State, uid: &str) -> Vec<String> {
    let Some(unit) = st.units.get(uid) else {
        return Vec::new();
    };
    let mut scope = unit.write_scope.clone();
    for before in ancestors(st, unit) {
        if let Some(prev) = st.units.get(before) {
            scope.extend(prev.write_scope.iter().cloned());
        }
    }
    scope.sort();
    scope.dedup();
    scope
}

/// Where the replacements of `replaces` start, when they must form a chain:
/// `Some(base)` with `base` the unit they continue from (`None` is the
/// integration branch), or `None` for an ordinary split of independent units.
///
/// They chain when the replaced unit pushed partial work before it reported
/// `blocked` (they continue from its branch), or when it is itself part of a
/// chain (they continue from its own start point, and whatever continued from
/// it moves to the end of the new chain).
#[must_use]
pub fn chain_base(st: &State, replaces: &str) -> Option<Option<String>> {
    let old = st.units.get(replaces)?;
    if old.blocked_branch.is_some() {
        return Some(Some(replaces.to_owned()));
    }
    if old.continues.is_some() || !continuers(st, replaces).is_empty() {
        return Some(old.continues.clone());
    }
    None
}

/// Wire freshly minted `ids` (a chained split of `replaces`, see
/// [`chain_base`]): each continues the one before, the first continues
/// `base`, `after` entries between the new units are dropped (the chain
/// carries that order), and every unit that continued from `replaces` now
/// continues from the end of the new chain.
pub fn link(st: &mut State, replaces: &str, base: Option<String>, ids: &[String]) {
    let end = ids
        .last()
        .cloned()
        .or_else(|| st.units.get(replaces).and_then(|u| u.continues.clone()));
    let mut prev = base;
    for id in ids {
        if let Some(unit) = st.units.get_mut(id) {
            unit.continues.clone_from(&prev);
            unit.after.retain(|dep| !ids.contains(dep));
        }
        prev = Some(id.clone());
    }
    for (uid, unit) in &mut st.units {
        if !ids.contains(uid) && unit.continues.as_deref() == Some(replaces) {
            unit.continues.clone_from(&end);
        }
    }
}

/// A delivered unit that other units continue from is not merged: it becomes
/// `replaced` by those units, which carry its branch forward. Returns true
/// when it was handed on.
pub fn hand_on(st: &mut State, uid: &str) -> bool {
    let next = continuers(st, uid);
    if next.is_empty() {
        return false;
    }
    let Some(unit) = st.units.get_mut(uid) else {
        return false;
    };
    unit.status = Status::Replaced;
    unit.replaced_by = next;
    true
}
