//! Dispatch reconcile parsing and the split re-ask.
//!
//! `collect` reconciles the pool once per tick: it applies every finished
//! pool job's outcome, applies planner completions, and expires unanswered
//! split planners. `resplit_bad_plan` re-asks a split planner once after a
//! bad plan.

use std::collections::BTreeMap;
use std::fs;

use crate::pump::host::{Ctx, shell_quote, shell_split};
use crate::pump::state::{State, Status};

#[path = "collect/burst.rs"]
pub(crate) mod burst;

#[path = "collect/split_expiry.rs"]
mod split_expiry;

pub(crate) use burst::rate_limit_burst;
use split_expiry::expire_splits;

/// Split-planner exits without a plan before the unit is parked stuck.
pub(crate) const SPLIT_RETRY_MAX: u32 = 3;
/// Seconds a deferred split launch waits before it retries.
const SPLIT_LAUNCH_STAGGER: u64 = 60;

/// `SPLIT_PROMPT`; the resplit record and unit handle are substituted.
const SPLIT_PROMPT: &str = r#"One unit ran out of time on a worker, so it is too big. Split it.

The unit record is the JSON between `PUMP-RESPLIT-INPUT-BEGIN` and `PUMP-RESPLIT-INPUT-END` below. Read the code it names in your current target checkout. Return one complete split plan in the format of your system prompt, with these extra rules:
- top level: {"need": "<the unit's need>", "replaces": "{uid}", "verdict": "units", "reason": "...", "units": [...]}
- unit ids are "{uid}.1", "{uid}.2", ... ; two to four units; each one must be finishable in 5 minutes by a mid-level model
- retain the original unit's target for every replacement unit
- chain them with `after` when they touch the same file
- if the worker's last message shows the work is in fact already done or not a defect, write "units": [] and say why in "reason"
Input JSON:
PUMP-RESPLIT-INPUT-BEGIN
{input}
PUMP-RESPLIT-INPUT-END
"#;

/// Parse `vivi pool reconcile --porcelain` output into task handles mapped
/// to `(state, detail)`.
///
/// Only lines whose first word is `reconciled` or `collected` and that carry
/// at least six shell words count; word 1 is the task, word 4 the state,
/// word 5 the detail. A later line for the same task wins.
#[must_use]
pub fn parse_reconcile(out: &str) -> BTreeMap<String, (String, String)> {
    let mut seen = BTreeMap::new();
    for line in out.lines() {
        let words = shell_split(line);
        if words.len() >= 6 && (words[0] == "reconciled" || words[0] == "collected") {
            seen.insert(words[1].clone(), (words[4].clone(), words[5].clone()));
        }
    }
    seen
}

/// The last 6000 chars of `vivi task show <task> --latest`.
#[must_use]
pub fn receipt(ctx: &Ctx, task: &str) -> String {
    tail(&receipt_full(ctx, task), 6000)
}

/// All of `vivi task show <task> --latest`: the pool's summary lines (the
/// agent's exit code among them) come first, the agent's own final message
/// after them.
#[must_use]
pub fn receipt_full(ctx: &Ctx, task: &str) -> String {
    ctx.vivi(&format!("task show {task} --latest"), 60).text
}

/// Split planners currently launched and not waiting on a retry; an
/// unprocessed exit receipt still counts, which only defers launches (the
/// safe side).
fn live_split_planners(st: &State) -> usize {
    st.units
        .iter()
        .filter(|(_, unit)| unit.status == Status::Splitting && unit.split_retry_at.is_none())
        .filter(|(uid, unit)| {
            match crate::pump::planner_jobs::lifecycle(st, "split", uid, unit.since) {
                Some((crate::pump::jobs::JobStatus::Pending, _)) => true,
                Some((crate::pump::jobs::JobStatus::Dispatched, _)) => true,
                Some(_) => false,
                None => true,
            }
        })
        .count()
}

/// The concurrent split planner cap from config, never below one.
fn split_planner_cap(ctx: &Ctx) -> usize {
    usize::try_from(ctx.cfg.planner.planners_max.max(1)).unwrap_or(usize::MAX)
}

/// Re-ask the split planner once after it wrote a bad split plan: the plan
/// file moves to `plans/rejected/`, its `plans_seen` key is forgotten, and
/// the planner respawns with the validation error appended to the prompt.
/// False when no re-ask applies — an unknown unit, a unit that is no longer
/// splitting or stuck, or a unit whose one re-ask is already spent.
pub(crate) fn resplit_bad_plan(ctx: &Ctx, st: &mut State, uid: &str, reason: &str) -> bool {
    let applies = st.units.get(uid).is_some_and(|unit| {
        matches!(unit.status, Status::Splitting | Status::Stuck) && unit.split_reasks == 0
    });
    if !applies {
        return false;
    }
    let now = ctx.now();
    let key = format!("{uid}.split.json");
    let rejected = ctx.home.join("plans/rejected");
    drop(fs::create_dir_all(&rejected));
    drop(fs::rename(
        ctx.home.join("plans").join(&key),
        rejected.join(format!("{key}.{now}")),
    ));
    st.plans_seen.retain(|seen| seen != &key);
    let prompt = format!(
        "{}\nYour previous plan was rejected: {reason}. Fix that and write the complete plan again.\n",
        split_prompt(ctx, uid)
    );
    let target = st
        .units
        .get(uid)
        .map(|unit| unit.target_id().to_string())
        .unwrap_or_default();
    let queue_error = split_expiry::enqueue_split(ctx, st, uid, now, &prompt, &target)
        .err()
        .map(|error| error.to_string());
    if let Some(unit) = st.units.get_mut(uid) {
        unit.split_reasks = 1;
        unit.split_retry_at = None;
        unit.split_fails = 0;
        unit.split_rate_retries = 0;
        unit.since = now;
    }
    if let Some(error) = queue_error {
        split_expiry::mark_queue_failure(ctx, st, uid, now, &error);
    }
    ctx.log(&format!("split-reask {uid} {}", tail(reason, 120)));
    true
}

/// Reconcile the pool against dispatched jobs and planner completions.
pub fn collect(ctx: &Ctx, st: &mut State) {
    let (out, reconcile_ok) = if crate::pump::jobs::has_dispatched(st) {
        let output = ctx.vivi("pool reconcile --porcelain", 300);
        (output.text, output.code == 0)
    } else {
        (String::new(), true)
    };
    let seen = parse_reconcile(&out);
    if !reconcile_ok {
        // A failed round still reports what it collected; a collected result
        // is reported only once, so drop nothing that the round did report.
        ctx.log(&format!("pool reconcile failed: {}", tail(&out, 300)));
    }
    let _terminal_jobs = crate::pump::jobs::apply_reconcile(ctx, st, &seen);
    crate::pump::planner_jobs::apply_completions(ctx, st);
    expire_splits(ctx, st);
}

/// A local worktree target commits its result branch directly into the
/// target integration clone instead of pushing it to the configured remote.
pub(crate) fn local_branch_exists(ctx: &Ctx, branch: &str) -> bool {
    if ctx.cfg.targets.is_empty() {
        return false;
    }
    let out = ctx.git(
        &format!(
            "show-ref --verify --quiet refs/heads/{}",
            shell_quote(branch)
        ),
        30,
    );
    out.code == 0
}

/// `SPLIT_PROMPT` with the stored resplit record and unit handle substituted.
#[must_use]
fn split_prompt(ctx: &Ctx, uid: &str) -> String {
    let input =
        fs::read_to_string(ctx.home.join(format!("resplit/{uid}.json"))).unwrap_or_else(|error| {
            ctx.log(&format!("resplit input {uid} could not be read: {error}"));
            String::new()
        });
    SPLIT_PROMPT
        .replace("{uid}", uid)
        .replace("{input}", &input)
}

/// The last `max` characters of `text` (Python's `text[-max:]`).
#[must_use]
pub(crate) fn tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        text.to_owned()
    } else {
        text.chars().skip(count - max).collect()
    }
}
