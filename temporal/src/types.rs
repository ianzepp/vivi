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

/// Input to `NeedWorkflow`. The workflow id is `need-<handle>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NeedInput {
    pub handle: String,
    pub lander_id: String,
    pub tuning: Tuning,
}

/// One slice of a need, as the plan step wrote it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SliceSpec {
    pub id: String,
    pub write_scope: Vec<String>,
}

/// Input to `SliceWorkflow`. The workflow id is `slice-<need>-<slice>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SliceInput {
    pub need: String,
    pub slice: SliceSpec,
    pub lander_id: String,
    pub tuning: Tuning,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanInput {
    pub need: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StepInput {
    pub need: String,
    pub slice: String,
    pub round: u32,
    pub head: String,
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
    pub step_cap_ms: u64,
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
