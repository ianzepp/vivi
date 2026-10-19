//! Operator requeue: the half of `vivi pump requeue` the running pump runs.
//!
//! The CLI only queues a request in `pump.db` (`store::requeue`); this module
//! is the state side, applied once per tick before the board pass. A request
//! names a unit and how its attempt counter should move, and the pump decides
//! the rest:
//!
//! - Only a unit nobody owns is requeueable: `stuck`, `quarantined`,
//!   `splitting` or `withdrawn`. Anything else is refused and the request is
//!   consumed, so a stale request cannot fire hours later.
//! - The attempt counter never collides with a branch that already exists:
//!   every attempt branch (`unit/<uid>-a<n>`) is listed on the target's
//!   remote first, and the next attempt is the highest suffix + 1.
//!
//! A split plan file landing after a requeue still replaces the unit
//! (`plan::record` sets `Replaced` regardless of status): accepted, because a
//! requeue is an operator prompt, not a plan revision.

use std::collections::{BTreeMap, BTreeSet};

use crate::pump::host::{Ctx, shell_quote};
use crate::pump::state::{State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore, RequeueRequest};
use crate::pump::transient;

/// Timeout for one `ls-remote` of a target's unit branches.
const SCAN_TIMEOUT_SECS: u64 = 60;

/// Highest attempt suffix per uid, for one target: `uid -> max n`.
type TargetBranches = BTreeMap<String, u32>;

/// Apply every pending requeue request. A missing pump database means the
/// pump has no local state to requeue, so this is a no-op; an unreadable one
/// is logged once and also skipped.
pub fn apply_pending(ctx: &Ctx, st: &mut State) {
    let path = ctx.home.join(DB_FILENAME);
    let requests = match PumpStore::open_read_only(&path) {
        Ok(Some(store)) => match store.pending_requeue_requests() {
            Ok(requests) => requests,
            Err(err) => {
                ctx.log(&format!("requeue: cannot read requests: {err}"));
                return;
            }
        },
        Ok(None) => return,
        Err(err) => {
            ctx.log(&format!("requeue: cannot read {}: {err}", path.display()));
            return;
        }
    };
    if requests.is_empty() {
        return;
    }
    let branches = scan_targets(ctx, st, &requests);
    let mut processed: Vec<i64> = Vec::new();
    for request in &requests {
        apply_one(ctx, st, request, &branches);
        processed.push(request.request_id);
    }
    mark_applied(ctx, &path, &processed);
}

/// One branch scan per distinct target of the requested units, keyed by
/// target id. A target whose scan fails has no recorded branches, so its
/// units are treated as having none on the remote.
fn scan_targets(
    ctx: &Ctx,
    st: &State,
    requests: &[RequeueRequest],
) -> BTreeMap<String, TargetBranches> {
    let targets: BTreeSet<&str> = requests
        .iter()
        .filter_map(|request| st.units.get(&request.unit))
        .map(|unit| unit.target_id())
        .collect();
    let mut scanned: BTreeMap<String, TargetBranches> = BTreeMap::new();
    for target in targets {
        let target_ctx = ctx.for_target(target).unwrap_or_else(|| ctx.clone());
        let command = format!(
            "ls-remote --heads {} {}",
            shell_quote(&target_ctx.cfg.remote),
            shell_quote("refs/heads/unit/*")
        );
        let output = target_ctx.git(&command, SCAN_TIMEOUT_SECS);
        if output.code != 0 {
            ctx.log(&format!(
                "requeue: could not list unit branches for target {target}: {}",
                output.text.trim()
            ));
            continue;
        }
        scanned.insert(target.to_string(), remote_suffixes(&output.text));
    }
    scanned
}

/// The highest attempt suffix per uid in one `ls-remote` reply: `unit/<uid>-a<n>`.
fn remote_suffixes(output: &str) -> TargetBranches {
    let mut suffixes = TargetBranches::new();
    for branch in output
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter_map(|reference| reference.strip_prefix("refs/heads/"))
        .filter(|branch| branch.starts_with("unit/"))
    {
        let Some((uid, n)) = branch.trim_start_matches("unit/").rsplit_once("-a") else {
            continue;
        };
        let Ok(suffix) = n.parse::<u32>() else {
            continue;
        };
        suffixes
            .entry(uid.to_string())
            .and_modify(|best| *best = (*best).max(suffix))
            .or_insert(suffix);
    }
    suffixes
}

/// The refusal reason for a unit that cannot be requeued, or `None` when it
/// can. `missing` covers a request naming a unit this state does not have.
fn refusal(st: &State, uid: &str) -> Option<String> {
    match st.units.get(uid).map(|unit| unit.status) {
        None => Some(String::from("missing")),
        Some(Status::Stuck | Status::Quarantined | Status::Splitting | Status::Withdrawn) => None,
        Some(status) => Some(status.as_str().to_string()),
    }
}

/// Apply one request, refusing a unit the pump does not own.
fn apply_one(
    ctx: &Ctx,
    st: &mut State,
    request: &RequeueRequest,
    branches: &BTreeMap<String, TargetBranches>,
) {
    let uid = request.unit.as_str();
    if let Some(status) = refusal(st, uid) {
        ctx.log(&format!("requeue {uid} refused: status is {status}"));
        return;
    }
    let base = base_attempts(st, uid, request);
    let existing = highest_branch(branches, st, uid);
    let attempts = base.max(existing);
    if attempts > base {
        ctx.log(&format!(
            "requeue {uid} attempts raised to {attempts}: branch unit/{uid}-a{existing} already exists"
        ));
    }
    let now = ctx.now();
    if let Some(unit) = st.units.get_mut(uid) {
        match unit.status {
            Status::Quarantined => {
                unit.status = Status::Delivered;
                unit.solo = true;
                unit.quarantine = None;
            }
            _ => unit.status = Status::Pending,
        }
        unit.attempts = attempts;
        if request.refund {
            unit.transient_retries = 0;
        }
        unit.task = None;
        unit.held_on = None;
        unit.since = now;
    }
    ctx.log(&format!("requeue {uid} by operator: {}", request.reason));
}

/// The attempt count the request itself asks for, before branch collisions
/// are resolved: `attempts=N`, `fresh` (0), `refund` (real failures only,
/// transient ones subtracted), or the unit's current count.
fn base_attempts(st: &State, uid: &str, request: &RequeueRequest) -> u32 {
    let unit = match st.units.get(uid) {
        Some(unit) => unit,
        None => return 0,
    };
    if let Some(attempts) = request.attempts {
        return u32::try_from(attempts).unwrap_or(u32::MAX);
    }
    if request.fresh {
        return 0;
    }
    if request.refund {
        let transient = transient_count(unit);
        return unit.attempts.saturating_sub(transient);
    }
    unit.attempts
}

/// How many of the unit's attempts failed for a transient reason.
fn transient_count(unit: &Unit) -> u32 {
    let count = unit
        .history
        .iter()
        .filter(|attempt| transient::classify(&attempt.why, &attempt.detail).is_some())
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// The highest attempt branch suffix recorded for `uid` on its target's
/// remote, or 0 when it has none.
fn highest_branch(branches: &BTreeMap<String, TargetBranches>, st: &State, uid: &str) -> u32 {
    let Some(unit) = st.units.get(uid) else {
        return 0;
    };
    branches
        .get(unit.target_id())
        .and_then(|suffixes| suffixes.get(uid))
        .copied()
        .unwrap_or(0)
}

/// Mark every processed request applied, in one read-write open at the end so
/// a tick that dies mid-way re-applies rather than loses a request.
fn mark_applied(ctx: &Ctx, path: &std::path::Path, processed: &[i64]) {
    if processed.is_empty() {
        return;
    }
    let mut store = match PumpStore::open(path) {
        Ok(store) => store,
        Err(err) => {
            ctx.log(&format!("requeue: cannot open {}: {err}", path.display()));
            return;
        }
    };
    let now = ctx.now();
    for request_id in processed {
        if let Err(err) = store.mark_requeue_applied(*request_id, now) {
            ctx.log(&format!("requeue: cannot mark request applied: {err}"));
        }
    }
}
