//! What a job's pool records say: the dispatch, collect and landing notes
//! on its task, and the collect receipt in its thread.

use serde_json::Value;

use super::job::TaskEvent;
use super::letter::Letter;

/// The note of each pool event of one job, as stored (JSON text).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobRecords {
    pub dispatched: Option<String>,
    pub collected: Option<String>,
    pub landed: Option<String>,
}

fn value_of(raw: &str) -> Option<Value> {
    serde_json::from_str(raw).map_or_else(|_| None, Some)
}

/// The newest note of each pool event type that names `job`.
#[must_use]
pub fn records_of(events: &[TaskEvent], job: &str) -> JobRecords {
    let mut records = JobRecords::default();
    for event in events {
        let Some(note) = event.note.as_deref() else {
            continue;
        };
        let slot = match event.event_type.as_str() {
            "pool_dispatched" => &mut records.dispatched,
            "pool_collected" => &mut records.collected,
            "pool_landed" => &mut records.landed,
            _ => continue,
        };
        if text_field(note, "job").as_deref() == Some(job) {
            *slot = Some(note.to_string());
        }
    }
    records
}

/// A string or number field of a JSON note, as text; empty is `None`.
#[must_use]
pub fn text_field(raw: &str, key: &str) -> Option<String> {
    let value = value_of(raw)?;
    match value.get(key)? {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// The job named by a pool event note, if it is JSON that names one.
#[must_use]
pub fn job_of(raw: &str) -> Option<String> {
    text_field(raw, "job")
}

/// JSON text laid out for reading; text that is not JSON comes back as it
/// was.
#[must_use]
pub fn pretty(raw: &str) -> String {
    value_of(raw)
        .and_then(|value| serde_json::to_string_pretty(&value).map_or_else(|_| None, Some))
        .unwrap_or_else(|| raw.to_string())
}

/// A `pool_landed` note: how the job's branch landed, and why it did not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landing {
    /// `landed`, `red`, `conflict` or `raced`.
    pub word: String,
    pub base: Option<String>,
    pub tip: Option<String>,
    pub secs: Option<i64>,
    /// The command that failed, for a red landing.
    pub command: Option<String>,
    /// The tail of its output.
    pub tail: Option<String>,
    /// The files in conflict.
    pub files: Vec<String>,
}

/// Read a landing note; `None` when it is not JSON or names no outcome.
#[must_use]
pub fn landing_of(raw: &str) -> Option<Landing> {
    let value = value_of(raw)?;
    let word = text_field(raw, "landing")?;
    let files = value
        .get("files")
        .and_then(Value::as_array)
        .map(|files| {
            files
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(Landing {
        word,
        base: text_field(raw, "base"),
        tip: text_field(raw, "tip"),
        secs: text_field(raw, "secs").and_then(|secs| secs.parse().map_or_else(|_| None, Some)),
        command: text_field(raw, "command"),
        tail: text_field(raw, "tail"),
        files,
    })
}

/// The section titles of a collect receipt, as the pool writes them.
const FACTS: &str = "facts";
const AGENT: &str = "receipt";
const STDERR: &str = "stderr tail";
const RUNNER_LOG: &str = "runner log tail";

/// The collect receipt split into its parts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Receipt {
    /// The summary lines before the first section.
    pub head: String,
    pub facts: Option<String>,
    /// The agent's final message.
    pub agent: Option<String>,
    pub stderr: Option<String>,
    pub runner_log: Option<String>,
}

/// The receipt of `job`: the reply whose text opens `pool result <job>:`.
#[must_use]
pub fn receipt_of(replies: &[Letter], job: &str) -> Option<Receipt> {
    let opening = format!("pool result {job}:");
    let reply = replies
        .iter()
        .rev()
        .find(|reply| reply.body.trim_start().starts_with(&opening))?;
    Some(split_receipt(&reply.body))
}

fn section_title(line: &str) -> Option<&'static str> {
    let title = line.strip_prefix("--- ")?;
    [FACTS, AGENT, STDERR, RUNNER_LOG]
        .into_iter()
        .find(|known| *known == title.trim())
}

fn split_receipt(text: &str) -> Receipt {
    let mut receipt = Receipt::default();
    let mut current: Option<&'static str> = None;
    let mut buffer = String::new();
    let mut previous_blank = false;
    for line in text.lines() {
        let title = if previous_blank {
            section_title(line)
        } else {
            None
        };
        if let Some(title) = title {
            file(&mut receipt, current, &buffer);
            current = Some(title);
            buffer.clear();
        } else {
            buffer.push_str(line);
            buffer.push('\n');
        }
        previous_blank = line.trim().is_empty();
    }
    file(&mut receipt, current, &buffer);
    receipt
}

fn file(receipt: &mut Receipt, section: Option<&str>, text: &str) {
    let text = text.trim_matches('\n').to_string();
    if text.is_empty() {
        return;
    }
    match section {
        None => receipt.head = text,
        Some(FACTS) => receipt.facts = Some(text),
        Some(AGENT) => receipt.agent = Some(text),
        Some(STDERR) => receipt.stderr = Some(text),
        Some(RUNNER_LOG) => receipt.runner_log = Some(text),
        Some(_) => {}
    }
}

#[cfg(test)]
#[path = "job_notes_test.rs"]
mod tests;
