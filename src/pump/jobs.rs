//! Durable jobs dispatched to pool roles.
//!
//! A pending job is an outbox entry: its identity and brief are saved before
//! a pool slot is selected. A dispatched job is owned by the pool, so the
//! pump may stop and later collect the task receipt without restarting work.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::VivariumError;
use crate::pump::dispatch;
use crate::pump::host::{Ctx, shell_quote};

mod recovery;
pub use recovery::recover_dispatched;

/// A request to make one durable role-addressed pool task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSpec {
    /// Stable caller-owned identity; repeated enqueue with this ID is a no-op.
    pub id: String,
    /// Durable kind label (`merge`, `gate`, `planner`, `cadence`, ...).
    pub kind: String,
    /// Local role name to receive the task.
    pub role: String,
    /// Task subject before the job marker is added.
    pub subject: String,
    /// Full worker brief.
    pub body: String,
    /// Ordered starting pools; each configured fallback chain is appended.
    pub pool_candidates: Vec<String>,
    /// Restrict dispatch to the listed/assigned local pools; never follow a
    /// fallback to remote execution.
    pub local_only: bool,
    /// Pools refused for this retry, even if assigned or in a fallback chain.
    pub pool_exclude: Vec<String>,
    /// Optional repository target passed to pool dispatch.
    pub target: Option<String>,
    /// Optional source ref or commit passed with `--from`.
    pub from_ref: Option<String>,
    /// Additional branches/refs the pool must fetch for this task.
    pub with: Vec<String>,
    /// Optional model override for a built-in role fallback.
    pub model: Option<String>,
    /// A shipped role charter to resolve when the task has no such identity.
    pub builtin_role: Option<String>,
    /// Optional destination branch for write jobs.
    pub branch: Option<String>,
    /// Grant this job the configured fast-forward-only default branch target.
    pub allow_default_branch: bool,
    /// Remote refs the privileged runner may publish after clean worker exit.
    pub publish_ref: Option<String>,
    /// Exact source commit required for guarded promotion.
    pub publish_expect: Option<String>,
    /// Pool execution budget in seconds.
    pub budget: u64,
    /// Explicit read-only request; the role can also require this mode.
    pub read_only: bool,
    /// Unit IDs held by a batch job, used to recover merge state.
    pub units: Vec<String>,
    /// Role jobs whose output branches this merge job integrates.
    pub source_jobs: Vec<String>,
    /// This write job must be merged and pass the gate before it is settled.
    pub integration_required: bool,
    /// Job-specific durable inputs used by its collector.
    pub payload: serde_json::Value,
}

impl JobSpec {
    /// Start a job with the common optional dispatch fields left empty.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        kind: impl Into<String>,
        role: impl Into<String>,
        subject: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            role: role.into(),
            subject: subject.into(),
            body: body.into(),
            pool_candidates: Vec::new(),
            local_only: false,
            pool_exclude: Vec::new(),
            target: None,
            from_ref: None,
            with: Vec::new(),
            model: None,
            builtin_role: None,
            branch: None,
            allow_default_branch: false,
            publish_ref: None,
            publish_expect: None,
            budget: 0,
            read_only: false,
            units: Vec::new(),
            source_jobs: Vec::new(),
            integration_required: false,
            payload: serde_json::Value::Object(serde_json::Map::new()),
        }
    }
}

/// Lifecycle of one durable pump job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobStatus {
    /// Durable intent is saved and waits for a free pool slot.
    Pending,
    /// A pool accepted the task; the pool owns its execution.
    Dispatched,
    /// Pool reported a completed result.
    Finished,
    /// Pool stopped a job after its time budget.
    Timeout,
    /// Pool reported a lost slot or result.
    Lost,
}

impl JobStatus {
    /// Whether the pool has reported a terminal state.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Finished | Self::Timeout | Self::Lost)
    }
}

/// A durable pool job, including its original input and runtime receipt key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolJob {
    pub id: String,
    /// Creation time for pending/running status and retry diagnostics.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub queued_at: u64,
    pub kind: String,
    pub role: String,
    pub subject: String,
    pub body: String,
    pub pool_candidates: Vec<String>,
    #[serde(default)]
    pub local_only: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pool_exclude: Vec<String>,
    pub target: Option<String>,
    pub from_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub with: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin_role: Option<String>,
    pub branch: Option<String>,
    #[serde(default)]
    pub allow_default_branch: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_expect: Option<String>,
    pub budget: u64,
    pub read_only: bool,
    pub units: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_jobs: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub integration_required: bool,
    pub payload: serde_json::Value,
    /// The mailspace task handle; absent until task send succeeds.
    pub task: Option<String>,
    /// Pool that accepted the dispatch.
    pub pool: Option<String>,
    /// Dispatch time in epoch seconds.
    pub dispatched_at: Option<u64>,
    pub status: JobStatus,
    pub outcome: Option<JobOutcome>,
    /// When the pump applied the result to its own lifecycle state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_at: Option<u64>,
    /// Integration branch tip after this job's output was merged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_tip: Option<String>,
    /// Tip that passed the gate covering the merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gated_tip: Option<String>,
    /// A decided merge failure that needs a new role attempt or operator action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_error: Option<String>,
}

impl From<JobSpec> for PoolJob {
    fn from(spec: JobSpec) -> Self {
        Self {
            id: spec.id,
            queued_at: unix_now(),
            kind: spec.kind,
            role: spec.role,
            subject: spec.subject,
            body: spec.body,
            pool_candidates: spec.pool_candidates,
            local_only: spec.local_only,
            pool_exclude: spec.pool_exclude,
            target: spec.target,
            from_ref: spec.from_ref,
            with: spec.with,
            model: spec.model,
            builtin_role: spec.builtin_role,
            branch: spec.branch,
            allow_default_branch: spec.allow_default_branch,
            publish_ref: spec.publish_ref,
            publish_expect: spec.publish_expect,
            budget: spec.budget,
            read_only: spec.read_only,
            units: spec.units,
            source_jobs: spec.source_jobs,
            integration_required: spec.integration_required,
            payload: spec.payload,
            task: None,
            pool: None,
            dispatched_at: None,
            status: JobStatus::Pending,
            outcome: None,
            applied_at: None,
            merged_tip: None,
            gated_tip: None,
            integration_error: None,
        }
    }
}

/// The terminal pool result and the full task-thread receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobOutcome {
    /// `finished`, `timeout` or `lost` from `pool reconcile`.
    pub state: String,
    /// Short detail from the reconcile line.
    pub detail: String,
    /// Full latest task-thread response, including the worker receipt.
    pub receipt: String,
    /// Collection time in epoch seconds.
    pub at: u64,
}

/// Verified privileged-runner publication receipt carried in the pool summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerPublishReceipt {
    pub state: String,
    pub reference: String,
    pub candidate: String,
    pub remote_sha: String,
}

impl RunnerPublishReceipt {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        matches!(self.state.as_str(), "pushed" | "already-on-remote")
    }
}

impl JobOutcome {
    /// Whether the pool receipt explicitly reports a successful worker exit.
    #[must_use]
    pub fn agent_success(&self) -> Option<bool> {
        crate::pump::attempt_record::agent_exit(&self.receipt).map(|code| code == 0)
    }

    /// Host/harness classification of a finished or lost pool receipt.
    #[must_use]
    pub fn host_failure(&self) -> Option<crate::pump::envfail::HostFailure> {
        crate::pump::envfail::classify(&self.state, &self.detail, &self.receipt)
    }

    /// Parse `runner publish: STATE REF CANDIDATE_SHA REMOTE_SHA` from the
    /// task's collected pool summary.
    #[must_use]
    pub fn runner_publish(&self) -> Option<RunnerPublishReceipt> {
        let line = self
            .receipt
            .lines()
            .find_map(|line| line.trim().strip_prefix("runner publish: "))?;
        let mut words = line.split_whitespace();
        Some(RunnerPublishReceipt {
            state: words.next()?.to_owned(),
            reference: words.next()?.to_owned(),
            candidate: words.next()?.to_owned(),
            remote_sha: words.next()?.to_owned(),
        })
    }
}

/// Read/write view of a durable job map, implemented by pump state.
pub trait JobStore {
    fn jobs(&self) -> &BTreeMap<String, PoolJob>;
    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob>;
}

/// Add a durable pending job. A stable ID makes repeated scheduler ticks safe.
///
/// # Errors
/// Returns an error if the same ID is reused for different job inputs.
pub fn enqueue(st: &mut impl JobStore, spec: JobSpec) -> Result<(), VivariumError> {
    validate_id(&spec.id)?;
    if let Some(current) = st.jobs().get(&spec.id) {
        if same_spec(current, &spec) {
            return Ok(());
        }
        return Err(VivariumError::Config(format!(
            "pump job ID {:?} already names different work",
            spec.id
        )));
    }
    st.jobs_mut().insert(spec.id.clone(), PoolJob::from(spec));
    Ok(())
}

/// Queue a fast-forward-only promotion on a merge/release pool role.
pub fn enqueue_promotion(
    ctx: &Ctx,
    st: &mut impl JobStore,
    target: &str,
    tip: &str,
    failing: &[String],
) -> Result<Option<String>, VivariumError> {
    let role = ctx
        .routing
        .lookup("release")
        .or_else(|| ctx.routing.merge());
    let Some(role) = role else {
        ctx.log("promotion queued nowhere: no release or merge role is configured");
        return Ok(None);
    };
    let Some(target_ctx) = ctx.for_target(target) else {
        return Err(VivariumError::Config(format!(
            "cannot promote unknown repository target {target:?}"
        )));
    };
    let short: String = tip.chars().take(12).collect();
    let id = format!("promote-{}-{short}", safe_component(target));
    let subject = format!("fast-forward {} to {}", target_ctx.cfg.main, &short);
    let remote = target_ctx.cfg.remote.clone();
    let runner_branch = format!("pump/promotion/{}/{}", safe_component(target), short);
    let body = format!(
        "The green integration tip is `{tip}`. Confirm your checkout HEAD is exactly that commit and the worktree is clean. Do not push or change branches. If HEAD differs, report `blocked`. Otherwise report `ready`; after your clean exit, the privileged pool runner may publish only this exact SHA to `{remote}` `refs/heads/{}`. The controller requires the runner's actual publish receipt and independently verifies the remote ref. End your receipt with one minified JSON line prefixed `PUMP_PROMOTE_RESULT=` and containing `{{\"status\":\"ready|blocked\",\"tip\":\"{tip}\"}}`. The pool runner branch is only scratch output; do not push the promotion result there.\n\nGreen gate failing names at this tip: {}",
        target_ctx.cfg.main,
        if failing.is_empty() {
            "(not recorded)".to_owned()
        } else {
            failing.join(", ")
        },
    );
    let mut spec = JobSpec::new(id.clone(), "promote", &role.name, subject, body);
    spec.target = Some(target.to_owned());
    spec.from_ref = Some(tip.to_owned());
    spec.branch = Some(runner_branch);
    spec.publish_ref = Some(format!("refs/heads/{}", target_ctx.cfg.main));
    spec.publish_expect = Some(tip.to_owned());
    spec.budget = target_ctx.cfg.dispatch.budget;
    spec.payload = serde_json::json!({
        "target": target,
        "branch": target_ctx.cfg.main,
        "remote": target_ctx.cfg.remote,
        "tip": tip,
        "failing": failing,
    });
    enqueue(st, spec)?;
    Ok(Some(id))
}

fn safe_component(value: &str) -> String {
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

/// Dispatch every pending job that has a free pool slot.
///
/// Returns IDs accepted by a pool. Jobs without capacity remain pending.
///
/// # Errors
/// Returns filesystem and task-store errors that prevent writing a brief.
pub fn dispatch_pending(ctx: &Ctx, st: &mut impl JobStore) -> Result<Vec<String>, VivariumError> {
    if !st
        .jobs()
        .values()
        .any(|job| job.status == JobStatus::Pending)
    {
        return Ok(Vec::new());
    }
    let mut free = dispatch::slots(ctx);
    let priorities = crate::pump::priority::levels(ctx);
    let mut pending: Vec<PoolJob> = st
        .jobs()
        .iter()
        .filter(|(_, job)| job.status == JobStatus::Pending)
        .map(|(_, job)| job.clone())
        .collect();
    pending.sort_by_key(|job| dispatch_order(job, &ctx.cfg.fix_prefix, &priorities));
    let mut started = Vec::new();
    for mut snapshot in pending {
        let id = snapshot.id.clone();
        let candidates = candidate_pools(
            ctx,
            &snapshot.role,
            &snapshot.pool_candidates,
            snapshot.local_only,
            &snapshot.pool_exclude,
        );
        for pool in candidates {
            if free.get(&pool).copied().unwrap_or(0) == 0 {
                continue;
            }
            if dispatch_one(ctx, st, &snapshot, &pool, &mut free)? {
                started.push(id.clone());
                break;
            }
            if let Some(updated) = st.jobs().get(&id).cloned() {
                snapshot = updated;
            }
        }
    }
    Ok(started)
}

fn dispatch_order(
    job: &PoolJob,
    fix_prefix: &str,
    priorities: &BTreeMap<String, i64>,
) -> (u8, Reverse<i64>, u64, String) {
    let recovery = matches!(job.kind.as_str(), "audit" | "gate" | "promote");
    let fix = job.kind.contains("fix")
        || (!fix_prefix.is_empty()
            && (job.id.starts_with(fix_prefix)
                || job.units.iter().any(|unit| unit.starts_with(fix_prefix))
                || payload_has_fix(&job.payload, fix_prefix)));
    let class = if recovery {
        0
    } else if fix {
        1
    } else {
        2
    };
    let priority = payload_priority(&job.payload, priorities);
    (class, Reverse(priority), job.queued_at, job.id.clone())
}

fn payload_has_fix(value: &serde_json::Value, fix_prefix: &str) -> bool {
    if fix_prefix.is_empty() {
        return false;
    }
    match value {
        serde_json::Value::String(value) => value.starts_with(fix_prefix),
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| payload_has_fix(value, fix_prefix)),
        serde_json::Value::Object(values) => values.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "id" | "need" | "need_id" | "owner_need" | "unit_id"
            ) && payload_has_fix(value, fix_prefix)
                || matches!(key.as_str(), "unit_sources" | "units")
                    && payload_has_fix(value, fix_prefix)
        }),
        _ => false,
    }
}

fn payload_priority(value: &serde_json::Value, priorities: &BTreeMap<String, i64>) -> i64 {
    match value {
        serde_json::Value::Object(values) => values
            .iter()
            .map(|(key, value)| {
                if matches!(key.as_str(), "need" | "need_id" | "owner_need")
                    && let Some(need) = value.as_str()
                {
                    return priorities.get(need).copied().unwrap_or(0);
                }
                payload_priority(value, priorities)
            })
            .max()
            .unwrap_or(0),
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| payload_priority(value, priorities))
            .max()
            .unwrap_or(0),
        _ => 0,
    }
}

/// Whether a reconcile pass has dispatched jobs that need pool polling.
#[must_use]
pub fn has_dispatched(st: &impl JobStore) -> bool {
    st.jobs()
        .values()
        .any(|job| job.status == JobStatus::Dispatched)
}

/// Whether a role/target has pending, running, or unapplied work of `kind`.
#[must_use]
pub fn has_unsettled_kind_for_target(st: &impl JobStore, kind: &str, target: &str) -> bool {
    st.jobs().values().any(|job| {
        job.kind == kind
            && job.target.as_deref() == Some(target)
            && (!job.status.is_terminal() || job.applied_at.is_none())
    })
}

/// Preserve a `merging` unit only while a dispatched merge job owns it.
#[must_use]
pub fn owns_merging_unit(st: &impl JobStore, uid: &str) -> bool {
    st.jobs().values().any(|job| {
        job.kind == "merge"
            && job.units.iter().any(|unit| unit == uid)
            && matches!(
                job.status,
                JobStatus::Pending | JobStatus::Dispatched | JobStatus::Finished
            )
    })
}

/// Read the saved outcome after the collector has transitioned a job.
#[must_use]
pub fn completion<'a>(st: &'a impl JobStore, id: &str) -> Option<&'a JobOutcome> {
    st.jobs().get(id)?.outcome.as_ref()
}

/// Whether a role job's output is clear of the common merge and gate pipeline.
#[must_use]
pub fn branch_landed(st: &impl JobStore, id: &str) -> bool {
    st.jobs()
        .get(id)
        .is_some_and(|job| !job.integration_required || job.gated_tip.is_some())
}

/// Decided branch-integration failure, if one ended the merge pipeline.
#[must_use]
pub fn integration_error<'a>(st: &'a impl JobStore, id: &str) -> Option<&'a str> {
    st.jobs().get(id)?.integration_error.as_deref()
}

/// A role branch ready to enter the common merge pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchSource {
    pub job_id: String,
    pub role: String,
    pub branch: String,
    pub subject: String,
}

/// Integration branches from completed write jobs that have not merged yet.
#[must_use]
pub fn branch_sources_pending(st: &impl JobStore, target: &str) -> Vec<BranchSource> {
    st.jobs()
        .values()
        .filter(|job| {
            job.integration_required
                && job.target.as_deref() == Some(target)
                && job.branch.is_some()
                && job.status == JobStatus::Finished
                && job.outcome.as_ref().and_then(JobOutcome::agent_success) == Some(true)
                && job.merged_tip.is_none()
                && job.integration_error.is_none()
        })
        .map(|job| BranchSource {
            job_id: job.id.clone(),
            role: job.role.clone(),
            branch: job.branch.clone().unwrap_or_default(),
            subject: job.subject.clone(),
        })
        .collect()
}

/// Role job IDs whose merged tips still await their gate receipt.
#[must_use]
pub fn sources_waiting_for_gate(st: &impl JobStore, target: &str) -> Vec<String> {
    st.jobs()
        .values()
        .filter(|job| {
            job.integration_required
                && job.target.as_deref() == Some(target)
                && job.merged_tip.is_some()
                && job.gated_tip.is_none()
                && job.integration_error.is_none()
        })
        .map(|job| job.id.clone())
        .collect()
}

/// Record the integration tip for role branches accepted by a merge job.
pub fn mark_merged(st: &mut impl JobStore, ids: &[String], tip: &str) {
    for id in ids {
        if let Some(job) = st.jobs_mut().get_mut(id)
            && job.integration_required
            && job.merged_tip.is_none()
        {
            job.merged_tip = Some(tip.to_owned());
        }
    }
}

/// Record the clean gate covering a role job's merged tip.
pub fn mark_gated(st: &mut impl JobStore, ids: &[String], tip: &str) {
    for id in ids {
        if let Some(job) = st.jobs_mut().get_mut(id)
            && job.integration_required
            && job.merged_tip.as_deref() == Some(tip)
        {
            job.gated_tip = Some(tip.to_owned());
        }
    }
}

/// Record a decided failure in merging a role job's pushed output.
pub fn mark_integration_failed(st: &mut impl JobStore, id: &str, detail: &str) {
    if let Some(job) = st.jobs_mut().get_mut(id)
        && job.integration_required
        && job.integration_error.is_none()
    {
        job.integration_error = Some(detail.to_owned());
    }
}

/// Mark a terminal result applied while keeping its receipt available to the
/// owning scheduler.
pub fn mark_applied(st: &mut impl JobStore, id: &str, at: u64) -> bool {
    let Some(job) = st.jobs_mut().get_mut(id) else {
        return false;
    };
    if !job.status.is_terminal()
        || job.applied_at.is_some()
        || (job.integration_required && job.gated_tip.is_none() && job.integration_error.is_none())
    {
        return false;
    }
    job.applied_at = Some(at);
    true
}

/// Apply terminal pool lines to all matching durable jobs.
pub fn apply_reconcile(
    ctx: &Ctx,
    st: &mut impl JobStore,
    seen: &BTreeMap<String, (String, String)>,
) -> Vec<String> {
    let finished: Vec<(String, String, String, JobStatus)> = st
        .jobs()
        .iter()
        .filter_map(|(id, job)| {
            if job.status != JobStatus::Dispatched {
                return None;
            }
            let task = job.task.as_ref()?;
            let (state, detail) = seen.get(task)?;
            let status = match state.as_str() {
                "finished" => JobStatus::Finished,
                "timeout" => JobStatus::Timeout,
                "lost" => JobStatus::Lost,
                _ => return None,
            };
            Some((id.clone(), task.clone(), detail.clone(), status))
        })
        .collect();
    let mut changed = Vec::new();
    for (id, task, detail, status) in finished {
        let outcome = JobOutcome {
            state: status.as_str().to_owned(),
            detail,
            receipt: crate::pump::collect::receipt_full(ctx, &task),
            at: ctx.now(),
        };
        if let Some(job) = st.jobs_mut().get_mut(&id) {
            if job.status == JobStatus::Dispatched {
                job.status = status;
                if job.integration_required && job.integration_error.is_none() {
                    if outcome.agent_success() == Some(true) {
                        // The branch may enter the normal merge pipeline.
                    } else {
                        let reason = outcome.host_failure().map_or_else(
                            || {
                                if outcome.agent_success().is_none() {
                                    String::from("worker receipt omitted its exit code")
                                } else {
                                    String::from("worker exit was unsuccessful")
                                }
                            },
                            |failure| failure.detail,
                        );
                        job.integration_error = Some(format!(
                            "{reason}; write-role branch is not eligible to merge"
                        ));
                    }
                }
                job.outcome = Some(outcome);
                changed.push(id);
            }
        }
    }
    changed
}

impl JobStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Dispatched => "dispatched",
            Self::Finished => "finished",
            Self::Timeout => "timeout",
            Self::Lost => "lost",
        }
    }
}

fn dispatch_one(
    ctx: &Ctx,
    st: &mut impl JobStore,
    snapshot: &PoolJob,
    pool: &str,
    free: &mut BTreeMap<String, u32>,
) -> Result<bool, VivariumError> {
    let task = match &snapshot.task {
        Some(task) => task.clone(),
        None => match send_task(ctx, snapshot)? {
            Some(task) => {
                if let Some(job) = st.jobs_mut().get_mut(&snapshot.id) {
                    job.task = Some(task.clone());
                }
                task
            }
            None => return Ok(false),
        },
    };
    let command = pool_command(snapshot, pool, &task);
    let output = ctx.vivi(&command, 240);
    if output.code != 0 || !output.text.contains("dispatched") {
        if output.text.contains("no free slot") {
            free.insert(pool.to_owned(), 0);
        }
        let latest = crate::pump::collect::receipt_full(ctx, &task);
        recovery::recover_task(ctx, st, &snapshot.id, &task, &latest);
        if st
            .jobs()
            .get(&snapshot.id)
            .is_some_and(|job| job.status != JobStatus::Pending)
        {
            return Ok(true);
        }
        ctx.log(&format!(
            "job dispatch deferred {} on {pool}: {}",
            snapshot.id,
            tail(&output.text, 240)
        ));
        return Ok(false);
    }
    let job = st.jobs_mut().get_mut(&snapshot.id).ok_or_else(|| {
        VivariumError::Message(format!(
            "pump job {:?} disappeared during dispatch",
            snapshot.id
        ))
    })?;
    job.pool = Some(pool.to_owned());
    job.dispatched_at = Some(ctx.now());
    job.status = JobStatus::Dispatched;
    free.entry(pool.to_owned())
        .and_modify(|count| *count = count.saturating_sub(1));
    ctx.log(&format!(
        "job dispatched {} role={} pool={pool} task={task}",
        snapshot.id, snapshot.role
    ));
    Ok(true)
}

fn send_task(ctx: &Ctx, job: &PoolJob) -> Result<Option<String>, VivariumError> {
    let dir = ctx.home.join("job-briefs");
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.txt", job.id));
    let body = format!(
        "pump-job-id: {}\npump-job-kind: {}\n{}{}\n{}",
        job.id,
        job.kind,
        job.builtin_role
            .as_deref()
            .map_or(String::new(), |role| format!("pump-builtin-role: {role}\n")),
        "",
        job.body,
    );
    fs::write(&path, body)?;
    let subject = format!("[pump-job:{}] {}", job.id, job.subject);
    let command = format!(
        "task send --from {} --to {} --subject {} --body-file {}",
        shell_quote(&ctx.cfg.from),
        shell_quote(&job.role),
        shell_quote(&subject),
        shell_quote(&path.to_string_lossy()),
    );
    let output = ctx.vivi(&command, 120);
    if output.code != 0 {
        ctx.log(&format!(
            "job task send failed {}: {}",
            job.id,
            tail(&output.text, 240)
        ));
    }
    Ok(dispatch::parse_created_handle(&output.text))
}

fn pool_command(job: &PoolJob, pool: &str, task: &str) -> String {
    let mut args = vec![
        "pool dispatch".to_string(),
        format!("--pool {}", shell_quote(pool)),
        shell_quote(task),
    ];
    if let Some(target) = &job.target {
        args.push(format!("--target {}", shell_quote(target)));
    }
    if let Some(from) = &job.from_ref {
        args.push(format!("--from {}", shell_quote(from)));
    }
    for extra in &job.with {
        args.push(format!("--with {}", shell_quote(extra)));
    }
    if let Some(model) = &job.model {
        args.push(format!("--model {}", shell_quote(model)));
    }
    if let Some(branch) = &job.branch {
        args.push(format!("--branch {}", shell_quote(branch)));
    }
    if job.allow_default_branch {
        args.push("--allow-default-branch".into());
    }
    if let Some(reference) = &job.publish_ref {
        args.push(format!("--publish-ref {}", shell_quote(reference)));
    }
    if let Some(expected) = &job.publish_expect {
        args.push(format!("--publish-expect {}", shell_quote(expected)));
    }
    if job.budget > 0 {
        args.push(format!("--budget {}", job.budget));
    }
    if job.read_only {
        args.push("--read-only".into());
    }
    args.push("--porcelain".into());
    args.join(" ")
}

/// Expand role/pool choices into their ordered fallback chain.
#[must_use]
pub fn candidate_pools(
    ctx: &Ctx,
    role_name: &str,
    candidates: &[String],
    local_only: bool,
    excluded: &[String],
) -> Vec<String> {
    let assigned = ctx
        .routing
        .lookup(role_name)
        .and_then(|role| role.pool.clone());
    let starts = if local_only {
        if candidates.is_empty() {
            assigned.into_iter().collect()
        } else {
            candidates.to_vec()
        }
    } else {
        assigned
            .into_iter()
            .chain(candidates.iter().cloned())
            .collect()
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut pools = Vec::new();
    for candidate in starts {
        let mut chain = if local_only {
            vec![candidate.clone()]
        } else {
            ctx.routing.fallback_chain(&candidate)
        };
        if chain.is_empty() {
            chain.push(candidate);
        }
        for pool in chain {
            let local = ctx
                .routing
                .pools
                .get(&pool)
                .is_some_and(|entry| entry.provider == "local");
            if (!local_only || local)
                && !excluded.iter().any(|excluded| excluded == &pool)
                && seen.insert(pool.clone())
            {
                pools.push(pool);
            }
        }
    }
    pools
}

fn same_spec(job: &PoolJob, spec: &JobSpec) -> bool {
    job.id == spec.id
        && job.kind == spec.kind
        && job.role == spec.role
        && job.subject == spec.subject
        && job.body == spec.body
        && job.pool_candidates == spec.pool_candidates
        && job.local_only == spec.local_only
        && job.pool_exclude == spec.pool_exclude
        && job.target == spec.target
        && job.from_ref == spec.from_ref
        && job.with == spec.with
        && job.model == spec.model
        && job.builtin_role == spec.builtin_role
        && job.branch == spec.branch
        && job.allow_default_branch == spec.allow_default_branch
        && job.publish_ref == spec.publish_ref
        && job.publish_expect == spec.publish_expect
        && job.budget == spec.budget
        && job.read_only == spec.read_only
        && job.units == spec.units
        && job.source_jobs == spec.source_jobs
        && job.integration_required == spec.integration_required
        && job.payload == spec.payload
}

fn validate_id(id: &str) -> Result<(), VivariumError> {
    let valid = !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte));
    if valid {
        Ok(())
    } else {
        Err(VivariumError::Config(format!(
            "invalid pump job ID {id:?}; use letters, digits, dash, underscore or dot"
        )))
    }
}

fn tail(text: &str, chars: usize) -> String {
    let len = text.chars().count();
    text.chars().skip(len.saturating_sub(chars)).collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[cfg(test)]
#[path = "jobs_test.rs"]
mod tests;
