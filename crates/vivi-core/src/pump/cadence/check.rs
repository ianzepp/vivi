//! Durable command-check state for command cadence roles.
//!
//! A model cadence run is replaceable: the next enqueue overwrites its
//! [`super::RoleRun`]. A command check is not. It has to remember the last
//! commit the command passed on, the red need a failure opened, and the
//! immutable context the latest execution ran in, so a later tick can tell a
//! green from a red interval and can refuse a second execution over the same
//! work. That state lives here, beside the replaceable run.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The outcome of a command execution that reported a pass.
pub const GREEN: &str = "green";

/// The outcome of a command execution that reported a failure in the code.
pub const RED: &str = "red";

/// The outcome of a command execution the environment stopped.
pub const ENVIRONMENT: &str = "environment";

/// One immutable command execution: the context it was enqueued with and the
/// outcome it reported afterwards.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandExecution {
    /// The durable pool job id.
    #[serde(default)]
    pub job: String,
    /// The check fingerprint the execution was enqueued for.
    #[serde(default)]
    pub fingerprint: String,
    /// The effective repository target; empty on a legacy single-target pump.
    #[serde(default)]
    pub target: String,
    /// The main ref the execution started from.
    #[serde(default)]
    pub main: String,
    /// The exact command the execution ran.
    #[serde(default)]
    pub command: String,
    /// The role name the execution was dispatched for.
    #[serde(default)]
    pub role: String,
    /// The attempt number this execution ran under.
    #[serde(default)]
    pub attempt: u32,
    #[serde(default)]
    pub started_at: u64,
    #[serde(default)]
    pub ended_at: Option<u64>,
    /// `green`, `red`, `environment`, or absent while the execution runs.
    #[serde(default)]
    pub outcome: Option<String>,
    /// Where the execution's report was filed.
    #[serde(default)]
    pub evidence: Option<String>,
    /// The next epoch this execution allows another one.
    #[serde(default)]
    pub due_at: Option<u64>,
    /// The backoff epoch an environment retry waits for.
    #[serde(default)]
    pub retry_at: Option<u64>,
}

/// Durable command-check state for one role.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandCheck {
    /// Fingerprint of the configuration the latest execution ran.
    #[serde(default)]
    pub fingerprint: String,
    /// The commit the command last passed on.
    #[serde(default)]
    pub last_green_commit: Option<String>,
    /// The red need a failure opened, until it is settled.
    #[serde(default)]
    pub red_need: Option<String>,
    /// The latest execution, with the context it was enqueued in.
    #[serde(default)]
    pub latest: Option<CommandExecution>,
}

impl CommandCheck {
    /// Record a new execution, keeping the durable green/red history.
    pub fn record(&mut self, execution: CommandExecution) {
        self.fingerprint.clone_from(&execution.fingerprint);
        self.latest = Some(execution);
    }
}

/// The stable fingerprint of one command check: canonical role name, effective
/// target, target main and the exact command. A process-independent hash, so
/// the same check keeps its identity across restarts and machines.
#[must_use]
pub fn fingerprint(role: &str, target: &str, main: &str, command: &str) -> String {
    let canonical_role = role.trim().to_lowercase();
    let mut hasher = Sha256::new();
    for part in [canonical_role.as_str(), target.trim(), main.trim(), command] {
        hasher.update(part.as_bytes());
        hasher.update([0_u8]);
    }
    hex::encode(hasher.finalize())
}
