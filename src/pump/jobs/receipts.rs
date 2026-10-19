//! Privileged runner receipts collected with a durable role job.

use serde::{Deserialize, Serialize};

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
    pub contains: Option<String>,
    pub contains_verified: bool,
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
            .runner_section()
            .lines()
            .find_map(|line| line.trim().strip_prefix("runner publish: "))?;
        let mut words = line.split_whitespace();
        Some(RunnerPublishReceipt {
            state: words.next()?.to_owned(),
            reference: words.next()?.to_owned(),
            candidate: words.next()?.to_owned(),
            remote_sha: words.next()?.to_owned(),
            contains: self.contains_proof().map(|(sha, _)| sha.to_owned()),
            contains_verified: self.contains_proof().is_some_and(|(_, verified)| verified),
        })
    }
}

/// Compare-and-delete receipt from the credentialed runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerDeleteReceipt {
    pub state: String,
    pub reference: String,
    pub expected: String,
    pub before: String,
    pub after: String,
}

impl RunnerDeleteReceipt {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        matches!(self.state.as_str(), "deleted" | "already-absent") && self.after == "absent"
    }
}

impl JobOutcome {
    fn contains_proof(&self) -> Option<(&str, bool)> {
        let line = self
            .runner_section()
            .lines()
            .find_map(|line| line.trim().strip_prefix("runner contains: "))?;
        let mut words = line.split_whitespace();
        Some((words.next()?, words.next()? == "1"))
    }

    /// Parse the runner's exact expected-ref compare-and-delete outcome.
    #[must_use]
    pub fn runner_delete(&self) -> Option<RunnerDeleteReceipt> {
        let line = self
            .runner_section()
            .lines()
            .find_map(|line| line.trim().strip_prefix("runner delete: "))?;
        let mut words = line.split_whitespace();
        Some(RunnerDeleteReceipt {
            state: words.next()?.to_owned(),
            reference: words.next()?.to_owned(),
            expected: words.next()?.to_owned(),
            before: words.next()?.to_owned(),
            after: words.next()?.to_owned(),
        })
    }
}

/// Source-tip inclusion independently measured by the privileged runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerSourceReceipt {
    pub reference: String,
    pub sha: String,
    pub contained: bool,
}

impl JobOutcome {
    fn runner_section(&self) -> &str {
        self.receipt
            .split_once("hand's final message:")
            .map_or(self.receipt.as_str(), |(summary, _)| summary)
    }

    #[must_use]
    pub fn runner_sources(&self) -> Vec<RunnerSourceReceipt> {
        self.runner_section()
            .lines()
            .filter_map(|line| {
                let line = line.trim().strip_prefix("runner source: ")?;
                let mut words = line.split_whitespace();
                Some(RunnerSourceReceipt {
                    reference: words.next()?.to_owned(),
                    sha: words.next()?.to_owned(),
                    contained: words.next()? == "1",
                })
            })
            .collect()
    }
}
