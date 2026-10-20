//! Which delivered units may enter a merge job, and how they are batched.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::pump::host::Ctx;
use crate::pump::ready::{dependency_holds, note_hold, ranked};
use crate::pump::state::{State, Status};

/// The next batch from the front of `queue`: up to `size` units, in rank
/// order. A solo unit goes alone, and in a batch of others it waits at the
/// front of the queue for the next job.
pub(super) fn take_batch(st: &State, queue: &mut VecDeque<String>, size: usize) -> Vec<String> {
    let solo = |uid: &String| st.units.get(uid).is_some_and(|unit| unit.solo);
    let Some(first) = queue.pop_front() else {
        return Vec::new();
    };
    if solo(&first) {
        return vec![first];
    }
    let mut batch = vec![first];
    let mut skipped = Vec::new();
    while batch.len() < size {
        match queue.pop_front() {
            Some(uid) if solo(&uid) => skipped.push(uid),
            Some(uid) => batch.push(uid),
            None => break,
        }
    }
    for uid in skipped.into_iter().rev() {
        queue.push_front(uid);
    }
    batch
}

/// Held units whose repository target is configured again become
/// `delivered` so they can be taken.
pub(super) fn recheck_held(st: &mut State, targets: &BTreeMap<String, usize>) {
    for unit in st.units.values_mut() {
        if unit.status == Status::Held
            && unit.branch.is_some()
            && targets.contains_key(unit.target_id())
        {
            unit.status = Status::Delivered;
        }
    }
}

#[derive(Default)]
struct RedCrates {
    names: BTreeSet<String>,
    has_unqualified_name: bool,
}

fn red_crates(st: &State) -> RedCrates {
    let mut red = RedCrates::default();
    for name in st
        .red
        .keys()
        .chain(st.target_states.values().flat_map(|t| t.red.keys()))
    {
        let mut words = name.split_whitespace();
        if let Some(krate) = words.next() {
            if words.next().is_some() {
                red.names.insert(krate.to_owned());
            } else {
                red.has_unqualified_name = true;
            }
        }
    }
    red
}

fn clear_of_red(write_scope: &[String], red: &RedCrates) -> bool {
    !red.has_unqualified_name && !red.names.is_empty() && !touches_crate(write_scope, &red.names)
}

fn touches_crate(write_scope: &[String], red: &BTreeSet<String>) -> bool {
    write_scope
        .iter()
        .any(|path| path.split('/').any(|segment| red.contains(segment)))
}

/// The `delivered` units that may be merged now, in rank order, grouped by
/// target. Dependency holds, the `HOLD-MERGE` flag and the target check are
/// applied here; a unit naming a target no job serves is held and reported.
pub(super) fn eligible(
    ctx: &Ctx,
    st: &mut State,
    known: &BTreeSet<String>,
) -> BTreeMap<String, VecDeque<String>> {
    let Some(manually_closed) = crate::pump::board::reconcile_done_needs(ctx, st) else {
        ctx.log("merge feed paused: could not confirm done needs");
        return BTreeMap::new();
    };
    let hold = ctx.flag("HOLD-MERGE");
    let floor = ctx.cfg.gate.hold_priority_floor.max(1);
    let levels = crate::pump::priority::levels(ctx);
    let red = red_crates(st);
    let holds = dependency_holds(ctx, st);
    let mut waiting: BTreeMap<String, VecDeque<String>> = BTreeMap::new();
    for uid in ranked(ctx, st, st.order.clone()) {
        consider_unit(
            ctx,
            st,
            known,
            &manually_closed,
            hold,
            floor,
            &levels,
            &red,
            &holds,
            &mut waiting,
            uid,
        );
    }
    waiting
}

fn consider_unit(
    ctx: &Ctx,
    st: &mut State,
    known: &BTreeSet<String>,
    manually_closed: &BTreeSet<String>,
    hold: bool,
    floor: i64,
    levels: &BTreeMap<String, i64>,
    red: &RedCrates,
    holds: &BTreeMap<String, Vec<String>>,
    waiting: &mut BTreeMap<String, VecDeque<String>>,
    uid: String,
) {
    let Some(unit) = st.units.get(&uid) else {
        return;
    };
    if manually_closed.contains(&unit.need) || st.closed_needs.contains(&unit.need) {
        return;
    }
    let dependency = st
        .units
        .get(&uid)
        .and_then(|unit| holds.get(&unit.need))
        .and_then(|deps| deps.first())
        .cloned();
    if note_hold(ctx, st, &uid, Status::Delivered, dependency) {
        return;
    }
    let Some(unit) = st.units.get_mut(&uid) else {
        return;
    };
    if hold && !uid.starts_with(&ctx.cfg.fix_prefix) {
        let level = levels.get(&unit.need).copied().unwrap_or(0);
        let clear = clear_of_red(&unit.write_scope, red);
        if level < floor && !clear {
            return;
        }
    }
    if unit.status != Status::Delivered {
        return;
    }
    let target = unit.target_id().to_string();
    if !known.contains(&target) {
        unit.status = Status::Held;
        let detail = format!("repository target `{target}` is not configured");
        ctx.exception("unknown-target", &uid, &detail);
        return;
    }
    waiting.entry(target).or_default().push_back(uid);
}

#[cfg(test)]
#[path = "select_test.rs"]
mod tests;
