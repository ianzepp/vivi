//! Scheduling and job-spec helpers for command cadence roles.
//!
//! A command role runs a shell command instead of a model. It needs no
//! charter, ignores the open want/need caps, and never waits on the outbound
//! signal age or the model's grace interval: its clock is its own completion
//! time. The selected repository target is shared with the model path, so one
//! ruling covers both — the role's `target`, else the pump's primary target,
//! else the legacy implicit target with the workspace main.

use crate::mailspace::RoleView;
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobSpec};
use crate::pump::state::State;
use vivi_util::duration::parse_duration_secs;

use super::check::{self, CommandCheck, CommandExecution};
use super::{CadenceState, DISABLED_STATUSES, RoleScan, epoch_label};

/// The repository target a cadence job works on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetChoice {
    /// A configured target: the job names it and starts from its main.
    Named { id: String, main: String },
    /// No target: the legacy single-repository pump and its workspace main.
    Legacy,
    /// The role names a target this pump does not configure.
    Unavailable { id: String },
}

/// Select the target for one cadence job: the role's own target, else the
/// pump's primary target, else the legacy implicit default.
#[must_use]
pub fn target_choice(ctx: &Ctx, role_target: Option<&str>) -> TargetChoice {
    let named = |id: &str| match ctx.cfg.for_target(id) {
        Some(cfg) => TargetChoice::Named {
            id: id.to_string(),
            main: cfg.main,
        },
        None => TargetChoice::Unavailable { id: id.to_string() },
    };
    if let Some(id) = role_target.map(str::trim).filter(|id| !id.is_empty()) {
        return named(id);
    }
    ctx.cfg
        .primary_target()
        .map_or(TargetChoice::Legacy, |id| named(&id))
}

/// The main ref a cadence job starts from for `choice`.
#[must_use]
pub fn main_of(ctx: &Ctx, choice: &TargetChoice) -> String {
    match choice {
        TargetChoice::Named { main, .. } => main.clone(),
        TargetChoice::Legacy | TargetChoice::Unavailable { .. } => ctx.cfg.main.clone(),
    }
}

/// Whether the selected target cannot be dispatched on at all.
#[must_use]
pub fn unavailable(choice: &TargetChoice) -> bool {
    matches!(choice, TargetChoice::Unavailable { .. })
}

/// Apply the shared target selection to a cadence job spec. Never uses the
/// dispatcher's own `from`.
pub fn apply_target(spec: &mut JobSpec, ctx: &Ctx, choice: &TargetChoice) {
    spec.target = match choice {
        TargetChoice::Named { id, .. } => Some(id.clone()),
        TargetChoice::Legacy | TargetChoice::Unavailable { .. } => None,
    };
    spec.from_ref = Some(main_of(ctx, choice));
}

/// The fingerprint of a command check for `role`, `choice` and `command`.
#[must_use]
pub fn fingerprint_of(role: &str, choice: &TargetChoice, command: &str) -> String {
    match choice {
        TargetChoice::Named { id, main } => check::fingerprint(role, id, main, command),
        TargetChoice::Legacy | TargetChoice::Unavailable { .. } => {
            check::fingerprint(role, "", "", command)
        }
    }
}

/// Whether a command role may start another execution now.
///
/// The command requires no charter, ignores open wants and needs, and ignores
/// the role's outbound signal age: the first run is immediately due, and a
/// completed green or red run waits its cadence from completion time. An
/// environment failure waits its backoff instead. A pending, running or
/// unapplied execution blocks another one whatever the configuration says.
#[must_use]
pub fn ready(st: &State, role: &RoleScan, check: Option<&CommandCheck>, now: u64) -> bool {
    let Some(command) = role.command.as_deref().filter(|c| !c.trim().is_empty()) else {
        return false;
    };
    if command.trim().is_empty() || role.target_unavailable {
        return false;
    }
    if DISABLED_STATUSES
        .iter()
        .any(|status| role.status.trim().eq_ignore_ascii_case(status))
    {
        return false;
    }
    let Some(cadence_secs) = role.cadence.as_deref().and_then(parse_secs) else {
        return false;
    };
    let Some(latest) = check.and_then(|check| check.latest.as_ref()) else {
        return true;
    };
    if blocks(st, latest) {
        return false;
    }
    latest
        .retry_at
        .or_else(|| latest.ended_at.map(|at| at.saturating_add(cadence_secs)))
        .is_none_or(|due| now >= due)
}

/// Whether a saved execution still holds the single-flight slot.
fn blocks(st: &State, latest: &CommandExecution) -> bool {
    let Some(job) = st.jobs.get(&latest.job) else {
        return false;
    };
    !job.status.is_terminal() || job.applied_at.is_none()
}

/// Build the command job spec: the exact command, a descriptive brief only,
/// the role's pool candidates, the dispatch budget, and read-only execution
/// with no branch or publication privileges.
#[must_use]
pub fn spec(
    ctx: &Ctx,
    role: &RoleView,
    scan: &RoleScan,
    choice: &TargetChoice,
    execution: &CommandExecution,
) -> JobSpec {
    let subject = format!(
        "cadence command {} {}",
        role.name,
        epoch_label(execution.started_at)
    );
    let body = brief(ctx, scan, choice, execution);
    let mut spec = JobSpec::new(
        String::from("cadence-pending"),
        String::from("cadence"),
        role.name.clone(),
        subject,
        body,
    );
    spec.command = Some(execution.command.clone());
    spec.pool_candidates = scan.pool.iter().cloned().collect();
    spec.budget = ctx.cfg.dispatch.budget;
    spec.read_only = true;
    apply_target(&mut spec, ctx, choice);
    spec.payload = serde_json::json!({
        "cadence": role.cadence,
        "scheduled_at": execution.started_at,
        "attempt": execution.attempt,
        "fingerprint": execution.fingerprint,
        "command_check": true,
    });
    spec
}

fn brief(
    ctx: &Ctx,
    scan: &RoleScan,
    choice: &TargetChoice,
    execution: &CommandExecution,
) -> String {
    let target = match choice {
        TargetChoice::Named { id, .. } => id.as_str(),
        TargetChoice::Legacy | TargetChoice::Unavailable { .. } => "legacy",
    };
    format!(
        "Run the configured command check for role `{}`.\n\
         Command: {}\n\
         Repository target: {target} at {}\n\
         This run is read-only: report the outcome and file what the result needs.\n\
         Do not commit, branch, push, publish, or merge.\n\
         Scheduled at {}.",
        scan.name,
        execution.command,
        main_of(ctx, choice),
        epoch_label(execution.started_at),
    )
}

/// Build and enqueue the command job for `scan`, then record the execution in
/// the durable check state.
pub fn enqueue(
    ctx: &Ctx,
    st: &mut State,
    cadence: &mut CadenceState,
    role: &RoleView,
    scan: &RoleScan,
    now: u64,
) {
    let Some(command) = scan.command.clone() else {
        return;
    };
    let choice = target_choice(ctx, role.target.as_deref());
    let fingerprint = fingerprint_of(&role.name, &choice, &command);
    let entry = cadence.checks.entry(scan.name.clone()).or_default();
    let attempt = next_attempt(entry);
    let execution = CommandExecution {
        job: String::new(),
        fingerprint: fingerprint.clone(),
        target: match &choice {
            TargetChoice::Named { id, .. } => id.clone(),
            TargetChoice::Legacy | TargetChoice::Unavailable { .. } => String::new(),
        },
        main: main_of(ctx, &choice),
        command,
        role: role.name.clone(),
        attempt,
        started_at: now,
        ..CommandExecution::default()
    };
    let mut spec = spec(ctx, role, scan, &choice, &execution);
    spec.id = format!(
        "cadence-{}-{}-{attempt}-{now}",
        role.name,
        &fingerprint[..8]
    );
    let job_id = spec.id.clone();
    if let Err(error) = jobs::enqueue(st, spec) {
        ctx.log(&format!("cadence command {} failed: {error}", scan.name));
        return;
    }
    let mut execution = execution;
    execution.job = job_id;
    entry.record(execution);
    ctx.log(&format!(
        "cadence command queued {} attempt {attempt}",
        scan.name
    ));
}

fn next_attempt(check: &CommandCheck) -> u32 {
    match check.latest.as_ref() {
        Some(latest) if latest.outcome.as_deref() == Some(check::ENVIRONMENT) => {
            latest.attempt.saturating_add(1).max(1)
        }
        _ => 1,
    }
}

fn parse_secs(raw: &str) -> Option<u64> {
    match parse_duration_secs(raw) {
        Ok(secs) => Some(secs),
        Err(_) => None,
    }
}
