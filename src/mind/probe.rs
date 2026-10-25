//! Read-only pump signals for one registered project.
//!
//! The real probe reuses `vivi pump health`, which reads `pump.db`
//! read-only and never takes a lock the pump waits on, so probing a wedged
//! pump cannot wedge the mind. The finer signals (locked needs, held steps
//! awaiting an answer, a red main, repair alarms) have no read-only accessor
//! on the pump snapshot yet; the spike leaves them at zero in
//! [`HealthProbe`] and the fake probe in tests sets them directly.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pump::health::{self, HealthOptions, Verdict};

/// What the mind knows about one project's pump at one instant.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSignals {
    pub root: PathBuf,
    /// `ok`, `degraded`, `stalled`, or `unknown`.
    pub verdict: String,
    /// Health reasons, verbatim from the pump. Untrusted text.
    pub reasons: Vec<String>,
    /// Needs whose flow run is locked (stub: always zero from the real probe).
    pub locked_needs: usize,
    /// Flow steps held awaiting an answer (stub: always zero from the real probe).
    pub held_steps: usize,
    /// Main failed its proof rung (stub: always false from the real probe).
    pub red_main: bool,
    /// A repair flow raised an alarm (stub: always false from the real probe).
    pub repair_alarm: bool,
}

impl ProjectSignals {
    /// Whether anything here should wake the mind outside its heartbeat.
    #[must_use]
    pub fn alarming(&self) -> bool {
        matches!(self.verdict.as_str(), "degraded" | "stalled")
            || self.locked_needs > 0
            || self.held_steps > 0
            || self.red_main
            || self.repair_alarm
    }

    /// A stable one-line summary, used to tell a new alarm from one the mind
    /// already woke for.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        format!(
            "{} verdict={} locked={} held={} red_main={} repair={}",
            self.root.display(),
            self.verdict,
            self.locked_needs,
            self.held_steps,
            self.red_main,
            self.repair_alarm
        )
    }
}

/// A source of project signals.
pub trait Probe {
    /// Probe one project. A probe never fails: a project that cannot be
    /// read is an `unknown` verdict with the error as its reason.
    fn probe(&self, root: &Path) -> ProjectSignals;
}

/// The real probe, over `vivi pump health`.
#[derive(Debug, Clone, Copy, Default)]
pub struct HealthProbe;

impl Probe for HealthProbe {
    fn probe(&self, root: &Path) -> ProjectSignals {
        match health::health(root, &HealthOptions::default()) {
            Ok(report) => ProjectSignals {
                root: root.to_path_buf(),
                verdict: report.verdict.as_str().to_string(),
                reasons: report.reasons,
                ..ProjectSignals::default()
            },
            Err(error) => ProjectSignals {
                root: root.to_path_buf(),
                verdict: Verdict::Unknown.as_str().to_string(),
                reasons: vec![error.to_string()],
                ..ProjectSignals::default()
            },
        }
    }
}

#[cfg(test)]
#[path = "probe_test.rs"]
mod tests;
