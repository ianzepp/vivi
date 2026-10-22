//! Board steps that throw a plan away so the need is planned again: a held
//! need whose prerequisites landed, and a first plan the pump rejected.

use std::fs;

use super::{NeedRow, PLANNER_BACKOFF, plan_path, tail_note};
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

/// A need whose first plan the pump rejected ([`crate::pump::plan`] records
/// it `invalid` with the findings) is asked for a new plan: the rejected file
/// moves to `plans/rejected/`, the need record is dropped, and the relaunch
/// waits the planner backoff, doubled per rejection. The launch carries the
/// findings in its prompt. The count is `planner_fails[<need>.invalid]`; past
/// [`crate::pump::plan::PLAN_REASKS`] the ingest holds the need instead.
pub fn reask_invalid_plans(ctx: &Ctx, st: &mut State, rows: &[NeedRow]) {
    for row in rows {
        let handle = row.handle.as_str();
        let due = st.needs.get(handle).is_some_and(|record| {
            record.verdict == "invalid" && record.units.is_empty() && record.revision <= 1
        }) && st.board.plan_findings.contains_key(handle);
        let key = crate::pump::plan::invalid_key(handle);
        let spent = st.board.planner_fails.get(&key).copied().unwrap_or(0);
        if !due || spent >= crate::pump::plan::PLAN_REASKS {
            continue;
        }
        forget_plan(ctx, st, handle, "rejected");
        let spent = spent + 1;
        st.board.planner_fails.insert(key, spent);
        let delay = PLANNER_BACKOFF.saturating_mul(1u64 << spent.saturating_sub(1).min(16));
        st.board
            .planner_retry_at
            .insert(handle.to_owned(), ctx.now().saturating_add(delay));
        let findings = st
            .board
            .plan_findings
            .get(handle)
            .cloned()
            .unwrap_or_default();
        ctx.log(&format!(
            "board: plan for {handle} rejected; asking the planner again in {delay}s              (re-ask {spent} of {}): {}",
            crate::pump::plan::PLAN_REASKS,
            tail_note(&findings, 600)
        ));
    }
}
