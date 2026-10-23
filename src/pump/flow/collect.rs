//! Collecting a finished role-step job into an outcome: the answer word from
//! the hand's final message, one retry, then `failed`.

use crate::pump::attempt_record::hand_message;
use crate::pump::flow::def::{ANSWER_LINE, NodeClass};
use crate::pump::flow::index::StepJobRow;
use crate::pump::jobs::{JobStatus, PoolJob};

/// A step is retried once: the second failed attempt answers `failed`.
const MAX_ATTEMPTS: u32 = 2;

/// What a finished role-step job amounts to.
#[derive(Debug, Clone, PartialEq)]
pub enum Collected {
    /// The step answered `word`; the payload is the hand's final message.
    Outcome(String, serde_json::Value),
    /// No usable answer yet: run the step's job again.
    Retry,
    /// The attempts are spent: the step answers `failed`.
    Failed,
}

/// Read the answer word of `job`, the attempt recorded by `step`, for a step
/// of `class`. `step.attempt` counts this job among the task's attempts
/// (1-based), so the task has had `step.attempt` attempts so far.
#[must_use]
pub fn collect(job: &PoolJob, step: &StepJobRow, class: &NodeClass) -> Collected {
    match answer(job, class) {
        Some((word, message)) => Collected::Outcome(word, serde_json::Value::String(message)),
        None if step.attempt < MAX_ATTEMPTS => Collected::Retry,
        None => Collected::Failed,
    }
}

/// The valid word and the final message, or `None` for a job that timed out,
/// was lost, or ended with no valid `ANSWER:` line.
fn answer(job: &PoolJob, class: &NodeClass) -> Option<(String, String)> {
    if job.status != JobStatus::Finished {
        return None;
    }
    let receipt = job.outcome.as_ref()?.receipt.as_str();
    let message = hand_message(receipt);
    let word = last_answer(&message, class)?;
    Some((word, message))
}

/// The class word named by the last `ANSWER:` line of `message`. The whole
/// line is compared, so `NOT READY` never reads as `READY`.
fn last_answer(message: &str, class: &NodeClass) -> Option<String> {
    let line = message
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| line.starts_with(ANSWER_LINE))?;
    let said = line[ANSWER_LINE.len()..]
        .trim()
        .trim_matches(|c: char| c == '`' || c == '*' || c == '.')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    class
        .words()
        .iter()
        .find(|word| word.eq_ignore_ascii_case(&said))
        .map(|word| (*word).to_owned())
}

#[cfg(test)]
#[path = "collect_test.rs"]
mod tests;
