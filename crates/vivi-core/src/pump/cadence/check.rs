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
    /// The agent exit code the execution reported; absent when it reported
    /// none at all.
    #[serde(default)]
    pub exit: Option<i32>,
    /// The full commit the runner started the execution on, read from the
    /// matched `pool_collected` record. Never a scan tip, a shortened receipt
    /// hash, or the main ref as it reads after collection.
    #[serde(default)]
    pub tested_commit: Option<String>,
    /// What the result still owes before its job may be applied: a red need,
    /// or a recovery report for a green over an open red need.
    #[serde(default)]
    pub awaiting: Option<String>,
}

/// A cadence-specific exception: the check could not establish a tested
/// result, so it retries with exponential backoff and files no need.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandException {
    /// The exception kind the pump recorded.
    #[serde(default)]
    pub kind: String,
    /// One line saying what could not be read or run.
    #[serde(default)]
    pub reason: String,
    /// When the exception was raised, epoch seconds.
    #[serde(default)]
    pub at: u64,
    /// The attempt this exception belongs to.
    #[serde(default)]
    pub attempt: u32,
    /// The epoch before which no execution may be launched.
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
    /// The main tip the changed-tip gate is waiting to move past.
    #[serde(default)]
    pub waiting_main: Option<String>,
    /// The cadence-specific exception the check is backing off from.
    #[serde(default)]
    pub exception: Option<CommandException>,
}

impl CommandCheck {
    /// Record a new execution, keeping the durable green/red history.
    ///
    /// A changed command, target or target main is a different check: it
    /// inherits neither another check's green skip nor its red need.
    pub fn record(&mut self, execution: CommandExecution) {
        if self.fingerprint != execution.fingerprint {
            self.last_green_commit = None;
            self.red_need = None;
        }
        self.fingerprint.clone_from(&execution.fingerprint);
        self.waiting_main = None;
        self.exception = None;
        self.latest = Some(execution);
    }

    /// The exception this check is still backing off from at `now`.
    #[must_use]
    pub fn pending_exception(&self, now: u64) -> Option<&CommandException> {
        self.exception
            .as_ref()
            .filter(|exception| exception.retry_at.is_none_or(|at| at > now))
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
