//! What a landing looks like on a task.
//!
//! A landing (the branch of a job merged into main, or why it was not) is a
//! `pool_landed` event on the task, written by `vivi pool landed`. It is kept
//! apart from the dispatch and collect events: [`POOL_EVENT_TYPES`] does not
//! list it, so `fold_open` never sees it and a landing of an old job cannot
//! close or be mistaken for a newer dispatch of the same task.
//!
//! [`POOL_EVENT_TYPES`]: crate::mailspace::POOL_EVENT_TYPES

use serde_json::{Map, Value};
use vivi_error::VivariumError;

use crate::mailspace::Mailspace;

/// The event type a landing is recorded under.
pub const LANDED_EVENT: &str = "pool_landed";

/// Most bytes of `tail` a landing note keeps (the end of the text).
pub const TAIL_CAP: usize = 4096;

/// How a landing ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landing {
    Landed,
    Red,
    Conflict,
    Raced,
}

impl Landing {
    /// Read one of the four outcome words.
    ///
    /// # Errors
    /// Returns an error for any other word.
    pub fn parse(word: &str) -> Result<Self, VivariumError> {
        match word {
            "landed" => Ok(Self::Landed),
            "red" => Ok(Self::Red),
            "conflict" => Ok(Self::Conflict),
            "raced" => Ok(Self::Raced),
            other => Err(VivariumError::Message(format!(
                "unknown landing outcome {other:?}; use landed, red, conflict or raced"
            ))),
        }
    }

    /// The word the note and the index carry.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Landed => "landed",
            Self::Red => "red",
            Self::Conflict => "conflict",
            Self::Raced => "raced",
        }
    }
}

/// One landing, as recorded on the task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandedRecord {
    pub job: String,
    pub landing: Landing,
    /// The main commit the landing started from.
    pub base: Option<String>,
    /// The commit main ended at.
    pub tip: Option<String>,
    /// Seconds the landing took.
    pub secs: Option<u64>,
    /// The failing command (red).
    pub command: Option<String>,
    /// The end of the failing command's output.
    pub tail: Option<String>,
    /// The conflicting files (conflict).
    pub files: Vec<String>,
}

/// The last [`TAIL_CAP`] bytes of `text`, cut on a character boundary.
#[must_use]
pub fn cap_tail(text: &str) -> &str {
    if text.len() <= TAIL_CAP {
        return text;
    }
    let mut start = text.len() - TAIL_CAP;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// The JSON note of a `pool_landed` event. Absent fields are left out.
#[must_use]
pub fn landed_note(record: &LandedRecord) -> String {
    let mut note = Map::new();
    note.insert("job".into(), Value::String(record.job.clone()));
    note.insert("landing".into(), record.landing.word().into());
    for (key, value) in [
        ("base", &record.base),
        ("tip", &record.tip),
        ("command", &record.command),
    ] {
        if let Some(value) = value {
            note.insert(key.into(), Value::String(value.clone()));
        }
    }
    if let Some(secs) = record.secs {
        note.insert("secs".into(), secs.into());
    }
    if let Some(tail) = &record.tail {
        note.insert("tail".into(), Value::String(cap_tail(tail).to_owned()));
    }
    if !record.files.is_empty() {
        note.insert("files".into(), serde_json::json!(record.files));
    }
    Value::Object(note).to_string()
}

/// The reply text of a landing.
#[must_use]
pub fn landed_text(record: &LandedRecord) -> String {
    let mut text = format!("pool landing {}: {}", record.job, record.landing.word());
    let short = |sha: &str| sha.get(..12).unwrap_or(sha).to_owned();
    let mut facts = Vec::new();
    if let Some(base) = &record.base {
        facts.push(format!("base {}", short(base)));
    }
    if let Some(tip) = &record.tip {
        facts.push(format!("tip {}", short(tip)));
    }
    if let Some(secs) = record.secs {
        facts.push(format!("{secs}s"));
    }
    if !facts.is_empty() {
        text.push('\n');
        text.push_str(&facts.join("  "));
    }
    if let Some(command) = &record.command {
        text.push_str(&format!("\nfailed: {command}"));
    }
    if !record.files.is_empty() {
        text.push_str("\nconflicting files:");
        for file in &record.files {
            text.push_str("\n  ");
            text.push_str(file);
        }
    }
    if let Some(tail) = &record.tail {
        text.push_str("\n\n--- tail\n");
        text.push_str(cap_tail(tail).trim_end());
    }
    text
}

/// Record a landing on `task`: a `pool_landed` event and its reply.
///
/// # Errors
/// Returns an error when no `pool_dispatched` event of `task` names the job,
/// or the event cannot be written.
pub fn record_landed(
    space: &Mailspace,
    task: &str,
    record: &LandedRecord,
) -> Result<(), VivariumError> {
    let handle = space.pool_handle(task)?;
    require_dispatched_job(space, &handle, &record.job)?;
    space.record_pool_note(
        task,
        LANDED_EVENT,
        &landed_note(record),
        &landed_text(record),
    )
}

/// Refuse a job the task was never dispatched as.
fn require_dispatched_job(space: &Mailspace, handle: &str, job: &str) -> Result<(), VivariumError> {
    let dispatched = space.pool_events()?.into_iter().any(|event| {
        event.handle == handle
            && event.event_type == "pool_dispatched"
            && match serde_json::from_str::<Value>(&event.note) {
                Ok(note) => note.get("job").and_then(Value::as_str) == Some(job),
                Err(_) => false,
            }
    });
    if dispatched {
        Ok(())
    } else {
        Err(VivariumError::Message(format!(
            "task {handle} has no dispatch of job {job}"
        )))
    }
}

#[cfg(test)]
#[path = "landed_test.rs"]
mod tests;
