//! Quarantine: leftover state for a merge batch whose shared check failed.
//!
//! Nothing creates quarantine records any more; these helpers only finish the
//! loop for units already parked that way. [`repair_of`] names the quarantine a
//! repair unit repairs, [`members`] lists the units still set aside, and
//! [`repair_stuck`] returns them to `delivered` when that repair goes stuck.
//!
//! A repair unit carries the id it repairs in `Unit::repairs`, never in
//! `quarantine` (membership), and is `solo`: it merges alone.

use crate::pump::host::Ctx;
use crate::pump::state::{State, Status};

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

/// Return `uid` to `delivered` as a solo unit, out of any quarantine.
fn release(st: &mut State, uid: &str) {
    if let Some(unit) = st.units.get_mut(uid) {
        unit.status = Status::Delivered;
        unit.solo = true;
        unit.quarantine = None;
    }
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
