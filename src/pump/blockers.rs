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
    /// Dependent needs whose units are held, the root's own need excluded.
    pub dependents: usize,
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

/// The `blocking-stuck` exceptions: roots that are not moving (a stuck unit,
/// or a need parked on a verdict) and hold at least
/// `alarm.blocking_hold_units` units. They go through the same once-per-hour
/// raised machinery as the stall alarms, so a persisting block repeats.
/// `pub(crate)` because [`crate::pump::alarm::Finding`] is crate-private.
#[must_use]
pub(crate) fn findings(ctx: &Ctx, st: &State, now: u64) -> Vec<crate::pump::alarm::Finding> {
    let threshold = ctx.cfg.alarm.blocking_hold_units;
    compute(ctx, st)
        .into_iter()
        .filter(|blocker| blocker.held >= threshold && unmoving(blocker))
        .map(|blocker| finding(ctx, now, &blocker))
        .collect()
}

/// The automatic response to a blocking-stuck root: raise the blocking
/// need's dispatch priority, requeue a stuck unit with transient-class
/// failures once, and otherwise send the unit to a fresh planner (re-plan
/// or split) after `alarm.blocking_replan_minutes` stuck. Parked-verdict
/// roots get no action here (the recurring blocking-stuck exception is
/// their alert); pending starved roots surface in status only.
pub fn check(ctx: &Ctx, st: &mut State) {
    for blocker in compute(ctx, st) {
        if blocker.state != "stuck" || blocker.held < ctx.cfg.alarm.blocking_hold_units {
            continue;
        }
        if !bump(ctx, &blocker) {
            continue;
        }
        respond(ctx, st, &blocker);
    }
}

/// True when the unit's last attempt is one of the conservative timeout
/// classes: a `timeout` or `finished/agent exit 124` attempt, or one whose
/// detail died with exit 124. Timing flakes exit 1 and stay on the re-plan
/// path.
fn transient_failure(unit: &Unit) -> bool {
    unit.history.last().is_some_and(|attempt| {
        attempt.why == "timeout"
            || attempt.why == "finished/agent exit 124"
            || attempt.detail.contains("failed (exit 124)")
    })
}

/// Set the blocker's need to priority 1 when it carries no level yet.
/// Returns whether the response continues for this blocker.
fn bump(ctx: &Ctx, blocker: &Blocker) -> bool {
    if crate::pump::priority::levels(ctx).contains_key(&blocker.need) {
        return true;
    }
    match crate::pump::priority::set(&ctx.home, &blocker.need, 1) {
        Ok(()) => {
            ctx.log(&format!(
                "blockers: priority 1 on {} (holds {} units)",
                blocker.need, blocker.held
            ));
            true
        }
        Err(error) => {
            ctx.log(&format!(
                "blockers: priority set failed on {}: {error}",
                blocker.need
            ));
            false
        }
    }
}

/// Requeue or re-plan the blocker need's stuck units: a transient-class
/// unit goes back to `pending` once, an old one goes to the resplit
/// planner (which refuses quarantine repair units via their base).
fn respond(ctx: &Ctx, st: &mut State, blocker: &Blocker) {
    let stuck: Vec<String> = st
        .units
        .iter()
        .filter(|(_, unit)| unit.status == Status::Stuck && unit.need == blocker.need)
        .map(|(uid, _)| uid.clone())
        .collect();
    for uid in stuck {
        let Some(unit) = st.units.get(&uid) else {
            continue;
        };
        let requeue = !unit.requeued && transient_failure(unit);
        let due =
            ctx.now().saturating_sub(unit.since) >= ctx.cfg.alarm.blocking_replan_minutes * 60;
        let text = unit
            .history
            .last()
            .map_or_else(String::new, |attempt| {
                format!("{}: {}", attempt.why, attempt.detail)
            });
        if requeue {
            if let Some(unit) = st.units.get_mut(&uid) {
                unit.requeued = true;
                unit.status = Status::Pending;
            }
            ctx.log(&format!(
                "blockers: requeued {uid} once (transient-class failures)"
            ));
        } else if due {
            crate::pump::collect::split(ctx, st, &uid, &text);
        }
    }
}

/// The `status.txt` lines for the largest holds: up to three blockers, each as
/// `blocker <need> <state> age <age> held <n>`. Empty when nothing is held.
#[must_use]
pub fn status_lines(ctx: &Ctx, st: &State) -> Vec<String> {
    let now = ctx.now();
    compute(ctx, st)
        .iter()
        .filter(|blocker| blocker.held >= 1)
        .take(3)
        .map(|blocker| {
            format!(
                "blocker {} {} age {} held {}",
                blocker.need,
                blocker.state,
                crate::pump::alarm::age(age_at(ctx, now, blocker)),
                blocker.held
            )
        })
        .collect()
}

/// True for a root that is stuck or parked on a verdict.
fn unmoving(blocker: &Blocker) -> bool {
    blocker.state == "stuck" || board::PARKED.contains(&blocker.state.as_str())
}

/// One blocker as the exception the alarm layer files.
fn finding(ctx: &Ctx, now: u64, blocker: &Blocker) -> crate::pump::alarm::Finding {
    crate::pump::alarm::Finding {
        kind: "blocking-stuck",
        unit: blocker.root.clone(),
        text: finding_text(ctx, now, blocker),
    }
}

/// The blocker's age read at `now`: `age_secs` was measured when the blocker
/// was computed, so its timestamp is recovered first.
fn age_at(ctx: &Ctx, now: u64, blocker: &Blocker) -> u64 {
    let since = ctx.now().saturating_sub(blocker.age_secs);
    now.saturating_sub(since)
}

/// `<state> for <age>; holds <n> units across <k> needs; top held: ...`.
fn finding_text(ctx: &Ctx, now: u64, blocker: &Blocker) -> String {
    let age = crate::pump::alarm::age(age_at(ctx, now, blocker));
    let needs = if blocker.dependents == 1 {
        "need"
    } else {
        "needs"
    };
    format!(
        "{} for {age}; holds {} units across {} {needs}; top held: {}",
        blocker.state,
        blocker.held,
        blocker.dependents,
        top_text(&blocker.top_needs)
    )
}

/// `m1 (4), m2 (2)` from the top held needs, `none` when there are none.
fn top_text(top: &[(String, usize)]) -> String {
    if top.is_empty() {
        return String::from("none");
    }
    top.iter()
        .map(|(need, held)| format!("{need} ({held})"))
        .collect::<Vec<String>>()
        .join(", ")
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
        dependents: dependent_needs(&root.need, &held),
        top_needs: top_needs(&root.need, &held),
    }
}

/// How many distinct needs other than the root have units in the hold.
fn dependent_needs(root: &str, held: &[(&str, &Unit)]) -> usize {
    held.iter()
        .map(|(_, unit)| unit.need.as_str())
        .filter(|need| *need != root)
        .collect::<BTreeSet<&str>>()
        .len()
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
