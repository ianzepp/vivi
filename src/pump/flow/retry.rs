//! Pure transient classification of a finished step job's failure: whether
//! the pump should redispatch the step, and under which failure class name.
//!
//! Evidence is machine evidence only — the pool's terminal status, the
//! outcome's `state` and `detail`, `envfail`'s host rules, and the receipt's
//! stderr and runner-log tails (`attempt_record::sections_tail`). The hand's
//! own message (`attempt_record::hand_message`) is read for exactly one
//! question — whether a finished model execution said anything substantive —
//! and is never scanned for provider words, so a valid envelope that
//! discusses 429s, checkout failures or connection errors is not classified.
//! Timeout, split and lost failures carry no newly classified evidence here
//! and keep their existing handling.

use crate::pump::attempt_record;
use crate::pump::envfail;
use crate::pump::jobs::{JobOutcome, JobStatus, PoolJob};

/// The durable bounded-retry decision and application a collector drives a
/// classified failure through: persist the intent, enqueue the replacement.
pub(crate) mod policy;

/// Provider-side transient failure: a dropped upstream connection, a rate
/// limit or overload (HTTP 429 included), or a recoverable server/gateway
/// response.
pub const PROVIDER: &str = "provider";
/// Runner-side transient failure: the runner's own fetch or checkout of the
/// source failed.
pub const RUNNER: &str = "runner";
/// A model execution that finished cleanly but left no substantive final
/// reply.
pub const EMPTY_REPLY: &str = "empty-reply";

/// Push outcomes of the runner itself that name a failed fetch or checkout.
const RUNNER_OUTCOMES: [&str; 2] = ["fetch_failed", "checkout_failed"];

/// Lowercase markers of a dropped or refused upstream connection.
const TRANSPORT_MARKERS: [&str; 5] = [
    "connection error",
    "connection refused",
    "connection timed out",
    "connection reset by peer",
    "connection closed",
];

/// Lowercase markers of a recoverable server or gateway response.
const GATEWAY_MARKERS: [&str; 4] = [
    "bad gateway",
    "service unavailable",
    "service temporarily",
    "gateway timeout",
];

/// Lowercase markers of an overload or rate limit.
const RATE_LIMIT_MARKERS: [&str; 5] = [
    "rate limit",
    "rate-limit",
    "ratelimit",
    "too many requests",
    "overloaded",
];

/// HTTP status code of a rate limit or overload.
const RATE_LIMIT_CODES: [&str; 1] = ["429"];

/// Lowercase markers of an authentication, credential or configuration
/// rejection; these are never retried and win over an empty reply.
const AUTH_MARKERS: [&str; 6] = [
    "invalid api key",
    "invalid_api_key",
    "incorrect api key",
    "unauthorized",
    "authentication failed",
    "invalid credentials",
];

/// HTTP status codes of an authentication or authorization rejection.
const AUTH_CODES: [&str; 2] = ["401", "403"];

/// HTTP status codes of a recoverable server or gateway response.
const GATEWAY_CODES: [&str; 3] = ["502", "503", "504"];

/// Classify a job's terminal failure as transient. `Some(class)` means the
/// step may ask for a bounded retry under that class; `None` means existing
/// handling applies — environment failures, envelope corrections, timeouts
/// and ordinary code failures. Rules in order:
///
/// 1. Only finished jobs with a `finished` pool outcome are read at all.
/// 2. Rejections — authentication, credentials, missing tools — return
///    `None` even when the hand also left no reply.
/// 3. The runner's `fetch_failed` / `checkout_failed` detail is [`RUNNER`].
/// 4. Provider evidence in the outcome detail or the receipt's stderr and
///    runner-log tails is [`PROVIDER`]; model-free command jobs never match,
///    so ordinary failing test output is not mistaken for provider failure.
/// 5. A model execution that exited 0 with a blank or literal-`null` reply
///    is [`EMPTY_REPLY`].
#[must_use]
pub fn classify(job: &PoolJob) -> Option<&'static str> {
    let outcome = finished_outcome(job)?;
    let scope = evidence_scope(outcome);
    if rejected(&scope, &outcome.receipt) {
        return None;
    }
    if runner_outcome(&outcome.detail) {
        return Some(RUNNER);
    }
    if !is_model_job(job) {
        return None;
    }
    if scope.lines().any(provider_line) {
        return Some(PROVIDER);
    }
    if empty_reply(&outcome.receipt) {
        return Some(EMPTY_REPLY);
    }
    None
}

/// The outcome of a job that reached a finished pool state; `None` for
/// pending and dispatched jobs, timeouts, losses and outcomes that say so.
fn finished_outcome(job: &PoolJob) -> Option<&JobOutcome> {
    if job.status != JobStatus::Finished {
        return None;
    }
    let outcome = job.outcome.as_ref()?;
    (outcome.state == "finished").then_some(outcome)
}

/// Whether the job ran a model rather than a shell command.
fn is_model_job(job: &PoolJob) -> bool {
    job.command.is_none()
}

/// The lines evidence rules may read: the outcome detail plus the receipt's
/// stderr and runner-log tails, never the hand's own message.
fn evidence_scope(outcome: &JobOutcome) -> String {
    let tail = attempt_record::sections_tail(&outcome.receipt);
    if tail.is_empty() {
        outcome.detail.clone()
    } else {
        format!("{}\n{}", outcome.detail, tail)
    }
}

/// Whether the evidence names a rejection that existing handling owns:
/// an authentication or credential marker, or a missing tool.
fn rejected(scope: &str, receipt: &str) -> bool {
    let missing_tool =
        attempt_record::agent_exit(receipt).is_some_and(|code| envfail::missing_tool(code, scope));
    missing_tool || scope.lines().any(auth_line)
}

/// Whether a line rejects the job's credentials or configuration.
fn auth_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    AUTH_CODES.iter().any(|code| status_token(&lower, code))
        || AUTH_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Whether a line names a provider transport, rate-limit or gateway failure.
fn provider_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    TRANSPORT_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
        || RATE_LIMIT_MARKERS
            .iter()
            .any(|marker| lower.contains(marker))
        || RATE_LIMIT_CODES
            .iter()
            .chain(GATEWAY_CODES.iter())
            .any(|code| status_token(&lower, code))
        || GATEWAY_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Whether a detail line names the runner's fetch or checkout outcome: the
/// outcome is the detail's first word, as the pool writes it.
fn runner_outcome(detail: &str) -> bool {
    detail
        .split_whitespace()
        .next()
        .is_some_and(|outcome| RUNNER_OUTCOMES.contains(&outcome))
}

/// Whether a finished model execution left no substantive final reply: the
/// agent exited 0 and its message is blank or the literal `null`.
fn empty_reply(receipt: &str) -> bool {
    let message = attempt_record::hand_message(receipt);
    let blank = message.trim().is_empty() || message.trim().eq_ignore_ascii_case("null");
    attempt_record::agent_exit(receipt) == Some(0) && blank
}

/// Whether a lowercased line carries an HTTP status `code` as a whole token:
/// neither neighbour is a digit, so `1429` or `4290` never match.
fn status_token(line: &str, code: &str) -> bool {
    let mut rest = line;
    while let Some(at) = rest.find(code) {
        let before = rest[..at].chars().next_back();
        let after = rest[at + code.len()..].chars().next();
        if before.is_none_or(|c| !c.is_ascii_digit()) && after.is_none_or(|c| !c.is_ascii_digit()) {
            return true;
        }
        rest = &rest[at + code.len()..];
    }
    false
}

#[cfg(test)]
#[path = "retry_test.rs"]
mod tests;
