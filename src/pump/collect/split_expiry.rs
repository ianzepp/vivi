//! Split planner receipt, retry and hard-cap handling.

use std::fs;

use crate::pump::envfail;
use crate::pump::host::{Ctx, exit_receipt_path};
use crate::pump::state::{State, Status, Unit};
use crate::pump::transient;

use super::{
    SPLIT_LAUNCH_STAGGER, SPLIT_RETRY_MAX, live_split_planners, rate_limit_burst,
    split_planner_cap, split_prompt,
};

/// The dead split planner's raw output (`resplit/<uid>.out`) when the host or
/// harness killed it before it could plan. `None` means silent or answered.
fn dead_planner_output(ctx: &Ctx, uid: &str) -> Option<String> {
    let path = ctx.home.join("resplit").join(format!("{uid}.out"));
    let Ok(text) = fs::read_to_string(path) else {
        return None;
    };
    envfail::classify("planner", &text, "").map(|_| text)
}

/// Expire split planners, report one rate-limit event for the whole burst,
/// and repair a quarantined unit if a terminal failure parks it.
pub(super) fn expire_splits(ctx: &Ctx, st: &mut State) {
    let now = ctx.now();
    let mut rate_limited = Vec::new();
    let splitting: Vec<String> = st
        .units
        .iter()
        .filter(|(_, unit)| unit.status == Status::Splitting)
        .map(|(id, _)| id.clone())
        .collect();
    for uid in splitting {
        if expire_one_split(ctx, st, &uid, now, &mut rate_limited) {
            crate::pump::quarantine::repair_stuck(ctx, st, &uid);
        }
    }
    rate_limit_burst(ctx, st, &rate_limited);
}

/// Decide one splitting unit. A written plan belongs to ingest; retries use
/// their delay; an unanswered planner is parked after the configured cap.
fn expire_one_split(
    ctx: &Ctx,
    st: &mut State,
    uid: &str,
    now: u64,
    rate_limited: &mut Vec<String>,
) -> bool {
    if ctx.home.join(format!("plans/{uid}.split.json")).exists() {
        return false;
    }
    let receipt = exit_receipt_path(&ctx.home.join(format!("resplit/{uid}.out")));
    let cap = split_planner_cap(ctx);
    let live = live_split_planners(st);
    if receipt.exists() {
        return expired_receipt(ctx, st, uid, now, &receipt, rate_limited);
    }
    if retry_split(ctx, st, uid, now, live, cap) {
        return false;
    }
    let Some(queued_at) = st.units.get(uid).map(|unit| unit.since) else {
        return false;
    };
    let started_at = match crate::pump::planner_jobs::lifecycle(st, "split", uid, queued_at) {
        Some((crate::pump::jobs::JobStatus::Pending, _)) => return false,
        Some((crate::pump::jobs::JobStatus::Dispatched, Some(at))) => at,
        Some((crate::pump::jobs::JobStatus::Dispatched, None)) => queued_at,
        Some(_) => return false,
        None => queued_at,
    };
    let Some(unit) = st.units.get_mut(uid) else {
        return false;
    };
    if let Some(text) = dead_planner_output(ctx, uid) {
        return split_planner_died(ctx, unit, uid, now, &text);
    }
    if now.saturating_sub(started_at) > ctx.cfg.planner.split_hard_cap {
        unit.status = Status::Stuck;
        let text = format!(
            "the split planner is still unreachable after {} minutes",
            ctx.cfg.planner.split_hard_cap / 60
        );
        ctx.exception("split-no-answer", uid, &text);
        return true;
    }
    false
}

/// Consume one split planner exit receipt, applying rate-limit or ordinary
/// failure accounting.
fn expired_receipt(
    ctx: &Ctx,
    st: &mut State,
    uid: &str,
    now: u64,
    receipt: &std::path::Path,
    rate_limited: &mut Vec<String>,
) -> bool {
    let raw = fs::read_to_string(receipt).unwrap_or_default();
    let code = raw.trim().parse::<i32>().unwrap_or(-1);
    drop(fs::remove_file(receipt));
    let Some(unit) = st.units.get_mut(uid) else {
        return false;
    };
    if let Some(wait) = rate_limited_retry(ctx, unit, uid) {
        unit.split_retry_at = Some(now + wait);
        rate_limited.push(uid.to_owned());
        ctx.log(&format!(
            "split-rate-limited {uid} retry in {wait}s; retry budget untouched"
        ));
        return false;
    }
    let fails = unit.split_fails + 1;
    unit.split_fails = fails;
    if fails >= SPLIT_RETRY_MAX {
        let text = format!(
            "the split planner exited without a plan {SPLIT_RETRY_MAX} times \
             (last exit {code})"
        );
        unit.status = Status::Stuck;
        ctx.exception("split-planner-failed", uid, &text);
        return true;
    }
    let backoff = (60u64 << (fails - 1)).min(900);
    unit.split_retry_at = Some(now + backoff);
    ctx.exception(
        "split-planner-failed",
        uid,
        &format!("planner exited {code} without a plan; retry {fails}/3 in {backoff}s"),
    );
    false
}

/// A provider 429 is not the unit's fault: back off without spending its
/// split-planner retry budget.
fn rate_limited_retry(ctx: &Ctx, unit: &mut Unit, uid: &str) -> Option<u64> {
    let out = fs::read_to_string(ctx.home.join(format!("resplit/{uid}.out"))).unwrap_or_default();
    if !transient::provider_rate_limited(&out) {
        return None;
    }
    unit.split_rate_retries = unit.split_rate_retries.saturating_add(1);
    let base = (120u64 << unit.split_rate_retries.saturating_sub(1).min(3)).min(900);
    Some(base + transient::jitter(uid))
}

/// A retry at capacity is staggered; otherwise the planner is relaunched.
/// Waiting units are never parked, even past the hard cap.
fn retry_split(ctx: &Ctx, st: &mut State, uid: &str, now: u64, live: usize, cap: usize) -> bool {
    let target = {
        let Some(unit) = st.units.get_mut(uid) else {
            return false;
        };
        let Some(at) = unit.split_retry_at else {
            return false;
        };
        if now < at {
            return true;
        }
        if live >= cap {
            unit.split_retry_at = Some(now + SPLIT_LAUNCH_STAGGER);
            ctx.log(&format!(
                "split-launch-deferred {uid}: {cap} split planners already running"
            ));
            return true;
        }
        unit.target_id().to_string()
    };
    let prompt = split_prompt(ctx, uid);
    if let Err(error) = enqueue_split(ctx, st, uid, now, &prompt, &target) {
        mark_queue_failure(ctx, st, uid, now, &error.to_string());
        return true;
    }
    if let Some(unit) = st.units.get_mut(uid) {
        unit.split_retry_at = None;
        unit.since = now;
    }
    ctx.log(&format!("split-retry {uid}"));
    true
}

/// Queue a split planner durably; a queue failure parks it for operator
/// recovery instead of leaving a phantom live split.
pub(super) fn enqueue_split(
    ctx: &Ctx,
    st: &mut State,
    uid: &str,
    now: u64,
    prompt: &str,
    target: &str,
) -> Result<String, crate::VivariumError> {
    crate::pump::planner_jobs::enqueue(
        ctx,
        st,
        "split",
        uid,
        now,
        prompt,
        ctx.cfg.planner.split_hard_cap,
        &format!("resplit/{uid}.out"),
        &format!("plans/{uid}.split.json"),
        Some(target),
    )
}

/// Park a unit when a durable planner task could not be stored.
pub(super) fn mark_queue_failure(ctx: &Ctx, st: &mut State, uid: &str, now: u64, reason: &str) {
    let need = if let Some(unit) = st.units.get_mut(uid) {
        unit.status = Status::Stuck;
        unit.split_retry_at = None;
        unit.since = now;
        unit.need.clone()
    } else {
        return;
    };
    ctx.exception(
        "split-planner-queue-failed",
        uid,
        &format!(
            "durable split planner queue failed: {reason}; retry with `vivi pump replan {need}`"
        ),
    );
    crate::pump::quarantine::repair_stuck(ctx, st, uid);
}

/// The split planner died host-class: return the unit to `pending` so it can
/// be redispatched, and report the evidence. Never stuck.
fn split_planner_died(ctx: &Ctx, unit: &mut Unit, uid: &str, now: u64, text: &str) -> bool {
    let evidence: String = text.trim().chars().take(200).collect();
    unit.status = Status::Pending;
    unit.task = None;
    unit.since = now;
    unit.since_dispatch = None;
    ctx.exception(
        "environment",
        uid,
        &format!("split planner died: {evidence}"),
    );
    ctx.log(&format!("split-planner-dead {uid}"));
    false
}
