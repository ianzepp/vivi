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

/// Whether a summary line is a result line the runner itself writes.
fn is_result_marker(line: &str) -> bool {
    let line = line.trim();
    line.starts_with("result: ") || line.starts_with("push: ")
}

/// The exact discarded total of a `result: read_only_discarded N` (current)
/// or `push: read_only_discarded N` (historical) marker. A marker of any other
/// outcome, or a total that is absent, malformed or overflows, is no evidence:
/// it never becomes an invented count.
fn discarded_marker_total(line: &str) -> Option<u64> {
    let line = line.trim();
    let rest = line
        .strip_prefix("result: ")
        .or_else(|| line.strip_prefix("push: "))?;
    let count = rest.trim_end().strip_prefix("read_only_discarded ")?;
    let count = count.trim_end();
    if count.is_empty() || !count.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    match count.parse() {
        Ok(total) => Some(total),
        Err(_) => None,
    }
}

/// The first path of the summary's `discarded paths (N of TOTAL):` sample, the
/// rest of the head's lines read from just after the marker. Spacing and
/// Unicode inside a path are kept; a missing or malformed sample leaves the
/// path absent rather than guessed.
fn sampled_first_path(lines: &mut std::str::Lines<'_>) -> Option<String> {
    for line in lines.by_ref() {
        if !(line.starts_with("discarded paths (") && line.trim_end().ends_with("):")) {
            continue;
        }
        let path = lines.next()?.strip_prefix("  ")?;
        return (!path.is_empty()).then(|| path.to_owned());
    }
    None
}

impl JobOutcome {
    /// Whether the runner reports a successful worker exit without a failed check.
    #[must_use]
    pub fn agent_success(&self) -> Option<bool> {
        if self
            .summary_head()
            .lines()
            .any(|line| line.starts_with("runner check: failed "))
        {
            return Some(false);
        }
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

    /// The runner-recorded discarded evidence of a read-only job: the exact
    /// total it discarded and, when the summary sampled one, the first sampled
    /// path. Only the summary head counts — the agent's own message and the
    /// rendered sections never supply either value.
    #[must_use]
    pub fn read_only_discarded(&self) -> Option<(u64, Option<String>)> {
        let head = self.summary_head();
        let mut lines = head.lines();
        let total = loop {
            let line = lines.next()?;
            if !is_result_marker(line) {
                continue;
            }
            break discarded_marker_total(line)?;
        };
        Some((total, sampled_first_path(&mut lines)))
    }

    /// The pool summary lines only: cut at the first rendered section heading
    /// and at the legacy `hand's final message:` boundary.
    fn summary_head(&self) -> &str {
        let head = crate::pool::record::split_sections(&self.receipt).0;
        head.split_once("hand's final message:")
            .map_or(head, |(summary, _)| summary)
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

#[cfg(test)]
#[path = "receipts_test.rs"]
mod tests;
