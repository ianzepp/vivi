//! Quarantine: a merge batch whose shared check failed, set aside.
//!
//! When a shared check fails on a batch of several units, nobody knows which
//! unit broke it, and finding out costs a full check run per unit. The lane
//! instead pushes the merged batch to `pump/quarantine/<id>` and reports every
//! merged unit as quarantined (`merge::batch`). This module is the state side:
//! [`set_aside`] parks each unit and opens ONE repair unit that starts from the
//! quarantine branch; [`repair_of`], [`members`] and [`repair_stuck`] close the
//! loop when the repair unit merges or goes stuck.
//!
//! A repair unit carries the id it repairs in `Unit::repairs`, never in
//! `quarantine` (membership), and is `solo`: it merges alone, so it cannot be
//! swept into another failing batch and lose its members.

use sha2::{Digest, Sha256};

use crate::pump::attempt_record::{self, Ending};
use crate::pump::host::Ctx;
use crate::pump::state::{MergeResult, NeedRecord, Quarantine, State, Status, Unit};

/// The branch a quarantined batch is pushed to.
#[must_use]
pub fn branch(id: &str) -> String {
    format!("pump/quarantine/{id}")
}

/// The ref a repair unit starts from: the quarantine branch as a remote-tracking
/// ref (`<remote>/pump/quarantine/<id>`), the same shape as `dispatch.from`, so a
/// pool resolves it the way it resolves `origin/next`.
#[must_use]
pub fn base(remote: &str, id: &str) -> String {
    format!("{remote}/{}", branch(id))
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
        .repairs
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
/// result of its batch, open the repair unit. A repair unit is never parked
/// as a member (it goes back to `delivered` to merge alone again), and when
/// no dispatch kind resolves for a repair the unit is released as solo.
pub fn set_aside(ctx: &Ctx, st: &mut State, result: &MergeResult) {
    let Some(q) = &result.quarantine else {
        return;
    };
    if st
        .units
        .get(&result.uid)
        .is_some_and(|unit| unit.repairs.is_some())
    {
        release(st, &result.uid);
        ctx.log(&format!(
            "repair unit {} met quarantine {}; it merges alone again",
            result.uid, q.id
        ));
        return;
    }
    let repair = repair_uid(ctx, &q.id);
    let Some(kind) = repair_kind(ctx, st, q) else {
        release(st, &result.uid);
        if lead(st, q) == Some(result.uid.as_str()) {
            let summary = format!(
                "no dispatch kind resolves for the repair of {}; its units return to delivered, one batch each: {}",
                branch(&q.id),
                q.units.join(",")
            );
            ctx.exception("quarantine-no-kind", &q.id, &summary);
        }
        return;
    };
    if let Some(unit) = st.units.get_mut(&result.uid) {
        unit.status = Status::Quarantined;
        unit.quarantine = Some(q.id.clone());
        unit.since = ctx.now();
    }
    ctx.log(&format!("quarantined {} on {}", result.uid, branch(&q.id)));
    attempt_record::post(ctx, st, &result.uid, &Ending::quarantined(&q.id, &repair));
    if !st.units.contains_key(&repair) {
        open_repair(ctx, st, q, &repair, &kind);
    }
}

/// Return `uid` to `delivered` as a solo unit, out of any quarantine.
fn release(st: &mut State, uid: &str) {
    if let Some(unit) = st.units.get_mut(uid) {
        unit.status = Status::Delivered;
        unit.solo = true;
        unit.quarantine = None;
    }
}

/// The first unit of `q` that is a plain batch member (not a repair unit).
fn lead<'a>(st: &State, q: &'a Quarantine) -> Option<&'a str> {
    q.units
        .iter()
        .find(|uid| st.units.get(*uid).is_some_and(|u| u.repairs.is_none()))
        .map(String::as_str)
}

/// The kind (`tools` value) of the repair unit for `q`: the first member's
/// non-empty `tools`, else `default_kind`. `None` when it names no defined
/// kind, so the repair could never be dispatched.
fn repair_kind(ctx: &Ctx, st: &State, q: &Quarantine) -> Option<String> {
    let tools = q
        .units
        .iter()
        .filter_map(|uid| st.units.get(uid))
        .filter(|unit| unit.repairs.is_none())
        .map(|unit| unit.tools.as_str())
        .find(|tools| !tools.is_empty())
        .unwrap_or("");
    ctx.cfg.kind_of(tools)?;
    Some(ctx.cfg.kind_name(tools).to_string())
}

/// Mint the repair unit for `q` and its need record, and raise the
/// `quarantine` exception.
fn open_repair(ctx: &Ctx, st: &mut State, q: &Quarantine, repair: &str, kind: &str) {
    let parked: Vec<(&String, &Unit)> = q
        .units
        .iter()
        .filter_map(|uid| st.units.get_key_value(uid))
        .filter(|(_, unit)| unit.repairs.is_none())
        .collect();
    let count = parked.len();
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
        title: format!("Repair quarantined batch {} ({} units)", q.id, parked.len()),
        brief: repair_brief(q, &parked),
        write_scope: scope,
        proof: vec![q.cmd.clone()],
        tools: kind.to_string(),
        status: Status::Pending,
        since: ctx.now(),
        base: Some(base(&ctx.cfg.remote, &q.id)),
        repairs: Some(q.id.clone()),
        solo: true,
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
        count,
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
        release(st, member);
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
/// `delivered`, each marked solo like [`repair_stuck`] so they cannot sink one
/// batch together again. Does nothing for any other unit.
pub fn repair_no_defect(ctx: &Ctx, st: &mut State, uid: &str) {
    let Some(id) = repair_of(ctx, st, uid) else {
        return;
    };
    let parked = members(st, &id);
    for member in &parked {
        release(st, member);
    }
    ctx.log(&format!(
        "quarantine {id}: repair {uid} found no defect; {} units return to delivered, one batch each",
        parked.len()
    ));
}
