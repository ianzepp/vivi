//! Values that cross workflow and activity boundaries. Everything here is
//! serde, because Temporal stores each of them in workflow history.

use serde::{Deserialize, Serialize};

/// Timeouts and budgets a workflow applies to its steps. Tests pass short
/// values; a deployment passes the measured ones (a 55-minute agent cap,
/// for example). Milliseconds, so the struct stays plain serde.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tuning {
    /// Work rounds per slice: implement → check → review → land. A red
    /// check, a review change request or a Lander ejection spends one.
    pub max_work_rounds: u32,
    /// Activity attempts per step for infra failures (lost host, timeout).
    pub max_infra_attempts: u32,
    /// Provider waits (429, quota) per step before the need parks.
    pub max_provider_waits: u32,
    /// Wait used when a provider failure names no Retry-After.
    pub provider_default_wait_ms: u64,
    /// The agent cap for one implement attempt (start-to-close).
    pub implement_cap_ms: u64,
    /// A host that misses heartbeats this long is treated as lost.
    pub heartbeat_ms: u64,
    /// Start-to-close for the short machine and LLM steps.
    pub step_cap_ms: u64,
    /// First infra retry delay.
    pub infra_retry_ms: u64,
}

/// Which task queue each step runs on. A queue template may hold `{host}`,
/// which is replaced by the host chosen for that step; a template without it
/// names one shared queue (the tests run everything on one queue that way).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Routing {
    /// Workflows, plan and record. Child workflows inherit it.
    pub control: String,
    /// Compose and publish, next to the lane's git cache.
    pub lane: String,
    pub agent: String,
    pub check: String,
    pub review: String,
    pub gate: String,
    /// Hosts that serve the agent, check and review templates.
    pub agent_hosts: Vec<String>,
    /// Hosts that serve the gate template.
    pub gate_hosts: Vec<String>,
    /// Schedule-to-start on the preferred host (the warm tree). When it
    /// passes, the step moves to the next host.
    pub affinity_wait_ms: u64,
    /// Schedule-to-start on a fallback host.
    pub fallback_wait_ms: u64,
}

impl Routing {
    /// Every step on one queue, with one stand-in host.
    pub fn single(queue: &str) -> Routing {
        Routing {
            control: queue.to_string(),
            lane: queue.to_string(),
            agent: queue.to_string(),
            check: queue.to_string(),
            review: queue.to_string(),
            gate: queue.to_string(),
            agent_hosts: vec!["local".to_string()],
            gate_hosts: vec!["local".to_string()],
            affinity_wait_ms: 60_000,
            fallback_wait_ms: 600_000,
        }
    }
}

/// Input to `NeedWorkflow`. The workflow id is `need-<handle>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NeedInput {
    pub handle: String,
    pub lander_id: String,
    pub tuning: Tuning,
    pub routing: Routing,
    /// The repository target (a key of the worker config's `[targets]`).
    pub target: String,
    /// What the need asks for; the plan step slices it.
    pub brief: String,
    /// Slices already planned (by a person or an earlier plan). The plan
    /// step returns these when set.
    pub slices: Vec<SliceSpec>,
    /// Slice branches are `<prefix><need>/<slice>`; `vivi/` by default.
    pub branch_prefix: String,
}

/// One slice of a need, as the plan step wrote it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SliceSpec {
    pub id: String,
    pub write_scope: Vec<String>,
    /// What the agent is asked to do for this slice.
    #[serde(default)]
    pub brief: String,
}

/// Input to `SliceWorkflow`. The workflow id is `slice-<need>-<slice>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SliceInput {
    pub need: String,
    pub slice: SliceSpec,
    pub lander_id: String,
    pub tuning: Tuning,
    pub routing: Routing,
    pub target: String,
    /// Slice branches are `<prefix><need>/<slice>`; `vivi/` by default.
    pub branch_prefix: String,
}

/// Why a need or slice stopped without landing. This is what the Vivi
/// record receives; nothing restarts at plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ParkCause {
    /// Red checks, review change requests or ejections used every round.
    WorkExhausted { rounds: u32, last_failure: String },
    /// A step failed on infra (lost host, timeout) after every attempt.
    InfraExhausted { step: String, detail: String },
    /// The provider stayed throttled for every allowed wait.
    ProviderExhausted { step: String, waits: u32 },
}

/// What a slice workflow returns to its need.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SliceOutcome {
    Landed {
        slice: String,
        head: String,
        rounds: u32,
    },
    Parked {
        slice: String,
        cause: ParkCause,
    },
}

/// What a need workflow returns.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum NeedOutcome {
    Closed { landed: Vec<String> },
    Parked { slice: String, cause: ParkCause },
}

/// The pushed work in progress for a slice: its branch and head commit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Checkpoint {
    pub branch: String,
    pub head: String,
    pub steps_done: u32,
    /// The host whose warm tree holds this work; the next step prefers it.
    #[serde(default)]
    pub host: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanInput {
    pub need: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub brief: String,
    #[serde(default)]
    pub slices: Vec<SliceSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImplementInput {
    pub need: String,
    pub slice: String,
    pub branch: String,
    pub round: u32,
    /// Failure text from the last red check, change request or ejection.
    pub feedback: Option<String>,
    /// Where the previous round left the branch.
    pub resume: Option<Checkpoint>,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub brief: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StepInput {
    pub need: String,
    pub slice: String,
    pub round: u32,
    pub head: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub brief: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CheckResult {
    Green,
    Red { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReviewResult {
    Approve,
    Changes { text: String },
}

/// A slice asking the Lander to merge its head.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LandRequest {
    /// Workflow id the Lander signals the result back to.
    pub reply_to: String,
    pub need: String,
    pub slice: String,
    pub head: String,
    #[serde(default)]
    pub branch: String,
}

/// A merge candidate: `base` plus the included units, as one commit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub target: String,
    pub base: String,
    pub sha: String,
    pub units: Vec<LandRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComposeInput {
    pub target: String,
    pub units: Vec<LandRequest>,
}

/// A unit compose left out of the candidate, and why (a conflict, say).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Excluded {
    pub unit: LandRequest,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComposeResult {
    /// `None` when every unit was excluded or already on main.
    pub candidate: Option<Candidate>,
    pub excluded: Vec<Excluded>,
    /// Units main already contains: landed, with nothing to do.
    #[serde(default)]
    pub already: Vec<LandRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PublishResult {
    Published {
        sha: String,
    },
    /// Main moved away from the candidate's base; nothing was written.
    Stale {
        text: String,
    },
}

/// A slice asking the Lander for leave to start (backpressure and the
/// hot-file mutex).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdmitRequest {
    /// Workflow id the Lander signals `admitted` back to; also the lease key.
    pub reply_to: String,
    pub write_scope: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LandResult {
    Merged,
    Ejected { cause: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum GateResult {
    Pass,
    Fail { text: String },
}

/// Lander settings. The workflow id is chosen by whoever starts it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LanderInput {
    /// How long the first queued unit waits for others to join its batch.
    pub batch_window_ms: u64,
    pub max_batch: usize,
    /// Start-to-close for compose and publish.
    pub step_cap_ms: u64,
    /// Start-to-close for one gate run.
    pub gate_cap_ms: u64,
    pub target: String,
    pub routing: Routing,
    pub admission: AdmissionPolicy,
}

/// When a slice may start. See `admission.rs`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdmissionPolicy {
    /// Paths (prefixes) where parallel work conflicts. Two slices whose
    /// write scopes overlap the same hot path never run at once.
    pub hot_paths: Vec<String>,
    /// No slice starts while the Lander's queue holds more than this much
    /// landing work. 0 turns backpressure off.
    pub max_backlog_ms: u64,
    /// Landing time one queued unit costs, for the backlog estimate.
    pub land_ms_per_unit: u64,
}

/// One call into the Vivi record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RecordCall {
    Status {
        need: String,
        status: String,
    },
    Park {
        need: String,
        slice: String,
        cause: ParkCause,
    },
    Close {
        need: String,
        summary: String,
    },
}
