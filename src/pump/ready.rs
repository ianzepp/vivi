//! Unit readiness and model choice.
//!
//! `satisfied`, `reserved_paths`, `ready` and `pick_model`.

use std::collections::BTreeMap;
use std::fs;

use crate::pump::config::Lane;
use crate::pump::host::Ctx;
use crate::pump::plan::overlaps;
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
        | Status::Inflight
        | Status::Delivered
        | Status::Merging
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

/// A unit is ready to dispatch when it is pending, its write scope overlaps
/// no reserved path, every `after` dependency is satisfied, and no other unit with an overlapping write scope is busy.
/// Fix units (ids starting with `fix_prefix`) only clash with other fix
/// units, and only while those are inflight.
#[must_use]
pub fn is_ready(ctx: &Ctx, st: &State, uid: &str) -> bool {
    let Some(u) = st.units.get(uid) else {
        return false;
    };
    if u.status != Status::Pending {
        return false;
    }
    let held = reserved_paths(ctx);
    if !held.is_empty() && overlaps(&u.write_scope, &held) {
        return false;
    }
    if u.after.iter().any(|dep| !satisfied(st, dep, 0)) {
        return false;
    }
    !overlap_busy(ctx, st, uid, u)
}

/// True when another unit is busy on an overlapping write scope. Busy is
/// `inflight` for fix units and `inflight` + `delivered` + `merging` for
/// normal units; a fix unit ignores overlap against non-fix units.
fn overlap_busy(ctx: &Ctx, st: &State, uid: &str, u: &Unit) -> bool {
    let fix = uid.starts_with(ctx.cfg.fix_prefix.as_str());
    for (oid, o) in &st.units {
        if oid == uid {
            continue;
        }
        let busy = if fix {
            o.status == Status::Inflight
        } else {
            matches!(
                o.status,
                Status::Inflight | Status::Delivered | Status::Merging
            )
        };
        if busy && overlaps(&u.write_scope, &o.write_scope) {
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
