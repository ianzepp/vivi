//! Shared transient-recovery helpers: payload metadata, deterministic
//! replacement identity, and a pure transient classifier.
//!
//! A finished step job whose machine evidence shows an infrastructure
//! failure — a provider 429 or transport drop, a runner fetch or checkout
//! that a plain rerun can fix, or a model that ended without any final
//! message — is worth one deterministic transient retry. This module owns
//! the vocabulary every recovery side reads: the `-tr<N>` job-id suffix,
//! the durable `recovery` payload object, the class words the classifier
//! returns, and the publication-safety gate of [`safety`]. Whether a retry
//! is allowed and which store intent freezes it is the wired collector's
//! job, not this module's.
//!
//! Staged like `server::mcp::send_options`: the vocabulary ships whole, and
//! the collector that consumes it lands in a later slice.
#![cfg_attr(not(test), allow(dead_code))]

use super::budget_retry;
use crate::pump::attempt_record;
use crate::pump::jobs::{JobOutcome, PoolJob};

/// The publication-safety gate a replacement must pass: [`safety::decide`].
pub(crate) mod safety;

// ---- the pure transient classifier ---------------------------------------

/// The class for a runner fetch or checkout failure a rerun can fix.
pub(crate) const RUNNER: &str = "runner";
/// The class for a provider 429, transport drop or temporary upstream.
pub(crate) const PROVIDER: &str = "provider";
/// The class for a finished model job whose final message was empty.
pub(crate) const EMPTY: &str = "empty";

/// The reconcile detail words of a privileged-runner fetch or checkout that
/// failed before the agent could run. Credential and target-configuration
/// outcomes (`envfail::HOST_PUSH_OUTCOMES` names them too) are deliberately
/// absent: a bad key or a mismatched target is not fixed by a rerun.
const RUNNER_OUTCOMES: &[&str] = &["fetch_failed", "checkout_failed"];

/// Line markers of a provider 429, transport drop or temporary upstream
/// failure, matched lowercased. Applied only to machine scopes — never to
/// the hand's own message — and never carrying the authentication markers
/// of `envfail::endpoint`: an invalid key or rejected credential is not
/// uniformly retryable, so no marker here accepts one.
const PROVIDER_MARKERS: &[&str] = &[
    "429",
    "rate limit",
    "too many requests",
    "overloaded",
    "connection error",
    "connection refused",
    "connection timed out",
    "connection reset by peer",
    "connection closed",
    "bad gateway",
    "service unavailable",
    "service temporarily",
    "gateway timeout",
];

/// The leading marker of a pool receipt section (`--- receipt`, ...).
const SECTION_MARKER: &str = "--- ";
/// The legacy receipt boundary after which only the hand's own text follows.
const HAND_MESSAGE_BOUNDARY: &str = "hand's final message:";
/// The literal some adapters print for an empty final result: JSON `null`.
const NULL_RESULT: &str = "null";

/// Classify a finished step job from machine evidence alone; `None` means
/// the failure is not a transient one. A command job has no model message
/// and its empty stdout is success, so commands never classify; a timeout
/// or lost reconcile is the budget-retry and recovery machinery's business;
/// a budget kill — either representation, the terminal timeout status or a
/// finished summary reporting agent exit 124 — is excluded by
/// [`budget_retry::is_budget_kill`].
#[must_use]
pub(crate) fn transient_class(job: &PoolJob) -> Option<&'static str> {
    if job.command.is_some() {
        return None;
    }
    let outcome = job.outcome.as_ref()?;
    if outcome.state != "finished" || budget_retry::is_budget_kill(job) {
        return None;
    }
    runner_outcome(&outcome.detail)
        .or_else(|| provider_trouble(outcome))
        .or_else(|| empty_message(outcome))
}

/// Whether the reconcile detail names a runner fetch or checkout failure.
fn runner_outcome(detail: &str) -> Option<&'static str> {
    let word = detail.split_whitespace().next()?;
    RUNNER_OUTCOMES.contains(&word).then_some(RUNNER)
}

/// Whether the machine scopes of the receipt carry a provider or transport
/// marker. The hand's own message section is never read, so model prose
/// about infrastructure cannot classify.
fn provider_trouble(outcome: &JobOutcome) -> Option<&'static str> {
    machine_scopes(outcome)
        .lines()
        .any(|line| {
            let lower = line.to_lowercase();
            PROVIDER_MARKERS.iter().any(|marker| lower.contains(marker))
        })
        .then_some(PROVIDER)
}

/// Whether the hand's final message is genuinely empty: the agent ran to a
/// clean exit ([`JobOutcome::agent_success`]) and its receipt section is
/// blank, or exactly the JSON `null` literal some adapters print when the
/// model ended with no final result. A nonzero exit with no message is a
/// harness or environment failure — a missing tool, a dead endpoint —
/// never a genuinely empty reply; a nonempty message that fails to parse
/// is a correction case.
fn empty_message(outcome: &JobOutcome) -> Option<&'static str> {
    if outcome.agent_success() != Some(true) {
        return None;
    }
    let message = attempt_record::hand_message(&outcome.receipt);
    let trimmed = message.trim();
    (trimmed.is_empty() || trimmed == NULL_RESULT).then_some(EMPTY)
}

/// The machine scopes of a receipt: the reconcile detail, the pool summary
/// head (bounded like a budget-kill summary), and the stderr and runner-log
/// tail sections.
fn machine_scopes(outcome: &JobOutcome) -> String {
    let mut scopes = outcome.detail.clone();
    for text in [
        summary_head(&outcome.receipt),
        attempt_record::sections_tail(&outcome.receipt),
    ] {
        if !text.is_empty() {
            if !scopes.is_empty() {
                scopes.push('\n');
            }
            scopes.push_str(&text);
        }
    }
    scopes
}

/// The receipt's summary head: the lines before the first section marker
/// and before the legacy hand-message boundary.
fn summary_head(receipt: &str) -> String {
    receipt
        .lines()
        .take_while(|line| {
            !line.starts_with(SECTION_MARKER) && !line.starts_with(HAND_MESSAGE_BOUNDARY)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---- deterministic replacement identity ----------------------------------

/// The transient-retry suffix; distinct from the correction `-c<N>`, which
/// keeps parsing a transient retry as an original job.
const RETRY_SUFFIX: &str = "-tr";
/// The correction suffix; a correction hung off a transient retry names the
/// same step input, so it is seen through when counting retries.
const CORRECTION_SUFFIX: &str = "-c";

/// The id of the job a transient-retry chain started from: `job-tr2` and
/// `job-tr2-c1` both strip to `job`.
#[must_use]
pub(crate) fn base_id(job_id: &str) -> &str {
    split_id(job_id).0
}

/// How many transient retries lie behind a job id: `0` for an original job,
/// `n` for `<id>-tr<n>` (a trailing correction ordinal is seen through).
#[must_use]
pub(crate) fn transient_retry_n(job_id: &str) -> u32 {
    split_id(job_id).1
}

/// The deterministic id of the next transient retry of `failed_job`: the
/// base id plus the advanced ordinal, so re-scheduling a failed retry
/// (`job-tr1`) yields `job-tr2`, never a stacked `job-tr1-tr1`.
#[must_use]
pub(crate) fn next_replacement_id(failed_job: &str) -> String {
    let (base, n) = split_id(failed_job);
    format!("{base}-tr{}", n + 1)
}

/// A job id split into its chain base and its transient-retry ordinal.
fn split_id(job_id: &str) -> (&str, u32) {
    // A correction of a transient retry (`job-tr1-c1`) names the same step
    // input: strip a trailing correction ordinal before reading `-tr`.
    let stripped = match job_id.rsplit_once(CORRECTION_SUFFIX) {
        Some((base, tail)) if tail.parse::<u32>().is_ok() => base,
        _ => job_id,
    };
    match stripped.rsplit_once(RETRY_SUFFIX) {
        Some((base, tail)) => match tail.parse::<u32>() {
            Ok(n) => (base, n),
            Err(_) => (stripped, 0),
        },
        None => (stripped, 0),
    }
}

// ---- recovery payload metadata -------------------------------------------

/// The durable payload object carrying recovery metadata on a transient
/// retry; sibling of `budget_retry::PAYLOAD_KEY`.
pub(crate) const PAYLOAD_KEY: &str = "recovery";

/// The metadata field naming the step input the job ran on.
const INPUT_KEY: &str = "input";
/// The metadata field naming which transient retry the job is.
const RETRY_KEY: &str = "retry";
/// The metadata field naming the transient class that triggered the retry.
const CLASS_KEY: &str = "class";

/// Recovery metadata read from a job payload. It identifies the step input
/// and the retry ordinal independently of the ordinary `attempt` field and
/// of any correction ordinal in the job id, so a failed replacement can be
/// accounted against the same step input it replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveryMeta {
    /// The caller's identity of the step input; one ordinal is spent per
    /// step input, matching the store's `recovery_intents.input`.
    pub input: String,
    /// Which transient retry of that step input this job is: 1 for the
    /// first. Absent metadata is the initial input: it has spent nothing,
    /// and it is never a fresh autonomous allowance.
    pub retry: u32,
    /// The transient class that triggered the retry ([`RUNNER`],
    /// [`PROVIDER`] or [`EMPTY`]).
    pub class: String,
}

/// Read the recovery metadata of a payload; `None` when absent or unreadable.
#[must_use]
pub(crate) fn metadata(payload: &serde_json::Value) -> Option<RecoveryMeta> {
    let object = payload.get(PAYLOAD_KEY)?;
    let raw = object.get(RETRY_KEY)?.as_u64()?;
    let Ok(retry) = u32::try_from(raw) else {
        return None;
    };
    Some(RecoveryMeta {
        input: object.get(INPUT_KEY)?.as_str()?.to_owned(),
        retry,
        class: object.get(CLASS_KEY)?.as_str()?.to_owned(),
    })
}

/// Write `meta` into a payload's [`PAYLOAD_KEY`] object: the one writer of
/// the recovery metadata. A null payload becomes an object; a payload of
/// any other shape is left untouched.
pub(crate) fn stamp_metadata(payload: &mut serde_json::Value, meta: &RecoveryMeta) {
    let value = serde_json::json!({
        INPUT_KEY: meta.input,
        RETRY_KEY: meta.retry,
        CLASS_KEY: meta.class,
    });
    match payload {
        serde_json::Value::Object(map) => {
            map.insert(PAYLOAD_KEY.to_owned(), value);
        }
        serde_json::Value::Null => {
            let mut map = serde_json::Map::new();
            map.insert(PAYLOAD_KEY.to_owned(), value);
            *payload = serde_json::Value::Object(map);
        }
        serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_)
        | serde_json::Value::Array(_) => {}
    }
}

/// How many transient-retry ordinals the step input behind `payload` has
/// already spent: the metadata's ordinal, or `0` when absent. Absent
/// metadata denotes the initial input — it has spent nothing; it never
/// grants a fresh autonomous allowance.
#[must_use]
pub(crate) fn retries_spent(payload: &serde_json::Value) -> u32 {
    metadata(payload).map_or(0, |meta| meta.retry)
}

#[cfg(test)]
#[path = "recovery_test.rs"]
mod tests;
