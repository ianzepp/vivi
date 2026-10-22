//! Operator requeue: `vivi pump requeue`, both halves.
//!
//! The CLI validates the uids against the loaded state and posts one
//! requeue action per unit on the need's open pump record
//! ([`crate::mailspace::PumpActionRequest`]); the pump applies each exactly
//! once per tick ([`crate::pump::action`]). A request names a unit and how
//! its attempt counter should move, and the pump decides the rest:
//!
//! - Only a unit nobody owns is requeueable: `stuck`, `quarantined`,
//!   `splitting` or `withdrawn`, and only one of the record's own need.
//!   Anything else is refused and the action is consumed, so a stale request
//!   cannot fire hours later.
//! - The attempt counter never collides with a branch that already exists:
//!   every attempt branch (`vivi/unit/<uid>-a<n>`) is listed on the target's
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
use crate::mailspace::{Mailspace, PumpActionRequest, PumpState};
use crate::pump::control::project_home;
use crate::pump::host::{Ctx, RealHost, shell_quote};
use crate::pump::state::{State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::pump::transient;

/// Timeout for one `ls-remote` of a target's unit branches.
const SCAN_TIMEOUT_SECS: u64 = 60;

/// The reason a request carries when the operator gave none.
const DEFAULT_REASON: &str = "operator requeue";

/// One requeue to apply: a unit and how its attempt counter moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequeueRequest {
    pub unit: String,
    /// Set the attempt counter to this value.
    pub attempts: Option<u32>,
    /// Reset the attempt counter to zero.
    pub fresh: bool,
    /// Refund the attempts that failed for a transient reason.
    pub refund: bool,
    /// Why, for the pump log.
    pub reason: String,
}

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
            attempts: request.attempts,
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

/// Apply requeue requests to `st`. One branch scan per distinct target
/// covers the whole batch.
pub fn apply_batch(ctx: &Ctx, st: &mut State, requests: &[RequeueRequest]) {
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
    for request in requests {
        apply_one(ctx, st, request, &branches);
    }
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
            shell_quote(&format!(
                "refs/heads/{}*",
                crate::pump::need::unit_namespace_prefix()
            ))
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

/// The highest attempt suffix per uid in one `ls-remote` reply: `vivi/unit/<uid>-a<n>`.
fn remote_suffixes(output: &str) -> TargetBranches {
    let mut suffixes = TargetBranches::new();
    for branch in output
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter_map(|reference| reference.strip_prefix("refs/heads/"))
        .filter(|branch| branch.starts_with(crate::pump::need::unit_namespace_prefix()))
    {
        let Some((uid, n)) = branch
            .strip_prefix(crate::pump::need::unit_namespace_prefix())
            .and_then(|branch| branch.rsplit_once("-a"))
        else {
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
            "requeue {uid} attempts raised to {attempts}: branch {} already exists",
            crate::pump::need::unit_branch(uid, existing)
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

/// `vivi pump requeue <uid>...`: post one requeue action per named unit on
/// its need's open pump record, for the running pump to apply at its next
/// tick. `--dry-run` prints what each uid would become and posts nothing.
///
/// `vivi pump requeue --stuck-since <time> --why <class>` is the bulk form:
/// every unit still stuck inside that window gets a default-policy action
/// whose reason records the incident class.
///
/// # Errors
/// Returns [`VivariumError::Message`] for conflicting attempt flags, no uid,
/// an unknown uid, a mailspace without a pump database, or a unit whose need
/// has no open pump record (nothing is posted then); a config error for an
/// unparsable `--stuck-since` window; and whatever reading the database or
/// posting an action fails with.
pub fn run(project: &Path, cmd: &PumpRequeueCommand) -> Result<(), VivariumError> {
    let home = project_home(project)?;
    let policy = AttemptPolicy::from_command(cmd)?;
    if cmd.stuck_since.is_some() {
        return run_bulk(project, cmd, &home);
    }
    if cmd.units.is_empty() {
        return Err(VivariumError::Message(String::from(
            "requeue needs at least one unit uid",
        )));
    }
    let path = home.join(DB_FILENAME);
    let st = load_state(&path)?;
    for uid in &cmd.units {
        if !st.units.contains_key(uid.as_str()) {
            return Err(VivariumError::Message(format!("unknown unit: {uid}")));
        }
    }
    let ctx = cli_ctx(project, &home)?;
    let branches = scan_targets(&ctx, &st, &cmd.units);
    let planned = plan(&st, &branches, &cmd.units, policy);
    if cmd.dry_run {
        print_planned(&planned);
        return Ok(());
    }
    let reason = cmd.reason.as_deref().unwrap_or(DEFAULT_REASON);
    post_all(project, &st, &planned, policy, reason, false)
}

/// Post one requeue action per planned unit on its need's open pump record.
/// Every need is checked first, so a unit whose need has no open record
/// refuses the whole command before anything is posted; the bulk form
/// (`skip_closed`) skips such a unit with a printed line instead.
fn post_all(
    project: &Path,
    st: &State,
    planned: &[Planned],
    policy: AttemptPolicy,
    reason: &str,
    skip_closed: bool,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(Some(project))?;
    let open: BTreeSet<String> = mailspace
        .pump_list(None, Some(PumpState::Open))?
        .into_iter()
        .map(|record| record.need)
        .collect();
    let mut targets: Vec<(&str, &str)> = Vec::new();
    for (uid, ..) in planned {
        let need = st
            .units
            .get(uid.as_str())
            .map_or("", |unit| unit.need.as_str());
        if open.contains(need) {
            targets.push((uid.as_str(), need));
        } else if skip_closed {
            println!("requeue {uid} skipped: need {need} has no open pump record");
        } else {
            return Err(VivariumError::Message(format!(
                "unit {uid} belongs to need {need}, which has no open pump record, so there is no dispatch to requeue it in; open one with `vivi pump send --need {need}`"
            )));
        }
    }
    for (uid, need) in targets {
        let request =
            PumpActionRequest::requeue(uid, policy.attempts, policy.fresh, policy.refund, reason);
        mailspace.pump_post_action(None, need, &request)?;
        println!("{}", requested_line(uid));
    }
    Ok(())
}

/// `vivi pump requeue --stuck-since <time> --why <class>`: bulk recovery.
///
/// Selects every stuck unit whose last activity (`history` tail, else
/// `since`) is inside the window, then queues one default-policy request per
/// unit with reason `bulk <class>`. `--dry-run` prints the would-lines and
/// writes nothing; an empty selection is a successful no-op.
fn run_bulk(project: &Path, cmd: &PumpRequeueCommand, home: &Path) -> Result<(), VivariumError> {
    let Some(since) = cmd.stuck_since.as_deref() else {
        return Err(VivariumError::Message(String::from(
            "bulk requeue needs --stuck-since",
        )));
    };
    let Some(class) = cmd.why.as_deref() else {
        return Err(VivariumError::Message(String::from(
            "--stuck-since requires --why <class>",
        )));
    };
    if !cmd.units.is_empty() {
        return Err(VivariumError::Message(String::from(
            "--stuck-since takes no unit uids",
        )));
    }
    let window = crate::duration::parse_duration_secs(since)?;
    let path = home.join(DB_FILENAME);
    let st = load_state(&path)?;
    let selected = select_stuck(&st, now().saturating_sub(window));
    println!("{}", selected_line(&selected));
    if selected.is_empty() {
        return Ok(());
    }
    let ctx = cli_ctx(project, home)?;
    let branches = scan_targets(&ctx, &st, &selected);
    let planned = plan(&st, &branches, &selected, AttemptPolicy::default());
    if cmd.dry_run {
        print_planned(&planned);
        return Ok(());
    }
    let reason = format!("bulk {class}");
    post_all(
        project,
        &st,
        &planned,
        AttemptPolicy::default(),
        &reason,
        true,
    )
}

/// Load the pump state at `path`, refusing a mailspace with no database.
fn load_state(path: &Path) -> Result<State, VivariumError> {
    let Some(store) = PumpStore::open_read_only(path)? else {
        return Err(VivariumError::Message(format!(
            "no pump.db at {}",
            path.display()
        )));
    };
    store.load()
}

/// The units the bulk form selects: stuck, and last active at or after
/// `cutoff`. Ordered by uid.
fn select_stuck(st: &State, cutoff: u64) -> Vec<String> {
    st.units
        .iter()
        .filter(|(_, unit)| matches!(unit.status, Status::Stuck))
        .filter(|(_, unit)| last_activity(unit) >= cutoff)
        .map(|(uid, _)| uid.clone())
        .collect()
}

/// When a unit was last touched: the tail of its attempt history, or `since`
/// when it has no history.
fn last_activity(unit: &Unit) -> u64 {
    unit.history.last().map_or(unit.since, |attempt| attempt.at)
}

/// The selection receipt the bulk form prints first.
fn selected_line(selected: &[String]) -> String {
    if selected.is_empty() {
        String::from("selected 0 stuck unit(s)")
    } else {
        format!(
            "selected {} stuck unit(s): {}",
            selected.len(),
            selected.join(" ")
        )
    }
}

/// One planned requeue: uid, current status, current attempts, planned ones.
type Planned = (String, Status, u32, u32);

/// The requeue each uid in `uids` would get under `policy`, past any branch
/// that already exists on the remote.
fn plan(
    st: &State,
    branches: &BTreeMap<String, TargetBranches>,
    uids: &[String],
    policy: AttemptPolicy,
) -> Vec<Planned> {
    let mut planned: Vec<Planned> = Vec::new();
    for uid in uids {
        let Some(unit) = st.units.get(uid.as_str()) else {
            continue;
        };
        let remote_max = highest_branch(branches, st, uid);
        let attempts = planned_attempts(unit, policy, remote_max);
        planned.push((uid.clone(), unit.status, unit.attempts, attempts));
    }
    planned
}

/// The line printed once a request is queued.
fn requested_line(uid: &str) -> String {
    format!("requeue {uid} requested; the pump applies it at its next tick")
}

/// The `--dry-run` lines for one planned requeue batch.
fn print_planned(planned: &[Planned]) {
    for (uid, status, old, attempts) in planned {
        println!("{}", would_line(uid, *status, *old, *attempts));
    }
}

/// The `--dry-run` line for one unit: what the requeue would change, and the
/// branch the next attempt would take.
#[must_use]
pub fn would_line(uid: &str, status: Status, old: u32, attempts: u32) -> String {
    format!(
        "would requeue {uid}: status {}, attempts {old} -> {attempts}, next branch {}",
        status.as_str(),
        crate::pump::need::unit_branch(uid, attempts + 1)
    )
}

/// A context for the remote branch scan, with a real host and the pump home
/// resolved from the mailspace's `[pump]` table.
pub(crate) fn cli_ctx(project: &Path, home: &Path) -> Result<Ctx, VivariumError> {
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
