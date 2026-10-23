//! Role auto-run cadence state; receipts persist in state.extra.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::duration::parse_duration_secs;
use crate::mailspace::{Mailspace, RoleView};
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobSpec};
use crate::pump::state::State;
use crate::role_schedule::{self, state_label};
use crate::routing::resolve_read_only;

/// The `state.extra` key holding the cadence state.
pub const EXTRA_KEY: &str = "cadence";

/// Role statuses that disable auto-run.
pub const DISABLED_STATUSES: [&str; 3] = ["paused", "parked", "retired"];

/// Seconds between role scans.
pub const SCAN_EVERY: u64 = 60;

/// Open wants a role may hold before auto-run declines to file another.
pub const MAX_OPEN_WANTS: usize = 8;

/// Open needs a role may hold before another cadence run is skipped.
pub const MAX_OPEN_NEEDS: usize = 8;

/// Seconds of backoff for the first retry of a failed run.
pub const BACKOFF_BASE: u64 = 60;

/// One role as seen by the cadence scan.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RoleScan {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub cadence: Option<String>,
    #[serde(default)]
    pub pool: Option<String>,
    #[serde(default)]
    pub open_wants: usize,
    #[serde(default)]
    pub open_needs: usize,
    #[serde(default)]
    pub has_charter: bool,
    #[serde(default)]
    pub schedule_state: String,
    #[serde(default)]
    pub last_signal_at: Option<String>,
    #[serde(default)]
    pub next_due_at: Option<u64>,
}

/// One auto-run of a role: its durable job, task, timing, and outcome.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RoleRun {
    /// The durable pool job id.
    #[serde(default)]
    pub job: String,
    /// The Vivi task handle, once the job has been dispatched.
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub started_at: u64,
    #[serde(default)]
    pub ended_at: Option<u64>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default)]
    pub retry_at: Option<u64>,
}

/// Cadence state: last scan time, the roles seen, and their runs.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct CadenceState {
    #[serde(default)]
    pub runs: BTreeMap<String, RoleRun>,
    #[serde(default)]
    pub scans: BTreeMap<String, RoleScan>,
    #[serde(default)]
    pub scanned_at: u64,
}

/// Read the cadence state, falling back to defaults for absent or bad payloads.
pub fn state(st: &State) -> CadenceState {
    let Some(value) = st.extra.get(EXTRA_KEY).cloned() else {
        return CadenceState::default();
    };
    serde_json::from_value(value).unwrap_or_default()
}

/// Store the cadence state in `state.extra`.
pub fn set_state(st: &mut State, cadence: &CadenceState) {
    if let Ok(value) = serde_json::to_value(cadence) {
        st.extra.insert(String::from(EXTRA_KEY), value);
    }
}

/// Seconds to wait before retrying a run that has failed `attempt` times.
#[must_use]
pub fn backoff_secs(attempt: u32) -> u64 {
    BACKOFF_BASE << attempt.saturating_sub(1).min(5)
}

/// Whether a role snapshot is ready for a cadence dispatch.
///
/// Role kind does not restrict cadence. The role's configured kind and
/// read-only override are carried to normal pool dispatch, where they decide
/// what the job can change.
#[must_use]
pub fn launchable(role: &RoleScan, previous: Option<&RoleRun>, now: u64) -> bool {
    let Some(cadence) = role.cadence.as_deref() else {
        return false;
    };
    let Ok(cadence_secs) = parse_duration_secs(cadence) else {
        return false;
    };
    if !role.has_charter
        || role.open_wants >= MAX_OPEN_WANTS
        || role.open_needs >= MAX_OPEN_NEEDS
        || DISABLED_STATUSES
            .iter()
            .any(|status| role.status.trim().eq_ignore_ascii_case(status))
        || !matches!(role.schedule_state.as_str(), "never" | "due" | "overdue")
    {
        return false;
    }
    if let Some(run) = previous {
        if run.ended_at.is_none() || run.retry_at.is_some_and(|retry_at| retry_at > now) {
            return false;
        }
        if run.outcome.as_deref() == Some("success") {
            let next_run_at = run
                .ended_at
                .unwrap_or_default()
                .saturating_add(cadence_secs.saturating_add(cadence_secs / 10));
            if now < next_run_at {
                return false;
            }
        }
    }
    true
}

/// Compute the next due epoch from a schedule report. A role with no signal,
/// or one already due, is due at `now`.
#[must_use]
pub fn next_due_at(report: &role_schedule::ScheduleReport, now: u64) -> Option<u64> {
    let cadence = report.cadence_seconds?;
    let due_after = cadence.saturating_add(cadence / 10);
    Some(now.saturating_add(due_after.saturating_sub(report.age_seconds.unwrap_or(0))))
}

/// Scan roles and enqueue due cadence jobs. The run loop calls this after
/// reconciling completed pool jobs and before regular dispatch.
pub fn prepare_due(ctx: &Ctx, st: &mut State) {
    let now = ctx.now();
    let mut cadence = state(st);
    complete_runs(ctx, st, &mut cadence, now);
    let mailspace = match Mailspace::discover(Some(&ctx.project)) {
        Ok(mailspace) => mailspace,
        Err(error) => {
            ctx.log(&format!("cadence scan skipped: {error}"));
            set_state(st, &cadence);
            return;
        }
    };
    let scans = scan_roles(ctx, &mailspace, now);
    for scan in scans.values() {
        let previous = cadence.runs.get(&scan.name);
        if !launchable(scan, previous, now) {
            continue;
        }
        enqueue_due(ctx, st, &mailspace, &mut cadence, scan, now);
    }
    cadence.scans = scans;
    cadence.scanned_at = now;
    set_state(st, &cadence);
}

/// Pump status lines for every role that currently has a cadence.
#[must_use]
pub fn status_lines(ctx: &Ctx, st: &State) -> Vec<String> {
    let cadence = state(st);
    let now = ctx.now();
    cadence
        .scans
        .iter()
        .filter(|(_, scan)| scan.cadence.is_some())
        .map(|(name, scan)| {
            let next_due = scan
                .next_due_at
                .map_or_else(|| String::from("unknown"), epoch_label);
            let due = scan
                .next_due_at
                .is_some_and(|at| at <= now)
                .then_some(" due")
                .unwrap_or("");
            let result = cadence
                .runs
                .get(name)
                .map_or_else(|| String::from("none"), |run| run_status(run));
            let status = if !scan.has_charter {
                "skipped-no-charter"
            } else if scan.open_wants >= MAX_OPEN_WANTS {
                "skipped-open-wants-cap"
            } else if scan.open_needs >= MAX_OPEN_NEEDS {
                "skipped-open-needs-cap"
            } else {
                scan.schedule_state.as_str()
            };
            format!(
                "cadence {name} kind {} state {status} mode {} next {next_due}{due} last {result}",
                scan.kind.as_deref().unwrap_or("unknown"),
                if scan.read_only { "read-only" } else { "write" }
            )
        })
        .collect()
}

fn scan_roles(ctx: &Ctx, mailspace: &Mailspace, now: u64) -> BTreeMap<String, RoleScan> {
    let mut scans = BTreeMap::new();
    for identity in &mailspace.config.identities {
        if identity.cadence.is_none() {
            continue;
        }
        let role = match mailspace.role_view(&identity.name) {
            Ok(role) => role,
            Err(error) => {
                ctx.log(&format!("cadence scan {} failed: {error}", identity.name));
                continue;
            }
        };
        let schedule = match mailspace.schedule_report(&role.name) {
            Ok(schedule) => schedule,
            Err(error) => {
                ctx.log(&format!("cadence schedule {} failed: {error}", role.name));
                continue;
            }
        };
        let open_wants = match mailspace.list_kind_all(Some(&role.name), "sent", "want") {
            Ok(wants) => wants.len(),
            Err(error) => {
                ctx.log(&format!("cadence wants {} failed: {error}", role.name));
                continue;
            }
        };
        let open_needs = match mailspace.list_kind_all(Some(&role.name), "sent", "need") {
            Ok(needs) => needs.len(),
            Err(error) => {
                ctx.log(&format!("cadence needs {} failed: {error}", role.name));
                continue;
            }
        };
        scans.insert(
            role.name.clone(),
            role_scan(role, schedule, open_wants, open_needs, now),
        );
    }
    scans
}

fn role_scan(
    role: RoleView,
    schedule: role_schedule::ScheduleReport,
    open_wants: usize,
    open_needs: usize,
    now: u64,
) -> RoleScan {
    let read_only = resolve_read_only(false, role.read_only, role.kind.as_deref());
    let next_due_at = next_due_at(&schedule, now);
    RoleScan {
        name: role.name,
        kind: role.kind,
        status: role.status,
        read_only,
        cadence: role.cadence,
        pool: role.pool,
        open_wants,
        open_needs,
        has_charter: role.has_charter,
        schedule_state: String::from(state_label(schedule.state)),
        last_signal_at: schedule.last_signal_at,
        next_due_at,
    }
}

fn enqueue_due(
    ctx: &Ctx,
    st: &mut State,
    mailspace: &Mailspace,
    cadence: &mut CadenceState,
    scan: &RoleScan,
    now: u64,
) {
    let Ok(role) = mailspace.role_view(&scan.name) else {
        return;
    };
    let attempt = next_attempt(cadence.runs.get(&scan.name));
    let spec = job_spec(ctx, st, &role, scan, attempt, now);
    let job_id = spec.id.clone();
    let branch = spec.branch.clone();
    let read_only = spec.read_only;
    if let Err(error) = jobs::enqueue(st, spec) {
        ctx.log(&format!("cadence enqueue {} failed: {error}", scan.name));
        return;
    }
    let task = st
        .jobs
        .get(&job_id)
        .and_then(|job| job.task.clone())
        .unwrap_or_default();
    cadence.runs.insert(
        scan.name.clone(),
        RoleRun {
            job: job_id,
            task,
            branch,
            read_only,
            started_at: now,
            ended_at: None,
            outcome: None,
            attempt,
            retry_at: None,
        },
    );
    ctx.log(&format!("cadence queued {} attempt {attempt}", scan.name));
}

fn job_spec(
    ctx: &Ctx,
    st: &State,
    role: &RoleView,
    scan: &RoleScan,
    attempt: u32,
    now: u64,
) -> JobSpec {
    let is_digest = role
        .kind
        .as_deref()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("digest"));
    let is_mind = role
        .kind
        .as_deref()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("mind"));
    let cadence_secs = scan
        .cadence
        .as_deref()
        .and_then(parse_secs)
        .unwrap_or(86_400);
    let id = format!(
        "cadence-{}-{}-{attempt}",
        role.name,
        scan.next_due_at.unwrap_or(now)
    );
    let (kind, subject, body) = if is_digest {
        digest_job(ctx, role, scan, cadence_secs, now)
    } else if is_mind {
        crate::pump::oversight::job_parts(ctx, st, role, now)
    } else {
        (
            String::from("cadence"),
            format!("cadence {} {}", role.name, epoch_label(now)),
            cadence_run_text(role, now),
        )
    };
    finish_cadence_spec(
        ctx, role, scan, attempt, now, is_mind, id, kind, subject, body,
    )
}

fn finish_cadence_spec(
    ctx: &Ctx,
    role: &RoleView,
    scan: &RoleScan,
    attempt: u32,
    now: u64,
    is_mind: bool,
    id: String,
    kind: String,
    subject: String,
    body: String,
) -> JobSpec {
    let read_only = is_mind || resolve_read_only(false, role.read_only, role.kind.as_deref());
    let branch = (!read_only).then(|| {
        format!(
            "cadence/{}-{}-a{attempt}",
            role.name,
            scan.next_due_at.unwrap_or(now)
        )
    });
    let mut spec = JobSpec::new(id, kind, role.name.clone(), subject, body);
    spec.target = branch.as_ref().map(|_| ctx.target_id.clone());
    spec.branch = branch;
    spec.budget = ctx.cfg.dispatch.budget;
    spec.integration_required = !read_only;
    spec.read_only = read_only;
    spec.payload = serde_json::json!({
        "cadence": role.cadence,
        "scheduled_at": now,
        "attempt": attempt,
        "open_wants": scan.open_wants,
        "open_needs": scan.open_needs,
        "oversight": is_mind,
    });
    spec
}

fn digest_job(
    ctx: &Ctx,
    role: &RoleView,
    scan: &RoleScan,
    first_run_secs: u64,
    now: u64,
) -> (String, String, String) {
    let now = epoch_date(now);
    let last_run = scan.last_signal_at.as_deref().and_then(parse_datetime);
    let brief = crate::pool::role_dispatch::digest_brief(now, last_run, first_run_secs);
    let body = crate::pool::role_dispatch::digest_run_text(&brief, &ctx.project, &role.name);
    (String::from("digest"), brief.subject.clone(), body)
}

fn cadence_run_text(role: &RoleView, now: u64) -> String {
    let head_rule = match role.kind.as_deref() {
        Some(kind) if kind.eq_ignore_ascii_case("head") && role.name == "head-ceo" => {
            "As head-ceo, review open wants. Promote a well-scoped want to a linked need.\n\
             For an unclear want, file a research need with `need send --mark research`\n\
             and a `for: <want-handle>` line. Write the mark before `vivi pump send`.\n\
             Before filing one, read the open needs marked research and skip a want\n\
             that one of them already names in its `for:` line. File at most two\n\
             research needs for one want in total; after that, promote or drop it.\n\
             On the next wake, read the analysis on the want, then promote the want,\n\
             drop it, or file another research need within that cap. Do not implement work.\n\
             End your reply with a TOML envelope between `+++` lines:\n\
             `answer` is `reviewed` or `no-op`; `promoted`, `research` and `dropped`\n\
             are lists of handles. Example:\n\
             +++\n\
             answer = \"reviewed\"\n\
             promoted = [\"3f9a1c2e\"]\n\
             research = []\n\
             dropped = [\"b71d04aa\"]\n\
             +++"
        }
        Some(kind) if kind.eq_ignore_ascii_case("head") => {
            "As a head role, file findings as wants only; do not file needs."
        }
        _ => "",
    };
    format!(
        "Run your configured role charter as a periodic task for role `{}`.\n\
         Follow the charter's instructions for this run. If there is nothing to do,\n\
         reply with a short no-op report. Stay within the role's normal permissions.\n\
         {head_rule}\n\
         Scheduled at {}.",
        role.name,
        epoch_label(now),
    )
}

fn complete_runs(ctx: &Ctx, st: &mut State, cadence: &mut CadenceState, now: u64) {
    let mut apply = Vec::new();
    for run in cadence.runs.values_mut() {
        if run.job.is_empty() {
            continue;
        }
        if let Some(task) = st.jobs.get(&run.job).and_then(|job| job.task.clone()) {
            run.task = task;
        }
        if run.ended_at.is_some() {
            continue;
        }
        let Some(outcome) = jobs::completion(st, &run.job) else {
            continue;
        };
        let success = outcome.state == "finished" && outcome.agent_success() == Some(true);
        if success && run.branch.is_some() {
            if let Some(error) = jobs::integration_error(st, &run.job) {
                finish_failed_run(run, now, &format!("integration-failed: {error}"));
                apply.push(run.job.clone());
                continue;
            }
            if !jobs::branch_landed(st, &run.job) {
                run.outcome = Some(String::from("awaiting-merge-gate"));
                continue;
            }
        }
        let ended_at = if run.branch.is_some() {
            now
        } else {
            outcome.at
        };
        run.ended_at = Some(ended_at);
        run.outcome = Some(if success {
            String::from("success")
        } else if outcome.state == "timeout" || outcome.state == "lost" {
            outcome.state.clone()
        } else if outcome.agent_success() == Some(false) {
            String::from("failed")
        } else {
            String::from("unknown")
        });
        if success {
            run.attempt = 0;
            run.retry_at = None;
        } else {
            run.retry_at = Some(ended_at.saturating_add(backoff_secs(run.attempt)));
        }
        apply.push(run.job.clone());
    }
    settle_completed_runs(ctx, st, apply, now);
}

fn settle_completed_runs(ctx: &Ctx, st: &mut State, ids: Vec<String>, now: u64) {
    for id in ids {
        jobs::mark_applied(st, &id, now);
        if crate::pump::oversight::due(st, &id) {
            crate::pump::oversight::apply(ctx, st, &id);
        }
    }
}

fn finish_failed_run(run: &mut RoleRun, at: u64, outcome: &str) {
    run.ended_at = Some(at);
    run.outcome = Some(outcome.to_string());
    run.retry_at = Some(at.saturating_add(backoff_secs(run.attempt)));
}

fn next_attempt(previous: Option<&RoleRun>) -> u32 {
    previous.map_or(1, |run| {
        if run.outcome.as_deref() == Some("success") {
            1
        } else {
            run.attempt.saturating_add(1).max(1)
        }
    })
}

fn parse_secs(raw: &str) -> Option<u64> {
    match parse_duration_secs(raw) {
        Ok(secs) => Some(secs),
        Err(_) => None,
    }
}

fn parse_datetime(raw: &str) -> Option<DateTime<Utc>> {
    match DateTime::parse_from_rfc3339(raw) {
        Ok(date) => Some(date.with_timezone(&Utc)),
        Err(_) => None,
    }
}

fn epoch_date(epoch: u64) -> DateTime<Utc> {
    let seconds = i64::try_from(epoch).unwrap_or(i64::MAX);
    DateTime::from_timestamp(seconds, 0).unwrap_or_else(Utc::now)
}

fn epoch_label(epoch: u64) -> String {
    epoch_date(epoch).to_rfc3339()
}

fn run_status(run: &RoleRun) -> String {
    match run.ended_at {
        Some(ended_at) => format!(
            "{} {}",
            run.outcome.as_deref().unwrap_or("unknown"),
            epoch_label(ended_at)
        ),
        None => format!(
            "{} {}",
            run.outcome.as_deref().unwrap_or("in-flight"),
            if run.task.is_empty() {
                &run.job
            } else {
                &run.task
            }
        ),
    }
}

#[cfg(test)]
#[path = "cadence_test.rs"]
mod tests;

#[cfg(test)]
#[path = "cadence_write_test.rs"]
mod write_tests;
