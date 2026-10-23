//! Durable jobs dispatched to pool roles.
//!
//! A pending job is an outbox entry: its identity and brief are saved before
//! a pool slot is selected. A dispatched job is owned by the pool, so the
//! pump may stop and later collect the task receipt without restarting work.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::VivariumError;
use crate::pump::host::Ctx;

mod brief;
mod dispatcher;
pub mod merge_main;
mod receipts;
mod recovery;
pub mod rung;
pub mod rung_result;
mod slots;
pub use dispatcher::{candidate_pools, dispatch_pending};
#[cfg(test)]
use dispatcher::{dispatch_order, pool_command, send_task};
pub use receipts::{JobOutcome, RunnerDeleteReceipt, RunnerPublishReceipt, RunnerSourceReceipt};
pub use recovery::recover_dispatched;
pub(crate) use slots::{parse_created_handle, slots};

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
    /// Other repository targets checked out read-only beside the job's own
    /// (`--also-target`). Only a read-only job may name any.
    pub extra_targets: Vec<String>,
    /// Optional source ref or commit passed with `--from`.
    pub from_ref: Option<String>,
    /// Additional branches/refs the pool must fetch for this task.
    pub with: Vec<String>,
    /// Optional model override for a built-in role fallback.
    pub model: Option<String>,
    /// Shell command run as the harness instead of a model (`--command`).
    pub command: Option<String>,
    /// A shipped role charter to resolve when the task has no such identity.
    pub builtin_role: Option<String>,
    /// Optional destination branch for write jobs.
    pub branch: Option<String>,
    /// Grant this job the configured fast-forward-only default branch target.
    pub allow_default_branch: bool,
    /// Remote refs the privileged runner may publish after clean worker exit.
    pub publish_ref: Option<String>,
    /// Exact source commit the runner publishes (for example a need-branch
    /// rename publishes the old tip under the prior name).
    pub publish_expect: Option<String>,
    pub publish_contains: Option<String>,
    pub delete_ref: Option<String>,
    pub delete_expect: Option<String>,
    /// Pool execution budget in seconds.
    pub budget: u64,
    /// Explicit read-only request; the role can also require this mode.
    pub read_only: bool,
    /// Unit IDs the job carried in the retired batch pipeline; copied into
    /// the job record, never read by the flow engine.
    pub units: Vec<String>,
    /// Source job ids from the retired merge pipeline; only copied into the
    /// job record.
    pub source_jobs: Vec<String>,
    /// Job-specific durable inputs used by its collector.
    pub payload: serde_json::Value,
    /// An existing mailspace task this job runs on; dispatch then skips
    /// `task send` and the pool receives this task as it is.
    pub task: Option<String>,
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
            extra_targets: Vec::new(),
            from_ref: None,
            with: Vec::new(),
            model: None,
            command: None,
            builtin_role: None,
            branch: None,
            allow_default_branch: false,
            publish_ref: None,
            publish_expect: None,
            publish_contains: None,
            delete_ref: None,
            delete_expect: None,
            budget: 0,
            read_only: false,
            units: Vec::new(),
            source_jobs: Vec::new(),
            payload: serde_json::Value::Object(serde_json::Map::new()),
            task: None,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_targets: Vec<String>,
    pub from_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub with: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Shell command run as the harness instead of a model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin_role: Option<String>,
    pub branch: Option<String>,
    #[serde(default)]
    pub allow_default_branch: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_expect: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_contains: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_expect: Option<String>,
    pub budget: u64,
    pub read_only: bool,
    pub units: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_jobs: Vec<String>,
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
            extra_targets: spec.extra_targets,
            from_ref: spec.from_ref,
            with: spec.with,
            model: spec.model,
            command: spec.command,
            builtin_role: spec.builtin_role,
            branch: spec.branch,
            allow_default_branch: spec.allow_default_branch,
            publish_ref: spec.publish_ref,
            publish_expect: spec.publish_expect,
            publish_contains: spec.publish_contains,
            delete_ref: spec.delete_ref,
            delete_expect: spec.delete_expect,
            budget: spec.budget,
            read_only: spec.read_only,
            units: spec.units,
            source_jobs: spec.source_jobs,
            payload: spec.payload,
            task: spec.task,
            pool: None,
            dispatched_at: None,
            status: JobStatus::Pending,
            outcome: None,
            applied_at: None,
        }
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

/// Read the saved outcome after the collector has transitioned a job.
#[must_use]
pub fn completion<'a>(st: &'a impl JobStore, id: &str) -> Option<&'a JobOutcome> {
    st.jobs().get(id)?.outcome.as_ref()
}

/// Mark a terminal result applied while keeping its receipt available to the
/// owning scheduler.
pub fn mark_applied(st: &mut impl JobStore, id: &str, at: u64) -> bool {
    let Some(job) = st.jobs_mut().get_mut(id) else {
        return false;
    };
    if !job.status.is_terminal() || job.applied_at.is_some() {
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
        && job.extra_targets == spec.extra_targets
        && job.from_ref == spec.from_ref
        && job.with == spec.with
        && job.model == spec.model
        && job.command == spec.command
        && job.builtin_role == spec.builtin_role
        && job.branch == spec.branch
        && job.allow_default_branch == spec.allow_default_branch
        && job.publish_ref == spec.publish_ref
        && job.publish_expect == spec.publish_expect
        && job.publish_contains == spec.publish_contains
        && job.delete_ref == spec.delete_ref
        && job.delete_expect == spec.delete_expect
        && job.budget == spec.budget
        && job.read_only == spec.read_only
        && job.units == spec.units
        && job.source_jobs == spec.source_jobs
        && job.payload == spec.payload
        // A spec with no task matches a job whose task dispatch assigned later.
        && (spec.task.is_none() || job.task == spec.task)
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
