//! Bounded prior-attempt context for repair-flow correction briefing.
//!
//! When a repair-flow step ends without a `built` verdict, the terminal step
//! payload can carry this module's captured value under `prior_context`, so
//! a later repairer job knows what the original attempt did and why it
//! failed. The collector resolves correction chains to their base job and
//! passes that job in; this module never substitutes another job and never
//! changes a flow decision.
//!
//! Fields of the captured value:
//!
//! - `source_job`: the original repairer job's id, as given by the caller;
//!   never part of any cut.
//! - `task`: the original job's mailspace task handle, or `null` when the
//!   dispatch never reached a task; never part of any cut.
//! - `summary`: the original repairer's own final message, labelled
//!   `authored_by: "model"`. Envelope front matter goes when it parses; a
//!   malformed envelope keeps its available prose, which is precisely the
//!   correction case. The prose is bounded independently at
//!   [`SUMMARY_BYTES`] with a UTF-8-safe cut, an explicit
//!   `[cut at N bytes]` marker and a `truncated` flag; `bytes` counts the
//!   retained text. `null` — explicitly unavailable — when there is no
//!   original job, no outcome, a receipt with no hand-message section (the
//!   pool summary is machine facts and is never manufactured into a
//!   summary), or an empty message.
//! - `failure`: a [`rung_result::FailureExcerpt`] of the original's
//!   collected receipt and stderr sections, with the excerpt's own bounds
//!   and metadata (`mode`, `text`, `source_lines`, `kept_lines`,
//!   `omitted_lines`, `truncated`) retained. `null` when there is no
//!   original job or no outcome.

use serde_json::{Value, json};

use crate::pump::attempt_record::{agent_silent, hand_message};
use crate::pump::jobs::PoolJob;
use crate::pump::jobs::rung_result;
use vivi_mail::letter::prose_of;

/// The summary prose is bounded independently of the failure excerpt's own
/// bounds: this many bytes of the original repairer's final message survive.
const SUMMARY_BYTES: usize = 4 * 1024;

/// Capture the bounded prior-attempt context of `original`, the job the
/// collector resolved from `source_job_id` (a correction chain's base id).
/// Missing evidence is explicitly unavailable (`null`); nothing here is
/// manufactured from pool-summary, facts or log text.
#[must_use]
pub(crate) fn capture(source_job_id: &str, original: Option<&PoolJob>) -> Value {
    let (task, summary, failure) = match original {
        Some(job) => (
            job.task.as_deref().map_or(Value::Null, |task| json!(task)),
            summary_of(job),
            failure_of(job),
        ),
        None => (Value::Null, Value::Null, Value::Null),
    };
    json!({
        "source_job": source_job_id,
        "task": task,
        "summary": summary,
        "failure": failure,
    })
}

/// Render captured context as short prose for a briefing: the original job
/// and task, the model-authored summary, the failure evidence, each part
/// named unavailable when absent. Absent historical context — no value at
/// all, `null`, or a value of another shape — renders as `none`.
#[must_use]
pub(crate) fn render(context: Option<&Value>) -> String {
    let Some(context) = context.filter(|context| context.is_object()) else {
        return String::from("none");
    };
    let mut out = header(context.get("source_job"), context.get("task"));
    out.push_str(&part("summary", context.get("summary")));
    out.push_str(&part("failure", context.get("failure")));
    out
}

/// The original's own final message as a summary value: `null` unless the
/// receipt carries the hand's message section, bounded at
/// [`SUMMARY_BYTES`].
fn summary_of(job: &PoolJob) -> Value {
    let Some(receipt) = job.outcome.as_ref().map(|outcome| outcome.receipt.as_str()) else {
        return Value::Null;
    };
    // `hand_message` falls back to the pool summary when the receipt has no
    // hand-message section; that head is machine facts, never a summary.
    if agent_silent(receipt) {
        return Value::Null;
    }
    // Front matter goes when it parses; a malformed envelope keeps its
    // available prose.
    let message = hand_message(receipt);
    let prose = prose_of(&message);
    if prose.is_empty() {
        return Value::Null;
    }
    let (text, truncated) = bounded(prose);
    json!({
        "authored_by": "model",
        "text": text,
        "bytes": text.len(),
        "truncated": truncated,
    })
}

/// The original's failure evidence: an excerpt of its collected receipt and
/// stderr sections, metadata retained; `null` when there is no outcome.
fn failure_of(job: &PoolJob) -> Value {
    match job.outcome.as_ref() {
        Some(outcome) => json!(rung_result::failure_excerpt(&outcome.receipt)),
        None => Value::Null,
    }
}

/// `text` whole, or its head cut at the largest UTF-8 boundary at or below
/// [`SUMMARY_BYTES`] with an explicit marker, and whether a cut happened.
fn bounded(text: &str) -> (String, bool) {
    if text.len() <= SUMMARY_BYTES {
        return (text.to_owned(), false);
    }
    let mut end = SUMMARY_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (
        format!("{}\n[cut at {SUMMARY_BYTES} bytes]", &text[..end]),
        true,
    )
}

/// The first rendered line: which original job and task, when known.
fn header(job: Option<&Value>, task: Option<&Value>) -> String {
    let Some(job) = job.and_then(Value::as_str) else {
        return String::new();
    };
    match task.and_then(Value::as_str) {
        Some(task) => format!("original job {job} on task {task}\n"),
        None => format!("original job {job}\n"),
    }
}

/// One labelled context part: `none (unavailable)` when the key is missing
/// or `null`, otherwise the label, any provenance the value carries, and
/// the value's indented text.
fn part(label: &str, value: Option<&Value>) -> String {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return format!("{label}: none (unavailable)\n");
    };
    let provenance = value
        .get("authored_by")
        .and_then(Value::as_str)
        .map_or_else(String::new, |by| format!(" ({by}-authored)"));
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    format!("{label}{provenance}:\n{}", indented(text))
}

/// `text` indented two spaces per line, `(none)` when it has no text.
fn indented(text: &str) -> String {
    if text.trim().is_empty() {
        return String::from("  (none)\n");
    }
    text.lines().map(|line| format!("  {line}\n")).collect()
}

#[cfg(test)]
#[path = "context_test.rs"]
mod tests;
