//! Per-target main rung tick phase: a red run files or replies on the repair need, a green run records the last green tip.

mod green;
mod red;
mod schedule;

use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::jobs::JobStore;

/// main_state key: the last main tip a main rung finished on.
pub(crate) const TESTED: &str = "main-tested";
/// main_state key: the last main tip whose main rung passed.
pub(crate) const GREEN: &str = "main-green";
/// main_state key: unix seconds when the last main rung job was started.
pub(crate) const STARTED: &str = "main-started";
/// main_state key: the failing test id set of the last red main rung.
pub(crate) const FAILING: &str = "main-failing";
/// main_state key: the long-lived task the main rung jobs run on.
pub(crate) const TASK: &str = "main-rung-task";
/// Job payload key holding the target a main rung job tests.
pub(crate) const PAYLOAD_KEY: &str = "main_rung";

/// A per-target main_state key, `<name>:<target>`.
pub(crate) fn key(name: &str, target: &str) -> String {
    format!("{name}:{target}")
}

/// The trimmed value of the first `target:` line of a record body; None when absent or empty.
pub(crate) fn record_target(body: &str) -> Option<&str> {
    body.lines()
        .find_map(|line| line.trim().strip_prefix("target:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Register the main rung tick phase. N6 fills it.
pub(crate) fn register<S: JobStore, M: FlowMail>(_registry: &mut Registry<S, M>) {}
