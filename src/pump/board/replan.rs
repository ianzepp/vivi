//! Board steps that throw a plan away so the need is planned again: a held
//! need whose prerequisites landed.

use std::fs;

use super::{NeedRow, plan_path};
use crate::pump::host::Ctx;
use crate::pump::state::State;

/// Throw away `handle`'s plan so the board plans the need again: the plan
/// file moves to `plans/<dir>/`, its basename leaves `plans_seen`, and the
/// need record and its `planned` reply key are dropped.
pub(super) fn forget_plan(ctx: &Ctx, st: &mut State, handle: &str, dir: &str) {
    let moved = ctx.home.join("plans").join(dir);
    drop(fs::create_dir_all(&moved));
    drop(fs::rename(
        plan_path(ctx, handle),
        moved.join(format!("{handle}.{}.json", ctx.now())),
    ));
    let json = format!("{handle}.json");
    st.plans_seen.retain(|seen| seen != &json);
    st.needs.remove(handle);
    let key = format!("{handle}:planned");
    st.board.replied.retain(|sent| sent != &key);
}

/// A need held because its plan waits on other needs
/// ([`crate::pump::state::NeedRecord::waits_on`]) is planned again once every
/// one of them has landed: the repository the new plan reads is then the one
/// the prerequisites changed.
pub fn replan_unblocked(ctx: &Ctx, st: &mut State, rows: &[NeedRow]) {
    for row in rows {
        let ready = st.needs.get(&row.handle).is_some_and(|record| {
            record.verdict == "hold"
                && record.units.is_empty()
                && !record.waits_on.is_empty()
                && record
                    .waits_on
                    .iter()
                    .all(|need| crate::pump::ready::need_landed(st, need))
        });
        if !ready {
            continue;
        }
        forget_plan(ctx, st, &row.handle, "superseded");
        ctx.log(&format!(
            "board: prerequisites of held need {} have landed; re-planning",
            row.handle
        ));
    }
}
