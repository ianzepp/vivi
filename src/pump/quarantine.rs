//! Quarantine: a merge batch whose shared check failed, set aside.
//!
//! When a shared check fails on a batch of several units, nobody knows which
//! unit broke it, and finding out costs a full check run per unit. The lane
//! instead pushes the merged batch to `pump/quarantine/<id>` and reports every
//! merged unit as quarantined (`merge::batch`). This module is the state side:
//! [`set_aside`] parks each unit and opens ONE repair unit that starts from the
//! quarantine branch; [`repair_of`], [`members`] and [`repair_stuck`] close the
//! loop when the repair unit merges or goes stuck.

use sha2::{Digest, Sha256};

use crate::pump::host::Ctx;
use crate::pump::state::{MergeResult, NeedRecord, Quarantine, State, Status, Unit};

/// The branch a quarantined batch is pushed to.
#[must_use]
pub fn branch(id: &str) -> String {
    format!("pump/quarantine/{id}")
}

/// A fresh quarantine id for a batch of `uids`: `q<epoch>-<6 hex>`, the hex
/// taken from the batch's uids so concurrent lanes never collide.
#[must_use]
pub fn new_id(ctx: &Ctx, uids: &[&str]) -> String {
    let digest = Sha256::digest(uids.join(",").as_bytes());
    format!("q{}-{}", ctx.now(), hex::encode(&digest[..3]))
}

/// The repair unit's handle for quarantine `id`: `<fix_prefix>-<id>`, so it
/// ranks with the fix units.
#[must_use]
pub fn repair_uid(ctx: &Ctx, id: &str) -> String {
    format!("{}-{id}", ctx.cfg.fix_prefix)
}

/// The quarantine id `uid` repairs, when it is a repair unit.
#[must_use]
pub fn repair_of(ctx: &Ctx, st: &State, uid: &str) -> Option<String> {
    st.units
        .get(uid)?
        .quarantine
        .clone()
        .filter(|id| repair_uid(ctx, id) == uid)
}

/// The units still set aside under quarantine `id`, in creation order.
#[must_use]
pub fn members(st: &State, id: &str) -> Vec<String> {
    st.order
        .iter()
        .filter(|uid| {
            st.units.get(*uid).is_some_and(|u| {
                u.status == Status::Quarantined && u.quarantine.as_deref() == Some(id)
            })
        })
        .cloned()
        .collect()
}

/// Apply one `quarantined` merge result: park the unit and, for the first
/// result of its batch, open the repair unit.
pub fn set_aside(ctx: &Ctx, st: &mut State, result: &MergeResult) {
    let Some(q) = &result.quarantine else {
        return;
    };
    let now = ctx.now();
    if let Some(unit) = st.units.get_mut(&result.uid) {
        unit.status = Status::Quarantined;
        unit.quarantine = Some(q.id.clone());
        unit.since = now;
    }
    ctx.log(&format!("quarantined {} on {}", result.uid, branch(&q.id)));
    let repair = repair_uid(ctx, &q.id);
    if !st.units.contains_key(&repair) {
        open_repair(ctx, st, q, &repair);
    }
}

/// Mint the repair unit for `q` and its need record, and raise the
/// `quarantine` exception.
fn open_repair(ctx: &Ctx, st: &mut State, q: &Quarantine, repair: &str) {
    let parked: Vec<(&String, &Unit)> = q
        .units
        .iter()
        .filter_map(|uid| st.units.get_key_value(uid))
        .collect();
    let mut scope: Vec<String> = Vec::new();
    for (_, unit) in &parked {
        for path in &unit.write_scope {
            if !scope.contains(path) {
                scope.push(path.clone());
            }
        }
    }
    let unit = Unit {
        need: repair.to_string(),
        target: parked.first().map_or_else(
            || String::from("default"),
            |(_, u)| u.target_id().to_string(),
        ),
        revision: 1,
        title: format!(
            "Repair quarantined batch {} ({} units)",
            q.id,
            q.units.len()
        ),
        brief: repair_brief(q, &parked),
        write_scope: scope,
        proof: vec![q.cmd.clone()],
        status: Status::Pending,
        since: ctx.now(),
        base: Some(branch(&q.id)),
        quarantine: Some(q.id.clone()),
        ..Unit::default()
    };
    st.units.insert(repair.to_string(), unit);
    st.order.push(repair.to_string());
    st.needs.insert(
        repair.to_string(),
        NeedRecord {
            verdict: String::from("units"),
            reason: format!("repair of quarantined batch {}", q.id),
            units: vec![repair.to_string()],
            revision: 1,
            ..NeedRecord::default()
        },
    );
    let summary = format!(
        "{} units set aside on {}: {} rc={}; repair unit {repair}",
        q.units.len(),
        branch(&q.id),
        q.cmd,
        q.code
    );
    ctx.exception("quarantine", &q.id, &summary);
}

/// The repair unit's brief: what failed, which units are in the batch, and
/// what to do about it.
fn repair_brief(q: &Quarantine, parked: &[(&String, &Unit)]) -> String {
    let units: Vec<String> = parked
        .iter()
        .map(|(uid, u)| format!("- {uid} (need {}): {}", u.need, u.title))
        .collect();
    format!(
        "A batch of {} merged units failed a shared check, so the batch was set aside on the branch {} instead of reaching the integration branch. The check that failed (exit {}):\n{}\n\nWhat failed:\n{}\n\nThe quarantined units:\n{}\n\nThe branch you start on already contains the merged batch. Make the failing check pass with the smallest change. Keep every unit's feature: do not revert or drop any unit's changes. If a unit cannot be made to pass without removing its feature, stop and say which unit in your reply.",
        q.units.len(),
        branch(&q.id),
        q.code,
        q.cmd,
        q.text,
        units.join("\n")
    )
}

/// The repair unit `uid` went stuck: return its quarantined units to
/// `delivered`, each marked solo so it merges in a batch of its own, and
/// raise `quarantine-stuck`. Does nothing for any other unit.
pub fn repair_stuck(ctx: &Ctx, st: &mut State, uid: &str) {
    let Some(id) = repair_of(ctx, st, uid) else {
        return;
    };
    let parked = members(st, &id);
    for member in &parked {
        if let Some(unit) = st.units.get_mut(member) {
            unit.status = Status::Delivered;
            unit.solo = true;
            unit.quarantine = None;
        }
    }
    let summary = format!(
        "repair {uid} is stuck; {} units return to delivered, one batch each: {}",
        parked.len(),
        parked.join(",")
    );
    ctx.exception("quarantine-stuck", &id, &summary);
}

/// The repair unit `uid` found nothing to fix (the failed check passes on
/// its checkout, so it was flaky): return its quarantined units to
/// `delivered` for an ordinary batch. Does nothing for any other unit.
pub fn repair_no_defect(ctx: &Ctx, st: &mut State, uid: &str) {
    let Some(id) = repair_of(ctx, st, uid) else {
        return;
    };
    let parked = members(st, &id);
    for member in &parked {
        if let Some(unit) = st.units.get_mut(member) {
            unit.status = Status::Delivered;
            unit.quarantine = None;
        }
    }
    ctx.log(&format!(
        "quarantine {id}: repair {uid} found no defect; {} units return to delivered",
        parked.len()
    ));
}
