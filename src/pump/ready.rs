//! Unit readiness and model choice.
//!
//! `satisfied`, `reserved_paths`, `ready` and `pick_model`.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use serde::Deserialize;

use crate::pump::config::Lane;
use crate::pump::host::Ctx;
use crate::pump::plan::overlaps;
use crate::pump::priority;
use crate::pump::state::{State, Status, Unit};

/// A unit is satisfied when it merged, found no defect, or was replaced and
/// every replacement is itself satisfied (recursion depth capped at 6).
#[must_use]
pub fn satisfied(st: &State, uid: &str, depth: u32) -> bool {
    let Some(u) = st.units.get(uid) else {
        return false;
    };
    if depth > 6 {
        return false;
    }
    match u.status {
        Status::Merged | Status::NoDefect => true,
        Status::Replaced => u.replaced_by.iter().all(|x| satisfied(st, x, depth + 1)),
        Status::Pending
        | Status::Held
        | Status::Inflight
        | Status::Delivered
        | Status::Merging
        | Status::Quarantined
        | Status::Stuck
        | Status::Splitting
        | Status::Withdrawn
        | Status::Other(_) => false,
    }
}

/// Paths another session holds for now: one path or glob per line in the
/// configured `reserved_file` (see `PumpConfig::reserved_path`). Blank lines and lines whose first character is `#`
/// (checked before trimming) are ignored. The file is read
/// afresh on every call; an unreadable or absent file holds nothing.
fn reserved_paths(ctx: &Ctx) -> Vec<String> {
    let Ok(text) = fs::read_to_string(ctx.cfg.reserved_path(&ctx.project)) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| l.trim().to_string())
        .collect()
}

/// The needs with a priority level above zero, read from `pump.db` on every
/// call. A missing database lists nothing.
#[must_use]
pub fn priority_needs(ctx: &Ctx) -> BTreeSet<String> {
    priority::levels(ctx).into_keys().collect()
}

/// `uids` in dispatch and merge order: fix units first (they are the path
/// back to a green gate), then units whose need has a priority level, highest
/// level first, then the rest; stable within each group.
#[must_use]
pub fn ranked(ctx: &Ctx, st: &State, mut uids: Vec<String>) -> Vec<String> {
    let levels = priority::levels(ctx);
    let rank = |uid: &String| -> (u8, Reverse<i64>) {
        if uid.starts_with(&ctx.cfg.fix_prefix) {
            return (0, Reverse(0));
        }
        match st.units.get(uid).and_then(|u| levels.get(&u.need)) {
            Some(level) => (1, Reverse(*level)),
            None => (2, Reverse(0)),
        }
    };
    uids.sort_by_key(rank);
    uids
}

/// The scope entries that are not generated paths for `target`: the files
/// the agent actually writes, with machine-regenerated paths excluded so
/// two units rebuilding the same generated file never block each other.
fn generated_free(ctx: &Ctx, target: &str, scope: &[String]) -> Vec<String> {
    let generated = ctx.cfg.generated_paths_for(target);
    scope
        .iter()
        .filter(|path| !overlaps(&[(*path).clone()], &generated))
        .cloned()
        .collect()
}

/// A unit is ready to dispatch when it is pending, outside its transient
/// backoff window, its need has no unlanded dependency, its write scope
/// overlaps no reserved path, every `after` dependency is satisfied, and no
/// other unit with an overlapping write scope is busy.
/// Busy means another agent is inflight; delivered and merging work does not
/// hold worker capacity. Fix and priority units only clash with fix units.
#[must_use]
pub fn is_ready(ctx: &Ctx, st: &State, uid: &str, holds: &BTreeMap<String, Vec<String>>) -> bool {
    let Some(u) = st.units.get(uid) else {
        return false;
    };
    if u.status != Status::Pending {
        return false;
    }
    if u.retry_not_before
        .is_some_and(|not_before| ctx.now() < not_before)
    {
        return false;
    }
    if holds.contains_key(&u.need) {
        return false;
    }
    let held = reserved_paths(ctx);
    if !held.is_empty() && overlaps(&generated_free(ctx, u.target_id(), &u.write_scope), &held) {
        return false;
    }
    if u.after.iter().any(|dep| !satisfied(st, dep, 0)) {
        return false;
    }
    !overlap_busy(ctx, st, uid, u)
}

/// A row of `vivi graph blocked --json`.
#[derive(Debug, Deserialize)]
struct BlockedRow {
    #[serde(default)]
    source_id: String,
    #[serde(default)]
    blocked_by: Vec<String>,
}

/// Needs held by an unlanded dependency, read once from the backlog graph:
/// `need -> prerequisite handles still unfinished`. A prerequisite the pump
/// already delivered — every unit of its need is satisfied — counts as
/// landed even while its Vivi node is still open, so a merged dependency
/// that nobody has closed does not hold its dependents.
///
/// An unreadable answer logs and holds nothing: the pump keeps dispatching
/// rather than stalling on a broken read.
#[must_use]
pub fn dependency_holds(ctx: &Ctx, st: &State) -> BTreeMap<String, Vec<String>> {
    let waiting = st
        .units
        .values()
        .any(|unit| matches!(unit.status, Status::Pending | Status::Delivered));
    if !waiting {
        return BTreeMap::new();
    }
    let out = ctx.vivi(
        "graph blocked backlog --kind need --json --confirm-large",
        60,
    );
    let Some(rows) = parse_blocked(&out.text) else {
        let detail: String = out.text.trim().chars().take(150).collect();
        ctx.log(&format!("dependency gate unreadable: {detail}"));
        return BTreeMap::new();
    };
    let mut holds = BTreeMap::new();
    for row in rows {
        let unmet: Vec<String> = row
            .blocked_by
            .into_iter()
            .filter(|dependency| !need_landed(st, dependency))
            .collect();
        if !row.source_id.is_empty() && !unmet.is_empty() {
            holds.insert(row.source_id, unmet);
        }
    }
    holds
}

/// Parse the JSON array of a `graph blocked --json` output. Empty output
/// (a host that answers nothing) means no blocked needs; text before the
/// first `[` is skipped, and invalid JSON gives `None`.
fn parse_blocked(text: &str) -> Option<Vec<BlockedRow>> {
    let text = text.trim();
    if text.is_empty() {
        return Some(Vec::new());
    }
    let start = text.find('[')?;
    match serde_json::from_str(&text[start..]) {
        Ok(rows) => Some(rows),
        Err(_) => None,
    }
}

/// True when the pump itself has delivered every unit of `need`. A need
/// with no plan record here defers to Vivi's node state.
fn need_landed(st: &State, need: &str) -> bool {
    if st.closed_needs.iter().any(|closed| closed == need) {
        return true;
    }
    st.needs.get(need).is_some_and(|record| {
        !record.units.is_empty() && record.units.iter().all(|uid| satisfied(st, uid, 0))
    })
}

/// Record the dependency hold on a unit in `status`, logging the first tick
/// of each hold. Returns whether the unit is held. Units in any other status
/// are left alone.
pub fn note_hold(
    ctx: &Ctx,
    st: &mut State,
    uid: &str,
    status: Status,
    dependency: Option<String>,
) -> bool {
    let Some(unit) = st.units.get(uid) else {
        return false;
    };
    if unit.status != status {
        return false;
    }
    let need = unit.need.clone();
    if unit.held_on == dependency {
        return dependency.is_some();
    }
    let Some(unit) = st.units.get_mut(uid) else {
        return false;
    };
    unit.held_on.clone_from(&dependency);
    let Some(dependency) = dependency else {
        return false;
    };
    ctx.log(&format!(
        "hold {uid}: need {need} waits on dependency {dependency} not landed"
    ));
    true
}

/// True when an agent is editing an overlapping write scope. Delivered and
/// merging units no longer occupy worker space, and the merge thread serializes
/// their integration; a fix unit ignores overlap against non-fix units.
fn overlap_busy(ctx: &Ctx, st: &State, uid: &str, u: &Unit) -> bool {
    let fix = uid.starts_with(ctx.cfg.fix_prefix.as_str()) || priority_needs(ctx).contains(&u.need);
    for (oid, o) in &st.units {
        if oid == uid {
            continue;
        }
        if o.target_id() != u.target_id() {
            continue;
        }
        let busy = o.status == Status::Inflight;
        if busy
            && overlaps(
                &generated_free(ctx, u.target_id(), &u.write_scope),
                &generated_free(ctx, o.target_id(), &o.write_scope),
            )
        {
            if fix && !oid.starts_with(ctx.cfg.fix_prefix.as_str()) {
                continue;
            }
            return true;
        }
    }
    false
}

/// Choose a dispatch lane for a unit: the last-resort model on the final
/// attempt when one is configured, otherwise the first lane under its cap that
/// is not the last attempt's model, then any lane under its cap. An empty
/// `last_resort` means the final attempt uses the normal lanes. Lane order
/// alternates with attempt parity. `None` only when every lane is full.
#[must_use]
pub fn pick_model(ctx: &Ctx, st: &State, unit: &Unit) -> Option<String> {
    let mut load: BTreeMap<&str, u32> = BTreeMap::new();
    for o in st.units.values() {
        if o.status == Status::Inflight
            && let Some(m) = o.model.as_deref()
        {
            *load.entry(m).or_insert(0) += 1;
        }
    }
    let last = ctx.cfg.dispatch.last_resort.as_str();
    if unit.attempts >= ctx.cfg.dispatch.max_attempts.saturating_sub(1) && !last.is_empty() {
        return Some(last.to_string());
    }
    let mut lanes: Vec<&Lane> = ctx.cfg.dispatch.lanes.iter().collect();
    if unit.attempts % 2 == 1 {
        lanes.reverse();
    }
    let last_model = unit.history.last().and_then(|h| h.model.as_deref());
    for skip_last in [true, false] {
        for lane in &lanes {
            let free = load.get(lane.model.as_str()).is_none_or(|n| *n < lane.cap);
            if free && (!skip_last || Some(lane.model.as_str()) != last_model) {
                return Some(lane.model.clone());
            }
        }
    }
    None
}
