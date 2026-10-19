//! Operator requeue: `vivi pump requeue`, both halves.
//!
//! The CLI validates the uids against the loaded state and queues a request in
//! `pump.db` (`store::requeue`); the pump applies it once per tick before the
//! board pass. A request names a unit and how its attempt counter should
//! move, and the pump decides the rest:
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
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::VivariumError;
use crate::cli::PumpRequeueCommand;
use crate::mailspace::Mailspace;
use crate::pump::control::project_home;
use crate::pump::host::{Ctx, RealHost, shell_quote};
use crate::pump::state::{State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore, RequeueRequest};
use crate::pump::transient;

/// Timeout for one `ls-remote` of a target's unit branches.
const SCAN_TIMEOUT_SECS: u64 = 60;

/// The reason a request carries when the operator gave none.
const DEFAULT_REASON: &str = "operator requeue";

/// How a requeue moves a unit's attempt counter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AttemptPolicy {
    /// Set the counter to this value.
    attempts: Option<u32>,
    /// Subtract the attempts that failed for a transient reason.
    refund: bool,
    /// Reset the counter to zero.
    fresh: bool,
}

impl AttemptPolicy {
    /// The policy one queued request carries.
    fn from_request(request: &RequeueRequest) -> Self {
        AttemptPolicy {
            attempts: request.attempts.and_then(|n| u32::try_from(n).ok()),
            refund: request.refund,
            fresh: request.fresh,
        }
    }

    /// The policy the operator's command line asks for; at most one of
    /// `--attempts`, `--refund-host-failures` and `--fresh` may be given.
    fn from_command(cmd: &PumpRequeueCommand) -> Result<Self, VivariumError> {
        let chosen = usize::from(cmd.attempts.is_some())
            + usize::from(cmd.refund_host_failures)
            + usize::from(cmd.fresh);
        if chosen > 1 {
            return Err(VivariumError::Message(String::from(
                "use only one of --attempts, --refund-host-failures, --fresh",
            )));
        }
        Ok(AttemptPolicy {
            attempts: cmd.attempts,
            refund: cmd.refund_host_failures,
            fresh: cmd.fresh,
        })
    }
}

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
    let branches = scan_targets(
        ctx,
        st,
        &requests
            .iter()
            .map(|request| request.unit.clone())
            .collect::<Vec<String>>(),
    );
    let mut processed: Vec<i64> = Vec::new();
    for request in &requests {
        apply_one(ctx, st, request, &branches);
        processed.push(request.request_id);
    }
    mark_applied(ctx, &path, &processed);
}

/// One branch scan per distinct target of `uids`, keyed by target id. A
/// target whose scan fails has no recorded branches, so its units are treated
/// as having none on the remote.
fn scan_targets(ctx: &Ctx, st: &State, uids: &[String]) -> BTreeMap<String, TargetBranches> {
    let targets: BTreeSet<&str> = uids
        .iter()
        .filter_map(|uid| st.units.get(uid))
        .map(Unit::target_id)
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
    let policy = AttemptPolicy::from_request(request);
    let existing = highest_branch(branches, st, uid);
    let Some(unit) = st.units.get(uid) else {
        return;
    };
    let base = base_attempts(unit, policy);
    let attempts = planned_attempts(unit, policy, existing);
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

/// The attempt count a requeue plans for one unit: what the policy asks for
/// ([`base_attempts`]) unless a branch for a later attempt already exists on
/// the remote, in which case that suffix wins.
fn planned_attempts(unit: &Unit, policy: AttemptPolicy, remote_max: u32) -> u32 {
    base_attempts(unit, policy).max(remote_max)
}

/// The attempt count the policy itself asks for, before branch collisions are
/// resolved: `attempts=N`, `fresh` (0), `refund` (real failures only,
/// transient ones subtracted), or the unit's current count.
fn base_attempts(unit: &Unit, policy: AttemptPolicy) -> u32 {
    if let Some(attempts) = policy.attempts {
        return attempts;
    }
    if policy.fresh {
        return 0;
    }
    if policy.refund {
        return unit.attempts.saturating_sub(transient_count(unit));
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

/// `vivi pump requeue <uid>...`: queue one request per named unit for the
/// running pump to apply at its next tick. `--dry-run` prints what each uid
/// would become and writes nothing.
///
/// # Errors
/// Returns [`VivariumError::Message`] for conflicting attempt flags, a bulk
/// `--stuck-since` request (not implemented yet), no uid, an unknown uid, or
/// a mailspace without a pump database; and whatever opening or writing the
/// database fails with.
pub fn run(project: &Path, cmd: &PumpRequeueCommand) -> Result<(), VivariumError> {
    let home = project_home(project)?;
    let policy = AttemptPolicy::from_command(cmd)?;
    if cmd.stuck_since.is_some() {
        // Temporary: the bulk form arrives in the next unit.
        return Err(VivariumError::Message(String::from(
            "bulk requeue (--stuck-since) arrives in the next unit",
        )));
    }
    if cmd.units.is_empty() {
        return Err(VivariumError::Message(String::from(
            "requeue needs at least one unit uid",
        )));
    }
    let path = home.join(DB_FILENAME);
    let Some(store) = PumpStore::open_read_only(&path)? else {
        return Err(VivariumError::Message(format!(
            "no pump.db at {}",
            path.display()
        )));
    };
    let st = store.load()?;
    drop(store);
    for uid in &cmd.units {
        if !st.units.contains_key(uid.as_str()) {
            return Err(VivariumError::Message(format!("unknown unit: {uid}")));
        }
    }
    let ctx = cli_ctx(project, &home)?;
    let branches = scan_targets(&ctx, &st, &cmd.units);
    let mut planned: Vec<(String, Status, u32, u32)> = Vec::new();
    for uid in &cmd.units {
        let Some(unit) = st.units.get(uid.as_str()) else {
            continue;
        };
        let remote_max = highest_branch(&branches, &st, uid);
        let attempts = planned_attempts(unit, policy, remote_max);
        planned.push((uid.clone(), unit.status, unit.attempts, attempts));
    }
    if cmd.dry_run {
        for (uid, status, old, attempts) in &planned {
            println!("{}", would_line(uid, *status, *old, *attempts));
        }
        return Ok(());
    }
    let reason = cmd.reason.as_deref().unwrap_or(DEFAULT_REASON);
    let at = now();
    let mut store = PumpStore::open(&path)?;
    for (uid, ..) in &planned {
        store.add_requeue_request(
            uid,
            policy.attempts.map(i64::from),
            policy.fresh,
            policy.refund,
            reason,
            at,
        )?;
        println!("requeue {uid} requested; the pump applies it at its next tick");
    }
    Ok(())
}

/// The `--dry-run` line for one unit: what the requeue would change, and the
/// branch the next attempt would take.
#[must_use]
pub fn would_line(uid: &str, status: Status, old: u32, attempts: u32) -> String {
    format!(
        "would requeue {uid}: status {}, attempts {old} -> {attempts}, next branch unit/{uid}-a{}",
        status.as_str(),
        attempts + 1
    )
}

/// A context for the remote branch scan, with a real host and the pump home
/// resolved from the mailspace's `[pump]` table.
fn cli_ctx(project: &Path, home: &Path) -> Result<Ctx, VivariumError> {
    let mailspace = Mailspace::discover(Some(project))?;
    let cfg = mailspace.config.pump.clone().ok_or_else(|| {
        VivariumError::Config(String::from("no [pump] table in .vivi/mailspace.toml"))
    })?;
    let vivi = std::env::current_exe()
        .map_or_else(|_| String::from("vivi"), |exe| exe.display().to_string());
    let mut ctx = Ctx::new(
        cfg,
        Arc::new(RealHost::new(home)),
        mailspace.root.clone(),
        vivi,
    );
    ctx.home = home.to_path_buf();
    Ok(ctx)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
