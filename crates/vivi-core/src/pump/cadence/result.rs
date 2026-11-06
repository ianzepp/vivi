//! Command execution evidence: the tip gate before a launch and the result of
//! one afterwards.
//!
//! A command check is green only when the runner started the job on a commit
//! the machine records and the command exited 0, and red only when the code
//! itself failed. Everything else — a missing tool, a lost slot, a receipt
//! without an exit, an unreadable target tip — is the environment: it retries
//! with backoff and files no need. A green that still owes a recovery report
//! and a genuine red stay staged, holding the single-flight slot until
//! [`super::report`] files, replies on or closes the red need they owe.

use crate::mailspace::{Mailspace, PoolEventRow};
use crate::pump::flow::verify;
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobStatus, PoolJob, rung_result};
use crate::pump::state::State;

use super::check::{self, CommandCheck, CommandException};
use super::command::{self, TargetChoice};
use super::{CadenceState, RoleScan, backoff_secs};

/// Exception kind for a cadence command with no unambiguous configured target.
pub const TARGET_EXCEPTION: &str = "cadence-command-target-unavailable";

/// Exception kind for a command check whose target main tip could not be read.
pub const TIP_EXCEPTION: &str = "cadence-command-main-tip";

/// Exception kind for a command execution the environment stopped.
pub const ENV_EXCEPTION: &str = "cadence-command-environment";

/// The result still owes the red need a genuine failure must file.
pub const AWAITING_RED: &str = "red";

/// The result still owes a recovery report over an open red need.
pub const AWAITING_RECOVERY: &str = "recovery";

/// What a due command execution may do once the target main tip is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TipGate {
    /// The tip moved, or nothing was green yet: launch.
    Run,
    /// The tip is the commit this fingerprint last passed on: wait.
    Unchanged(String),
    /// The tip could not be established: a cadence-specific exception.
    Failed(String),
}

/// Read the selected target's configured main and compare it with the commit
/// this check last passed on. The main ref stays the job's start ref; the
/// green commit is remembered separately from any scan-time SHA.
#[must_use]
pub fn tip_gate(
    ctx: &Ctx,
    name: &str,
    command_text: &str,
    choice: &TargetChoice,
    check: Option<&CommandCheck>,
) -> TipGate {
    let (target, main) = match choice {
        TargetChoice::Named { id, .. } => match ctx.for_target(id) {
            Some(target) => {
                let main = target.cfg.main.clone();
                (target, main)
            }
            None => return TipGate::Failed(format!("target {id} has no repository")),
        },
        TargetChoice::Legacy | TargetChoice::Ambiguous => (ctx.clone(), ctx.cfg.main.clone()),
        TargetChoice::Unavailable { id } => {
            return TipGate::Failed(format!("target {id} is not configured"));
        }
    };
    let green = check
        .filter(|check| check.fingerprint == command::fingerprint_of(name, choice, command_text))
        .and_then(|check| check.last_green_commit.clone());
    match verify::remote_tip(&target, &target.cfg.remote, &main) {
        Ok(Some(tip)) if green.as_deref() == Some(tip.as_str()) => TipGate::Unchanged(tip),
        Ok(Some(_)) => TipGate::Run,
        Ok(None) => TipGate::Failed(format!("target main {main} is absent")),
        Err(reason) => TipGate::Failed(format!("target main {main} unreadable: {reason}")),
    }
}

/// Whether a due command check may launch now. An unchanged tip persists
/// `waiting_main` for the status projection; an unreadable one raises a
/// cadence-specific exception and backs off. Neither files a need.
pub fn gate_due(ctx: &Ctx, cadence: &mut CadenceState, scan: &RoleScan, now: u64) -> bool {
    let Some(command_text) = scan.command.clone() else {
        return false;
    };
    let choice = command::target_choice(ctx, scan.target.as_deref());
    let entry = cadence.checks.entry(scan.name.clone()).or_default();
    if entry.pending_exception(now).is_some() {
        return false;
    }
    let target_reason = match &choice {
        TargetChoice::Ambiguous => Some(String::from(
            "multiple repository targets are configured without a primary target",
        )),
        TargetChoice::Unavailable { id } => Some(format!("target {id} is not configured")),
        TargetChoice::Named { .. } | TargetChoice::Legacy => None,
    };
    if let Some(reason) = target_reason {
        back_off(ctx, &scan.name, entry, TARGET_EXCEPTION, &reason, now);
        return false;
    }
    match tip_gate(ctx, &scan.name, &command_text, &choice, Some(entry)) {
        TipGate::Run => {
            entry.waiting_main = None;
            entry.exception = None;
            true
        }
        TipGate::Unchanged(tip) => {
            // Logged once per tip: the gate is asked again on every tick.
            if entry.waiting_main.as_deref() != Some(tip.as_str()) {
                ctx.log(&format!(
                    "cadence command {} waits for main to move past {}",
                    scan.name,
                    tip.get(..12).unwrap_or(&tip)
                ));
            }
            entry.waiting_main = Some(tip);
            entry.exception = None;
            false
        }
        TipGate::Failed(reason) => {
            back_off(ctx, &scan.name, entry, TIP_EXCEPTION, &reason, now);
            false
        }
    }
}

/// Raise the tip exception and record the backoff that follows it.
fn back_off(ctx: &Ctx, name: &str, entry: &mut CommandCheck, kind: &str, reason: &str, now: u64) {
    let attempt = entry
        .exception
        .as_ref()
        .map_or(1, |exception| exception.attempt.saturating_add(1).max(1));
    entry.waiting_main = None;
    entry.exception = Some(CommandException {
        kind: String::from(kind),
        reason: String::from(reason),
        at: now,
        attempt,
        retry_at: Some(now.saturating_add(backoff_secs(attempt))),
    });
    ctx.exception(kind, name, reason);
}

/// What one terminal execution amounts to.
struct Verdict {
    outcome: &'static str,
    exit: Option<i32>,
    tested: Option<String>,
    at: u64,
    reason: String,
}

/// Classify a terminal command job from machine facts and the execution
/// evidence, never from model text.
fn classify(job: &PoolJob, tested: Option<String>) -> Verdict {
    let rung = rung_result::parse(job);
    let at = job.outcome.as_ref().map_or(0, |outcome| outcome.at);
    let environment = |reason: String, tested: Option<String>| Verdict {
        outcome: check::ENVIRONMENT,
        exit: rung.exit,
        tested,
        at,
        reason,
    };
    if let Some(failure) = rung_result::env_failure(job) {
        let kind = match failure.kind {
            rung_result::EnvKind::ToolMissing => "missing tool",
            rung_result::EnvKind::HostSetup => "host setup",
        };
        return environment(format!("{kind}: {}", failure.evidence), tested);
    }
    if job.status != JobStatus::Finished {
        return environment(
            format!("{job:?} terminal without a completed execution"),
            tested,
        );
    }
    let Some(exit) = rung.exit else {
        return environment(String::from("the receipt carries no agent exit"), tested);
    };
    let Some(tested) = tested else {
        return environment(
            String::from("no runner start commit for this execution"),
            None,
        );
    };
    if exit == 0 {
        return Verdict {
            outcome: check::GREEN,
            exit: Some(exit),
            tested: Some(tested),
            at,
            reason: String::new(),
        };
    }
    Verdict {
        outcome: check::RED,
        exit: Some(exit),
        tested: Some(tested),
        at,
        reason: format!("agent exit {exit}"),
    }
}

/// The full tested commit of the matched execution: the `start` the runner
/// recorded on the `pool_collected` event of this job's own task. The pool's
/// runner id need not equal the durable pump job id, so the task association
/// is what matches.
#[must_use]
pub fn tested_commit(events: &[PoolEventRow], task: Option<&str>) -> Option<String> {
    let task = task.filter(|task| !task.is_empty())?;
    events.iter().rev().find_map(|event| {
        if event.event_type != "pool_collected" || event.handle != task {
            return None;
        }
        let Ok(note) = serde_json::from_str::<serde_json::Value>(&event.note) else {
            return None;
        };
        let start = note.get("start")?.as_str()?;
        full_sha(start).then(|| start.to_owned())
    })
}

/// Whether `sha` is a full commit SHA: a shortened human receipt hash is not
/// execution provenance.
#[must_use]
pub fn full_sha(sha: &str) -> bool {
    (40..=64).contains(&sha.len()) && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Collect the terminal results of command executions. Evidence needs the
/// mailspace, so a tick without one consumes nothing. A green that owes no
/// report and an environment failure are applied at once; a red and a green
/// that owes a recovery report are staged, which keeps the single-flight slot
/// closed until [`super::report`] pays and applies them.
pub fn collect(
    ctx: &Ctx,
    st: &mut State,
    cadence: &mut CadenceState,
    mailspace: &Mailspace,
    now: u64,
) {
    let events = match mailspace.pool_events() {
        Ok(events) => events,
        Err(error) => {
            ctx.log(&format!("cadence command evidence unavailable: {error}"));
            return;
        }
    };
    let mut apply = Vec::new();
    for (name, entry) in &mut cadence.checks {
        let Some(job_id) = entry
            .latest
            .as_ref()
            .filter(|latest| latest.ended_at.is_none() && !latest.job.is_empty())
            .map(|latest| latest.job.clone())
        else {
            continue;
        };
        let Some(job) = st.jobs.get(&job_id) else {
            continue;
        };
        if !job.status.is_terminal() {
            continue;
        }
        let verdict = classify(job, tested_commit(&events, job.task.as_deref()));
        stage(ctx, name, entry, job_id, &verdict, &mut apply, now);
    }
    for id in apply {
        jobs::mark_applied(st, &id, now);
    }
}

/// Stage one verdict: the run context records the exit, outcome and tested
/// commit, then whatever the result still owes decides whether its job is
/// applied now or holds the single-flight slot.
fn stage(
    ctx: &Ctx,
    name: &str,
    entry: &mut CommandCheck,
    job: String,
    verdict: &Verdict,
    apply: &mut Vec<String>,
    now: u64,
) {
    let recovery = entry.red_need.is_some();
    let attempt = entry
        .latest
        .as_ref()
        .map_or(1, |latest| latest.attempt.max(1));
    let awaiting = match verdict.outcome {
        check::GREEN if recovery => Some(String::from(AWAITING_RECOVERY)),
        check::GREEN => {
            entry.last_green_commit.clone_from(&verdict.tested);
            apply.push(job);
            None
        }
        check::ENVIRONMENT => {
            retry_after_environment(ctx, name, entry, verdict, attempt, now);
            apply.push(job);
            None
        }
        _ => Some(String::from(AWAITING_RED)),
    };
    if let Some(latest) = entry.latest.as_mut() {
        latest.exit = verdict.exit;
        latest.tested_commit.clone_from(&verdict.tested);
        latest.ended_at = Some(verdict.at);
        latest.outcome = Some(String::from(verdict.outcome));
        latest.awaiting = awaiting;
    }
}

/// Record the environment retry: exponential backoff from the completion time
/// of the attempt, and a cadence-specific exception instead of a need.
fn retry_after_environment(
    ctx: &Ctx,
    name: &str,
    entry: &mut CommandCheck,
    verdict: &Verdict,
    attempt: u32,
    now: u64,
) {
    let retry_at = verdict.at.saturating_add(backoff_secs(attempt));
    entry.exception = Some(CommandException {
        kind: String::from(ENV_EXCEPTION),
        reason: verdict.reason.clone(),
        at: now,
        attempt,
        retry_at: Some(retry_at),
    });
    if let Some(latest) = entry.latest.as_mut() {
        latest.retry_at = Some(retry_at);
        latest.due_at = Some(retry_at);
    }
    ctx.exception(ENV_EXCEPTION, name, &verdict.reason);
}
