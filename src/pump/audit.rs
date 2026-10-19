//! Red-gate auditing state and the auditor's fix-forward brief.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use serde::{Deserialize, Serialize};

use crate::pump::dispatch::slots;
use crate::pump::envfail::HostFailure;
use crate::pump::gatefix::{fix_need, send_fix_need, take_residuals};
use crate::pump::host::Ctx;
use crate::pump::jobs::{
    JobOutcome, JobSpec, JobStatus, PoolJob, completion, enqueue, mark_applied,
};
use crate::pump::state::{NeedRecord, State, Status, Unit};
use crate::routing::RoleRouting;

/// Seconds before an audit that could not start is retried.
const RETRY_SECONDS: u64 = 60;

/// Failed launched audits before the run stops for operator attention.
pub const MAX_AUDIT_ATTEMPTS: u64 = 3;

/// The `state.extra` key holding the live audit run.
pub(crate) const AUDIT_KEY: &str = "audit";

const NO_BLAME: &str = "Do not try to identify which merge or commit caused a \
    failure: no suspect lists, no bisecting, no culprit commit in briefs, no \
    reverts by blame. Nothing goes back to a developer as a rejected commit. \
    Identify what is broken, fix it forward, move on.";

const REPLY_FORMAT: &str = "\
End the reply with a block between the exact markers AUDIT-BEGIN and AUDIT-END \
containing only a JSON array, one object per root cause. Each object must \
contain root_cause (one line), brief (the complete fix-unit brief naming the \
failing tests and concrete fix), tests (the failing test names this cause \
covers), write_scope (files the fix may write), and proof (commands that must \
exit 0, one per failing test when possible). Use this JSON shape:\n\
AUDIT-BEGIN\n\
[\n\
  {\n\
    \"root_cause\": \"<one line>\",\n\
    \"brief\": \"<complete fix-unit brief naming failing tests and concrete fix>\",\n\
    \"tests\": [\"<failing test name>\"],\n\
    \"write_scope\": [\"<file the fix may write>\"],\n\
    \"proof\": [\"<command that exits 0>\"]\n\
  }\n\
]\n\
AUDIT-END\n\
done_when: the AUDIT block is in the reply and every cause names its tests, a \
concrete fix, a write scope and proof\n";

/// One audit run in flight, or the last one still owed a report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRun {
    /// Vivi task handle while the audit runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Durable job ID for the current audit attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    /// The fix need id owning the red names.
    #[serde(default)]
    pub need: String,
    /// The integration tip that went red.
    #[serde(default)]
    pub tip: String,
    /// The pool the auditor runs in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    /// Epoch the audit started.
    #[serde(default)]
    pub at: u64,
    /// Consecutive attempts that produced no report.
    #[serde(default)]
    pub fails: u64,
    /// Earliest epoch a retry may go out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<u64>,
    /// Number of new failing names covered by the audit.
    #[serde(default)]
    pub new: usize,
    /// Number of residual failing names covered by the audit.
    #[serde(default)]
    pub residual: usize,
    /// Sorted failing-name set that identifies this red result.
    #[serde(default)]
    pub failures: Vec<String>,
    /// Total queued audit job IDs, retained across operator retries.
    #[serde(default)]
    pub attempts: u64,
    /// This exact red result reached its bounded retry limit.
    #[serde(default)]
    pub exhausted: bool,
    /// Latest retry or terminal reason shown in status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Start the red-gate audit for `tip`.
///
/// The failing names are owned by a gate-fix need first, so promotion stays
/// held while the audit runs. The audit task then goes to the mailspace's
/// `auditor` role through the durable jobs queue: role charter, identity,
/// pool and model all come from the role config. Without a configured role,
/// the built-in auditor uses `[pump] to`, the gate pools, and the gate model
/// in read-only mode.
///
/// Nothing is sent while an audit task is open, and nothing is sent when fix
/// units already exist for this tip.
pub fn start_audit(
    ctx: &Ctx,
    st: &mut State,
    tip: &str,
    output_tail: &str,
    new: &[String],
    residual: &[(String, String)],
    prior: Option<&str>,
) {
    let failures = failure_names(new, residual);
    let previous = current(st);
    if previous
        .as_ref()
        .is_some_and(|run| run.task.is_some() || run.job_id.is_some())
    {
        return;
    }
    if let Some(run) = previous.as_ref()
        && same_failure(st, run, tip, &failures, new.len(), residual.len())
        && (run.exhausted || run.fails >= MAX_AUDIT_ATTEMPTS || run.retry_at.is_some())
    {
        return;
    }
    let base = fix_need(ctx, tip);
    if st.units.values().any(|unit| unit.need == base) {
        return;
    }
    let owner = send_fix_need(ctx, tip, new, residual).unwrap_or(base);
    for name in new {
        st.red.insert(name.clone(), owner.clone());
    }
    take_residuals(ctx, st, &owner, residual);
    let mut run = AuditRun {
        need: owner.clone(),
        tip: tip.to_owned(),
        at: ctx.now(),
        new: new.len(),
        residual: residual.len(),
        failures,
        attempts: previous
            .as_ref()
            .map_or(0, |run| run.attempts.max(run.fails)),
        ..AuditRun::default()
    };
    let brief = audit_brief(ctx, tip, output_tail, new, residual, prior);
    if !write_audit_brief(ctx, &brief) {
        defer_retry(ctx, &mut run, "audit brief could not be saved");
        store(st, Some(run));
        return;
    }
    enqueue_attempt(ctx, st, &mut run, &brief);
    store(st, Some(run));
}

/// Queue one durable audit attempt, preserving the built-in fallback contract.
fn enqueue_attempt(ctx: &Ctx, st: &mut State, run: &mut AuditRun, brief: &str) {
    let role = ctx.routing.auditor();
    let fallback = role.is_none();
    let recipient = role.map_or_else(|| ctx.cfg.to.clone(), |role| role.name.clone());
    let job_id = audit_job_id(ctx, run);
    let failures = run.new + run.residual;
    let previous_pool = run.pool.clone();
    let subject = format!(
        "[pump audit] {failures} failures at {}",
        tip_short(&run.tip)
    );
    let mut spec = JobSpec::new(job_id.clone(), "audit", recipient, subject, brief);
    let candidates = retry_pools(ctx, run);
    spec.pool_exclude = previous_pool
        .filter(|failed| candidates.iter().any(|pool| pool != failed))
        .into_iter()
        .collect();
    if fallback {
        let Some(pool) = RoleRouting::first_free(None, &candidates, &slots(ctx)) else {
            defer_retry(ctx, run, "no free auditor pool slot");
            return;
        };
        run.pool = Some(pool.clone());
        spec.pool_candidates = vec![pool];
    } else {
        spec.pool_candidates = candidates;
    }
    spec.target = Some(ctx.target_id.clone());
    spec.from_ref = Some(run.tip.clone());
    spec.budget = ctx.cfg.gate.budget;
    spec.read_only = fallback;
    spec.model = fallback.then(|| ctx.cfg.gate.model.clone());
    spec.builtin_role = fallback.then(|| String::from("auditor"));
    spec.payload = serde_json::json!({
        "need": run.need,
        "tip": run.tip,
        "new": run.new,
        "residual": run.residual,
    });
    if let Err(err) = enqueue(st, spec) {
        defer_retry(ctx, run, &format!("job enqueue failed: {err}"));
        return;
    }
    run.attempts = run.attempts.max(run.fails).saturating_add(1);
    run.job_id = Some(job_id);
    run.task = None;
    run.at = ctx.now();
    run.retry_at = None;
    ctx.log(&format!(
        "audit queued for {} ({} new, {} residual)",
        run.need, run.new, run.residual
    ));
}

/// Retry an audit whose prior pool job failed or produced no valid report.
pub fn retry_due(ctx: &Ctx, st: &mut State) {
    let Some(mut run) = current(st) else {
        return;
    };
    if run.exhausted {
        return;
    }
    if run.fails >= MAX_AUDIT_ATTEMPTS {
        let reason = run
            .last_error
            .clone()
            .unwrap_or_else(|| String::from("retry limit reached"));
        exhaust_audit(ctx, &mut run, &reason);
        store(st, Some(run));
        return;
    }
    if run.job_id.is_some() || run.task.is_some() || run.retry_at.is_none_or(|at| ctx.now() < at) {
        return;
    }
    let path = ctx.home.join("briefs").join("audit.txt");
    let Ok(brief) = fs::read_to_string(&path) else {
        defer_retry(ctx, &mut run, "saved audit brief is missing");
        store(st, Some(run));
        return;
    };
    enqueue_attempt(ctx, st, &mut run, &brief);
    store(st, Some(run));
}

/// Collect a completed durable audit job and apply its report once.
pub fn collect_completed(ctx: &Ctx, st: &mut State) {
    let Some(mut run) = current(st) else {
        return;
    };
    let Some(job_id) = run.job_id.clone() else {
        return;
    };
    let Some(outcome) = completion(st, &job_id).cloned() else {
        return;
    };
    if !mark_applied(st, &job_id, ctx.now()) {
        return;
    }
    run.task = st.jobs.get(&job_id).and_then(|job| job.task.clone());
    run.pool = st.jobs.get(&job_id).and_then(|job| job.pool.clone());
    let causes = if outcome.state == "finished" && outcome.agent_success() == Some(true) {
        parse_report(&outcome.receipt)
    } else {
        None
    };
    if let Some(causes) = causes {
        let raw = report_block(&outcome.receipt).unwrap_or_else(|| outcome.receipt.clone());
        apply_report(ctx, st, &run.need, &raw, &causes);
        if run.fails > 0 || run.attempts > 1 {
            ctx.resolve(
                "gatefix-planner-failed",
                &run.need,
                "a valid audit report arrived and fix units were created",
            );
        }
        store(st, None);
        return;
    }
    let (reason, host_failure) = failure_reason(&outcome);
    if let Some(failure) = host_failure {
        record_environment_failure(ctx, &run.need, &failure, &reason);
    }
    retry_later(ctx, &mut run, &reason);
    store(st, Some(run));
}

/// Build retry evidence from the durable pool outcome.
fn failure_reason(outcome: &JobOutcome) -> (String, Option<HostFailure>) {
    let host_failure = outcome.host_failure();
    let exit = crate::pump::attempt_record::agent_exit(&outcome.receipt)
        .map_or_else(|| String::from("unknown"), |code| code.to_string());
    let reason = if let Some(failure) = &host_failure {
        format!(
            "{}; exit={exit}; receipt tail: {}",
            failure.detail,
            receipt_tail(&outcome.receipt)
        )
    } else if outcome.state != "finished" {
        outcome.state.clone()
    } else if outcome.agent_success() != Some(true) {
        String::from("auditor did not report success")
    } else {
        String::from("auditor report was unreadable")
    };
    (reason, host_failure)
}

/// Record provider or harness evidence with the exit and receipt tail.
fn record_environment_failure(ctx: &Ctx, need: &str, failure: &HostFailure, reason: &str) {
    ctx.exception(
        "environment",
        need,
        &format!("audit {} failure: {reason}", failure.class),
    );
}

/// Write the rendered brief to `<home>/briefs/audit.txt`.
fn write_audit_brief(ctx: &Ctx, brief: &str) -> bool {
    let path = ctx.home.join("briefs").join("audit.txt");
    if let Some(dir) = path.parent()
        && let Err(err) = fs::create_dir_all(dir)
    {
        ctx.log(&format!("audit brief dir failed: {err}"));
        return false;
    }
    if let Err(err) = fs::write(&path, brief) {
        ctx.log(&format!("audit brief write failed: {err}"));
        return false;
    }
    true
}

/// Gate pools tried on a prior failed attempt are removed while alternatives
/// remain, so retries fail over before returning to the original pool.
fn retry_pools(ctx: &Ctx, run: &AuditRun) -> Vec<String> {
    let mut pools = ctx.cfg.gate.pools.clone();
    if pools.len() > 1
        && let Some(failed) = &run.pool
    {
        pools.retain(|pool| pool != failed);
    }
    pools
}

/// A stable job identity for this audit attempt.
fn audit_job_id(ctx: &Ctx, run: &AuditRun) -> String {
    format!(
        "audit-{}-{}-{}",
        safe_id(&ctx.target_id),
        safe_id(&run.need),
        run.attempts.max(run.fails).saturating_add(1)
    )
}

/// Keep only characters accepted by the durable job ID validator.
fn safe_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// Count one attempt that produced no valid audit report and arm backoff.
fn retry_later(ctx: &Ctx, run: &mut AuditRun, reason: &str) {
    run.task = None;
    run.job_id = None;
    run.fails = run.fails.saturating_add(1);
    run.last_error = Some(reason.to_owned());
    if run.fails >= MAX_AUDIT_ATTEMPTS {
        exhaust_audit(ctx, run, reason);
        return;
    }
    let delay = retry_delay(run.fails);
    run.retry_at = Some(ctx.now().saturating_add(delay));
    ctx.log(&format!(
        "audit retry {} for {} in {delay}s: {reason}",
        run.fails, run.need
    ));
}

/// Record the bounded stop and the command that can explicitly restart it.
fn exhaust_audit(ctx: &Ctx, run: &mut AuditRun, reason: &str) {
    run.retry_at = None;
    run.exhausted = true;
    run.last_error = Some(reason.to_owned());
    let text = format!(
        "audit failed {}/{} times for this red result: {reason}; run `vivi pump replan {}` to retry",
        run.fails, MAX_AUDIT_ATTEMPTS, run.need
    );
    ctx.exception("gatefix-planner-failed", &run.need, &text);
    ctx.log(&text);
}

/// Defer work that did not reach a launched audit attempt.
fn defer_retry(ctx: &Ctx, run: &mut AuditRun, reason: &str) {
    run.task = None;
    run.job_id = None;
    run.last_error = Some(reason.to_owned());
    run.retry_at = Some(ctx.now().saturating_add(RETRY_SECONDS));
    ctx.log(&format!("audit retry deferred for {}: {reason}", run.need));
}

/// Existing audit backoff, retained across the bounded retry change.
fn retry_delay(fails: u64) -> u64 {
    (RETRY_SECONDS << fails.saturating_sub(1).min(4)).min(900)
}

/// Stable sorted names from the new and residual gate failures.
fn failure_names(new: &[String], residual: &[(String, String)]) -> Vec<String> {
    new.iter()
        .cloned()
        .chain(residual.iter().map(|(name, _)| name.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Whether a stored run covers the same tip and red-name set.
fn same_failure(
    st: &State,
    run: &AuditRun,
    tip: &str,
    failures: &[String],
    new: usize,
    residual: usize,
) -> bool {
    if run.tip != tip {
        return false;
    }
    if !run.failures.is_empty() {
        return run.failures == failures;
    }
    let previous: Vec<String> = st
        .red
        .iter()
        .filter(|(_, owner)| *owner == &run.need)
        .map(|(name, _)| name.clone())
        .collect();
    if previous.is_empty() {
        run.new == new && run.residual == residual
    } else {
        previous == failures
    }
}

/// A short receipt tail for logs and environment exceptions.
fn receipt_tail(receipt: &str) -> String {
    let chars: Vec<char> = receipt.chars().collect();
    let start = chars.len().saturating_sub(300);
    chars[start..].iter().collect()
}

/// Status lines for red gatefix work, including owners with no live worker.
pub fn status_lines(_ctx: &Ctx, st: &State) -> Vec<String> {
    let mut red = BTreeMap::new();
    add_red_counts(&mut red, &st.red);
    for target in st.target_states.values() {
        add_red_counts(&mut red, &target.red);
    }
    let runs = audit_runs(st);
    for run in &runs {
        red.entry(run.need.clone()).or_insert(0);
    }
    red.into_iter()
        .map(|(need, names)| status_for_need(st, &runs, &need, names))
        .collect()
}

/// Count the failing names still owned by each gatefix need.
fn add_red_counts(counts: &mut BTreeMap<String, usize>, red: &BTreeMap<String, String>) {
    for owner in red.values() {
        *counts.entry(owner.clone()).or_default() += 1;
    }
}

/// Find audit state in the global state and every target's isolated state.
fn audit_runs(st: &State) -> Vec<AuditRun> {
    let mut runs = Vec::new();
    if let Some(run) = audit_from_extra(&st.extra) {
        runs.push(run);
    }
    for target in st.target_states.values() {
        if let Some(run) = audit_from_extra(&target.extra) {
            runs.push(run);
        }
    }
    runs
}

/// Whether a red owner already has durable audit state reported elsewhere.
pub(crate) fn has_run_for_need(st: &State, need: &str) -> bool {
    current(st).is_some_and(|run| run.need == need)
        || st
            .target_states
            .values()
            .any(|target| audit_from_extra(&target.extra).is_some_and(|run| run.need == need))
}

/// Decode one audit run from a state's forward-compatible extra map.
fn audit_from_extra(extra: &BTreeMap<String, serde_json::Value>) -> Option<AuditRun> {
    let value = extra.get(AUDIT_KEY)?.clone();
    match serde_json::from_value(value) {
        Ok(run) => Some(run),
        Err(_) => None,
    }
}

/// Render one owner's current audit and unit activity.
fn status_for_need(st: &State, runs: &[AuditRun], need: &str, red: usize) -> String {
    let run = runs.iter().find(|run| run.need.as_str() == need);
    let units = unit_counts(st, need);
    let (audit, audit_live, reason) = audit_status(st, run);
    let live = audit_live || units.active > 0;
    let action = status_action(run, need);
    let label = if live {
        "gatefix-active"
    } else {
        "gatefix-no-live-work"
    };
    format!(
        "{label} {need} red={red} audit={audit} jobs={} failures={}/{} units=active:{} queued:{} parked:{} reason={reason:?} action={action:?}",
        run.map_or(0, |run| run.attempts),
        run.map_or(0, |run| run.fails),
        MAX_AUDIT_ATTEMPTS,
        units.active,
        units.queued,
        units.parked,
    )
}

/// Give the operator the recovery or wait action that matches the audit.
fn status_action(run: Option<&AuditRun>, need: &str) -> String {
    if let Some(run) = run {
        if run.exhausted {
            return format!("vivi pump replan {need}");
        }
        if run.job_id.is_none()
            && let Some(at) = run.retry_at
        {
            return format!("wait-until {at}");
        }
    }
    String::from("vivi pump once")
}

/// Activity counts for units owned by one gatefix need.
#[derive(Default)]
struct UnitActivity {
    active: usize,
    queued: usize,
    parked: usize,
}

/// Count queued, running and parked fix units for a need.
fn unit_counts(st: &State, need: &str) -> UnitActivity {
    let mut counts = UnitActivity::default();
    for unit in st.units.values().filter(|unit| unit.need == need) {
        match unit.status {
            Status::Pending | Status::Held => counts.queued += 1,
            Status::Inflight | Status::Delivered | Status::Merging | Status::Splitting => {
                counts.active += 1;
            }
            Status::Quarantined | Status::Stuck | Status::NeedsReplan => counts.parked += 1,
            Status::Merged
            | Status::NoDefect
            | Status::Replaced
            | Status::Withdrawn
            | Status::Other => {}
        }
    }
    counts
}

/// Describe the stored run and whether a worker is actively executing it.
fn audit_status(st: &State, run: Option<&AuditRun>) -> (String, bool, String) {
    let Some(run) = run else {
        return (
            String::from("missing"),
            false,
            String::from("no audit run recorded"),
        );
    };
    if run.exhausted {
        return (
            String::from("failed"),
            false,
            run.last_error
                .clone()
                .unwrap_or_else(|| String::from("retry limit reached")),
        );
    }
    if run.task.is_some() {
        return (
            String::from("in-flight"),
            true,
            String::from("legacy audit task is running"),
        );
    }
    if let Some(job) = run.job_id.as_ref().and_then(|id| st.jobs.get(id)) {
        return job_status(job);
    }
    if let Some(at) = run.retry_at {
        return (
            String::from("retry-wait"),
            false,
            format!(
                "retry scheduled at {at}: {}",
                run.last_error.as_deref().unwrap_or("retry")
            ),
        );
    }
    (
        String::from("idle"),
        false,
        run.last_error
            .clone()
            .unwrap_or_else(|| String::from("audit has no active job or retry")),
    )
}

/// Render one durable audit-job lifecycle state.
fn job_status(job: &PoolJob) -> (String, bool, String) {
    match job.status {
        JobStatus::Pending => (
            String::from("queued"),
            false,
            String::from("waiting for a pool slot"),
        ),
        JobStatus::Dispatched => (
            String::from("in-flight"),
            true,
            String::from("auditor job dispatched"),
        ),
        JobStatus::Finished | JobStatus::Timeout | JobStatus::Lost => (
            String::from("result-pending"),
            false,
            String::from("terminal result awaits collection"),
        ),
    }
}

/// Whether an explicit operator replan can recover this exhausted audit.
pub(crate) fn operator_replan_allowed(st: &State, need: &str) -> bool {
    current(st).is_some_and(|run| exhausted_for(&run, need))
        || st.target_states.values().any(|target| {
            audit_from_extra(&target.extra).is_some_and(|run| exhausted_for(&run, need))
        })
}

/// Rearm an exhausted audit after an explicit operator replan request.
pub(crate) fn rearm_operator_replan(ctx: &Ctx, st: &mut State, need: &str) -> bool {
    if current(st).is_some_and(|run| exhausted_for(&run, need)) {
        return rearm_current(ctx, st, need);
    }
    let Some(target) = st.target_states.iter().find_map(|(target, state)| {
        audit_from_extra(&state.extra)
            .is_some_and(|run| exhausted_for(&run, need))
            .then(|| target.clone())
    }) else {
        return false;
    };
    crate::pump::state::with_target_state(st, &target, |target_st| {
        rearm_current(ctx, target_st, need)
    })
}

/// Check whether one persisted audit has stopped and can be restarted.
fn exhausted_for(run: &AuditRun, need: &str) -> bool {
    run.need == need && run.exhausted && run.job_id.is_none() && run.task.is_none()
}

/// Clear the bounded stop and queue its next retry after the explicit request.
fn rearm_current(ctx: &Ctx, st: &mut State, need: &str) -> bool {
    let Some(mut run) = current(st) else {
        return false;
    };
    if !exhausted_for(&run, need) {
        return false;
    }
    let prior = run
        .last_error
        .clone()
        .unwrap_or_else(|| String::from("retry limit reached"));
    run.attempts = run.attempts.max(run.fails);
    run.fails = 0;
    run.exhausted = false;
    run.retry_at = Some(ctx.now());
    run.last_error = Some(format!("operator replan requested after: {prior}"));
    store(st, Some(run));
    ctx.log(&format!(
        "audit retry rearmed by operator replan for {need}"
    ));
    true
}

/// The first ten characters of a tip, for log and subject lines.
fn tip_short(tip: &str) -> String {
    tip.chars().take(10).collect()
}

/// Read the live audit run, if one is stored.
pub(crate) fn current(st: &State) -> Option<AuditRun> {
    let value = st.extra.get(AUDIT_KEY)?.clone();
    match serde_json::from_value(value) {
        Ok(run) => Some(run),
        Err(_) => None,
    }
}

/// Store (or clear) the live audit run in `state.extra`.
pub(crate) fn store(st: &mut State, run: Option<AuditRun>) {
    match run {
        Some(run) => match serde_json::to_value(run) {
            Ok(value) => {
                st.extra.insert(String::from(AUDIT_KEY), value);
            }
            Err(_) => {
                st.extra.remove(AUDIT_KEY);
            }
        },
        None => {
            st.extra.remove(AUDIT_KEY);
        }
    }
}

/// Build the auditor brief for one red integration tip.
pub(crate) fn audit_brief(
    _ctx: &crate::pump::host::Ctx,
    tip: &str,
    output_tail: &str,
    new: &[String],
    residual: &[(String, String)],
    prior: Option<&str>,
) -> String {
    let mut brief = format!(
        "The integration branch went red at {tip}. You are the auditor: read the \
         failing tests and the code they exercise and report what is broken and \
         why, grouped by root cause, with a concrete fix per cause. Fix forward \
         only.\n\n{NO_BLAME}\n\nIt must never weaken, skip or delete tests.\n"
    );
    brief.push_str("\nNew failing tests:\n");
    append_bullets(&mut brief, new.iter().map(String::as_str));

    let residual_names = residual.iter().map(|(name, _)| name.as_str());
    if !residual.is_empty() {
        brief.push_str("\nStill failing after earlier fixes:\n");
        append_bullets(&mut brief, residual_names);
    }

    brief.push_str("\nGate output (tail):\n```\n");
    brief.push_str(output_tail);
    if !output_tail.ends_with('\n') {
        brief.push('\n');
    }
    brief.push_str("```\n");

    if let Some(prior) = prior.filter(|text| !text.trim().is_empty()) {
        brief.push_str("\nPrior audit report (context, do not repeat it):\n");
        brief.push_str(prior);
        if !prior.ends_with('\n') {
            brief.push('\n');
        }
    }
    brief.push('\n');
    brief.push_str(REPLY_FORMAT);
    brief
}

fn append_bullets<'a>(brief: &mut String, names: impl Iterator<Item = &'a str>) {
    for name in names {
        brief.push_str("- ");
        brief.push_str(name);
        brief.push('\n');
    }
}

const REPORT_BEGIN: &str = "AUDIT-BEGIN";
const REPORT_END: &str = "AUDIT-END";

/// One root cause from an auditor report, which becomes one fix unit.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct Cause {
    /// What is broken and why.
    pub(crate) root_cause: String,
    /// The complete fix-unit brief naming failing tests and a concrete fix.
    pub(crate) brief: String,
    /// Failing test names covered by this cause.
    #[serde(default)]
    pub(crate) tests: Vec<String>,
    /// Files the fix unit may write.
    pub(crate) write_scope: Vec<String>,
    /// Commands that must exit successfully.
    #[serde(default)]
    pub(crate) proof: Vec<String>,
}

/// Return the body of the last complete `AUDIT-BEGIN ... AUDIT-END` block.
fn last_report_block(text: &str) -> Option<&str> {
    let mut last = None;
    let mut rest = text;
    while let Some(at) = rest.find(REPORT_BEGIN) {
        let after = &rest[at + REPORT_BEGIN.len()..];
        let ws = after.len() - after.trim_start().len();
        if !after[..ws].contains('\n') {
            rest = after;
            continue;
        }
        let body = &after[ws..];
        let Some(end) = body.find(REPORT_END) else {
            break;
        };
        last = Some(&body[..end]);
        rest = &body[end + REPORT_END.len()..];
    }
    last
}

/// Return the last complete marked report in the canonical compact form.
pub(crate) fn report_block(text: &str) -> Option<String> {
    let body = last_report_block(text)?.trim();
    Some(format!("{REPORT_BEGIN}\n{body}\n{REPORT_END}\n"))
}

/// Parse the last complete audit report block as one cause per fix unit.
///
/// Returns `None` for missing markers, malformed or empty JSON, or a cause
/// without a root cause, brief, or at least one write-scope path.
pub(crate) fn parse_report(text: &str) -> Option<Vec<Cause>> {
    let body = last_report_block(text)?;
    let Ok(causes) = serde_json::from_str::<Vec<Cause>>(body.trim()) else {
        return None;
    };
    if causes.is_empty()
        || causes.iter().any(|cause| {
            cause.root_cause.trim().is_empty()
                || cause.brief.trim().is_empty()
                || cause.write_scope.is_empty()
        })
    {
        return None;
    }
    Some(causes)
}

/// Save an auditor reply and mint one pending fix unit for each root cause.
pub(crate) fn apply_report(
    ctx: &Ctx,
    st: &mut State,
    need: &str,
    raw: &str,
    causes: &[Cause],
) -> usize {
    let dir = ctx.home.join("audit");
    drop(fs::create_dir_all(&dir));
    drop(fs::write(dir.join(format!("{need}.report")), raw));

    let mut uids = Vec::new();
    for (k, cause) in causes.iter().enumerate() {
        let uid = format!("{need}-{}", k + 1);
        let unit = Unit {
            need: need.to_owned(),
            title: cause.root_cause.chars().take(90).collect(),
            brief: cause.brief.clone(),
            write_scope: cause.write_scope.clone(),
            proof: cause.proof.clone(),
            tools: String::new(),
            status: Status::Pending,
            since: ctx.now(),
            ..Unit::default()
        };
        st.units.insert(uid.clone(), unit);
        st.order.push(uid.clone());
        uids.push(uid);
    }
    st.needs.insert(
        need.to_owned(),
        NeedRecord {
            verdict: String::from("units"),
            reason: String::from("audit report"),
            units: uids,
            revision: 1,
            ..NeedRecord::default()
        },
    );
    ctx.log(&format!(
        "audit report for {need}: {} fix units",
        causes.len()
    ));
    causes.len()
}

/// Read prior reports for distinct residual owners, capped at 4000 chars.
pub(crate) fn prior_reports(ctx: &Ctx, residual: &[(String, String)]) -> Option<String> {
    let mut owners = Vec::new();
    let mut combined = String::new();
    for owner in residual.iter().map(|(_, owner)| owner) {
        if owners.iter().any(|seen| *seen == owner) {
            continue;
        }
        owners.push(owner);
        let Ok(text) = fs::read_to_string(ctx.home.join("audit").join(format!("{owner}.report")))
        else {
            continue;
        };
        append_limited(&mut combined, &format!("prior report for {owner}:\n"), 4000);
        append_limited(&mut combined, &text, 4000);
        if !combined.ends_with('\n') {
            append_limited(&mut combined, "\n", 4000);
        }
        if combined.chars().count() >= 4000 {
            break;
        }
    }
    (!combined.is_empty()).then_some(combined)
}

/// Append at most the remaining number of characters up to `limit`.
fn append_limited(output: &mut String, text: &str, limit: usize) {
    let remaining = limit.saturating_sub(output.chars().count());
    output.extend(text.chars().take(remaining));
}
