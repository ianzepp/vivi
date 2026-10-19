//! Applying merge results to the pump state: the only writer of unit state
//! that merge lanes feed. [`Merger::apply`](super::Merger::apply) calls these.

use std::fs;
use std::io::Write;

use crate::pump::collect;
use crate::pump::host::Ctx;
use crate::pump::quarantine;
use crate::pump::ready::satisfied;
use crate::pump::state::{MergeResult, State, Status};

/// `ok=null`: the unit returns to `delivered` for a later retry, and the
/// log line is `<why> <uid>: <first 200 chars of detail>`.
pub(super) fn apply_undecided(ctx: &Ctx, st: &mut State, result: &MergeResult) {
    if let Some(unit) = st.units.get_mut(&result.uid) {
        unit.status = Status::Delivered;
    }
    let why = result.why.as_deref().unwrap_or("");
    let head: String = result
        .detail
        .clone()
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect();
    ctx.log(&format!("{why} {}: {head}", result.uid));
}

/// `ok=false`: a decided failure. A proof that failed as a *usage* error is
/// a plan defect, not a hand failure ([`apply_needs_replan`]); every other
/// failure goes through [`collect::fail`], which retries it or marks it
/// stuck.
pub(super) fn apply_failure(ctx: &Ctx, st: &mut State, result: &MergeResult) {
    let why = result.why.clone().unwrap_or_default();
    let detail = result.detail.clone().unwrap_or_default();
    if why == "proof-usage" {
        apply_needs_replan(ctx, st, result, &detail);
        return;
    }
    collect::fail(ctx, st, &result.uid, &why, &detail);
}

/// A proof whose command cannot run as written is a defect in the plan: park
/// the unit `needs-replan`, name the unit and command in an exception, and ask
/// for a revision of the need's plan.
///
/// No history attempt is pushed, no attempt record is posted, and
/// `unit.attempts` is left alone: the dispatch already counted this attempt,
/// and the unit is not redispatched until a revision amends it.
fn apply_needs_replan(ctx: &Ctx, st: &mut State, result: &MergeResult, detail: &str) {
    let uid = result.uid.as_str();
    let need = match st.units.get_mut(uid) {
        Some(unit) => {
            unit.status = Status::NeedsReplan;
            unit.since = ctx.now();
            unit.task = None;
            Some(unit.need.clone())
        }
        None => None,
    };
    let text: String = detail.chars().take(300).collect();
    ctx.exception("needs-replan", uid, &text);
    ctx.log(&format!("needs-replan {uid}: proof usage error"));
    if let Some(need) = need.filter(|need| st.needs.contains_key(need)) {
        crate::pump::amend::request(
            ctx,
            st,
            &need,
            &format!("unit {uid} failed its proof as a usage error"),
        );
    }
}

/// `ok=true`: the unit becomes `merged` with its tip and outside files, the
/// merge counter ticks, and the `scope-warn` and `merged` lines are logged.
pub(super) fn apply_merged(ctx: &Ctx, st: &mut State, result: &MergeResult) {
    let now = ctx.now();
    if let Some(unit) = st.units.get_mut(&result.uid) {
        unit.status = Status::Merged;
        unit.commit.clone_from(&result.tip);
        unit.merged_at = Some(now);
        unit.since = now;
        unit.outside.clone_from(&result.outside);
    }
    st.merges += 1;
    if !result.outside.is_empty() {
        let files: String = result.outside.join(" ").chars().take(300).collect();
        ctx.log(&format!(
            "scope-warn {} touched outside scope: {files}",
            result.uid
        ));
    }
    let tip: String = result
        .tip
        .as_deref()
        .unwrap_or_default()
        .chars()
        .take(10)
        .collect();
    ctx.log(&format!(
        "merged {} tip={} in {}s",
        result.uid,
        tip,
        result.secs.unwrap_or(0)
    ));
    close_need(ctx, st, &result.uid, &tip);
    land_quarantined(ctx, st, result);
    reclaim_work_lane(ctx, st, &result.uid);
}

/// Recycle the unit's pool work lane now that it is on the merge lane.
///
/// Pump did not do this before: [`apply_merged`] only set [`Status::Merged`]
/// (integration-branch success) and left the worktree, `targets/<hash>`
/// caches, and local `unit/` scratch branch in place. Promote to main is a
/// later gate step and is not a unit status — cleanup must not wait for it.
/// Missing mailspace or pool is a no-op; a failure is logged, not raised.
fn reclaim_work_lane(ctx: &Ctx, st: &State, uid: &str) {
    let Some(unit) = st.units.get(uid) else {
        return;
    };
    match crate::pool::clean::after_merged(&ctx.project, unit) {
        Ok(report) => {
            for action in &report.actions {
                ctx.log(&format!(
                    "work-lane clean {} {} {} {}",
                    action.kind.word(),
                    action.slot,
                    action.bytes,
                    action.note
                ));
            }
        }
        Err(e) => ctx.log(&format!("work-lane clean {uid}: {e}")),
    }
}

/// A repair unit landed: the batch it repaired is on the integration branch
/// with it, so every unit still set aside under its quarantine id merges with
/// the repair's tip, closing needs like any other merge.
fn land_quarantined(ctx: &Ctx, st: &mut State, result: &MergeResult) {
    let Some(id) = quarantine::repair_of(ctx, st, &result.uid) else {
        return;
    };
    for uid in quarantine::members(st, &id) {
        let landed = MergeResult {
            uid,
            ok: Some(true),
            tip: result.tip.clone(),
            secs: result.secs,
            ..MergeResult::default()
        };
        apply_merged(ctx, st, &landed);
    }
}

/// Close the unit's need and append its `needs-ready.txt` line when the
/// need record exists, is not closed yet, and every one of its units is
/// satisfied.
fn close_need(ctx: &Ctx, st: &mut State, uid: &str, tip: &str) {
    let Some(need_id) = st.units.get(uid).map(|u| u.need.clone()) else {
        return;
    };
    let Some(need) = st.needs.get(&need_id) else {
        return;
    };
    if st.closed_needs.contains(&need_id) || !need.units.iter().all(|x| satisfied(st, x, 0)) {
        return;
    }
    let units = need.units.join(",");
    st.closed_needs.push(need_id.clone());
    let line = format!("{need_id} {tip} {units}\n");
    if append_needs_ready(ctx, &line).is_err() {
        ctx.log(&format!("needs-ready: cannot append {need_id}"));
    }
}

/// Append one line to `<home>/needs-ready.txt`, creating the file when it
/// does not exist yet.
fn append_needs_ready(ctx: &Ctx, line: &str) -> std::io::Result<()> {
    let path = ctx.home.join("needs-ready.txt");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())
}
