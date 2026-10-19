//! Blockers: the needs whose stall holds other needs' units back, and how
//! many units each of them holds.
//!
//! A blocker is a need that is itself not moving — a stuck unit, a verdict
//! parked on an operator ruling, or a need starved of dispatch. The dependency
//! graph from `vivi graph blocked` turns that into a hold count: every unit of
//! the needs that transitively depend on it, minus the ones already satisfied.

use std::collections::{BTreeMap, BTreeSet};

use crate::pump::board;
use crate::pump::host::Ctx;
use crate::pump::ready::{dependency_holds, satisfied};
use crate::pump::state::{NeedRecord, State, Status, Unit};

/// One blocking root: what is stuck, where, how old, and how much it holds.
#[derive(Debug, Clone)]
pub struct Blocker {
    /// The root: a stuck unit's uid, or the parked or pending need handle.
    pub root: String,
    /// The need the root belongs to.
    pub need: String,
    /// `stuck`, the parked verdict string (`operator`, `design`, `hold`), or
    /// `pending`.
    pub state: String,
    /// Epoch seconds since the oldest held unit was created.
    pub age_secs: u64,
    /// Units held, directly or through the dependency closure.
    pub held: usize,
    /// Up to three dependent needs, by held-unit count, largest first.
    pub top_needs: Vec<(String, usize)>,
}

/// One root candidate before its closure and hold count are known.
struct Root {
    need: String,
    state: String,
    root: String,
}

/// Every blocking root over the current state, largest hold first, ties by
/// need handle.
#[must_use]
pub fn compute(ctx: &Ctx, st: &State) -> Vec<Blocker> {
    let reverse = reverse_holds(&dependency_holds(ctx, st));
    let now = ctx.now();
    let mut out: Vec<Blocker> = roots(st, &reverse)
        .iter()
        .map(|root| blocker(st, root, &reverse, now))
        .collect();
    out.sort_by(|a, b| b.held.cmp(&a.held).then_with(|| a.need.cmp(&b.need)));
    out
}

/// The dependency map turned around: `dependency -> needs depending on it`.
fn reverse_holds(holds: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, BTreeSet<String>> {
    let mut reverse: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (need, dependencies) in holds {
        for dependency in dependencies {
            reverse
                .entry(dependency.clone())
                .or_default()
                .insert(need.clone());
        }
    }
    reverse
}

/// Build one blocker from its root: the dependency closure, its held units,
/// the oldest of them, and the needs under the root that hold the most.
fn blocker(
    st: &State,
    root: &Root,
    reverse: &BTreeMap<String, BTreeSet<String>>,
    now: u64,
) -> Blocker {
    let scope = closure(reverse, &root.need);
    let held = blocked_units(st, &scope);
    Blocker {
        root: root.root.clone(),
        need: root.need.clone(),
        state: root.state.clone(),
        age_secs: age_secs(st, root, &held, now),
        held: held.len(),
        top_needs: top_needs(&root.need, &held),
    }
}

/// The root need plus every need that transitively depends on it.
fn closure(reverse: &BTreeMap<String, BTreeSet<String>>, root: &str) -> BTreeSet<String> {
    let mut scope = BTreeSet::new();
    let mut queue = vec![root.to_string()];
    while let Some(need) = queue.pop() {
        if !scope.insert(need.clone()) {
            continue;
        }
        if let Some(dependents) = reverse.get(&need) {
            queue.extend(dependents.iter().cloned());
        }
    }
    scope
}

/// The units in `scope` that are genuinely blocked: not withdrawn, not held
/// for a missing target, and not satisfied.
fn blocked_units<'a>(st: &'a State, scope: &BTreeSet<String>) -> Vec<(&'a str, &'a Unit)> {
    let mut out = Vec::new();
    for (uid, unit) in &st.units {
        if !scope.contains(&unit.need) {
            continue;
        }
        if matches!(unit.status, Status::Withdrawn | Status::Held) {
            continue;
        }
        if satisfied(st, uid, 0) {
            continue;
        }
        out.push((uid.as_str(), unit));
    }
    out.sort_by(|a, b| a.0.cmp(b.0));
    out
}

/// Age of the oldest held unit, or of the root's own stuck units when the
/// hold count came up empty.
fn age_secs(st: &State, root: &Root, held: &[(&str, &Unit)], now: u64) -> u64 {
    let oldest = held
        .iter()
        .map(|(_, unit)| unit.since)
        .chain(stuck_units(st, &root.need).map(|unit| unit.since))
        .min()
        .unwrap_or(now);
    now.saturating_sub(oldest)
}

/// The units of `need` in `Status::Stuck`.
fn stuck_units<'a>(st: &'a State, need: &'a str) -> impl Iterator<Item = &'a Unit> + 'a {
    st.units
        .values()
        .filter(move |unit| unit.status == Status::Stuck && unit.need == need)
}

/// Up to three dependent needs by their held-unit count, largest first.
fn top_needs(root: &str, held: &[(&str, &Unit)]) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (_, unit) in held {
        if unit.need == root {
            continue;
        }
        *counts.entry(unit.need.clone()).or_insert(0) += 1;
    }
    let mut pairs: Vec<(String, usize)> = counts.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs.truncate(3);
    pairs
}

/// One root per need, in priority order: a stuck unit first, then a parked
/// verdict that other needs wait on, then a parked-free need starved of
/// dispatch that other needs wait on.
fn roots(st: &State, reverse: &BTreeMap<String, BTreeSet<String>>) -> Vec<Root> {
    let mut out = Vec::new();
    for need in needs(st) {
        if let Some(root) = root_of_need(st, &need, reverse) {
            out.push(root);
        }
    }
    out
}

/// Every need handle the state knows about: from its units and its plan
/// records.
fn needs(st: &State) -> BTreeSet<String> {
    st.units
        .values()
        .map(|unit| unit.need.clone())
        .chain(st.needs.keys().cloned())
        .collect()
}

/// The root a single need contributes, when it contributes one.
fn root_of_need(
    st: &State,
    need: &str,
    reverse: &BTreeMap<String, BTreeSet<String>>,
) -> Option<Root> {
    if let Some(oldest) = oldest_stuck(st, need) {
        return Some(Root {
            need: need.to_string(),
            state: String::from("stuck"),
            root: oldest,
        });
    }
    if !reverse.contains_key(need) {
        return None;
    }
    let record = st.needs.get(need);
    let state = parked(record).or_else(|| starved(st, need))?;
    Some(Root {
        need: need.to_string(),
        state,
        root: need.to_string(),
    })
}

/// The uid of the need's oldest stuck unit.
fn oldest_stuck(st: &State, need: &str) -> Option<String> {
    st.units
        .iter()
        .filter(|(_, unit)| unit.status == Status::Stuck && unit.need == need)
        .min_by_key(|(_, unit)| unit.since)
        .map(|(uid, _)| uid.clone())
}

/// The need's parked verdict, when it is parked.
fn parked(record: Option<&NeedRecord>) -> Option<String> {
    let verdict = record.map(|record| record.verdict.as_str()).unwrap_or("");
    board::PARKED
        .contains(&verdict)
        .then(|| verdict.to_string())
}

/// `pending` when the need still has a pending unit.
fn starved(st: &State, need: &str) -> Option<String> {
    st.units
        .values()
        .any(|unit| unit.need == need && unit.status == Status::Pending)
        .then(|| String::from("pending"))
}
