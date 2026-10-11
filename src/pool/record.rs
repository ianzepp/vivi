//! What a dispatch and its result look like on a task.
//!
//! Both are events on the task (`pool_dispatched`, `pool_collected`) whose note
//! is a JSON record, paired with a human-readable reply in the task's thread.
//! A dispatch is open until a later `pool_collected` event closes it.

use serde::{Deserialize, Serialize};

use super::collect::{Collected, Outcome};
use crate::mailspace::PoolEventRow;

/// The machine-readable record of a dispatch, as written to the task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchRecord {
    pub slot: String,
    pub machine: String,
    pub job: String,
    pub model: String,
    pub from: String,
    pub branch: String,
    /// Commit `from` resolved to when the job was dispatched.
    pub start: String,
    pub budget: u64,
    /// Dispatch time, epoch seconds.
    pub at: u64,
}

/// A dispatch with no result recorded yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenDispatch {
    pub task: String,
    pub record: DispatchRecord,
}

/// The JSON note of a `pool_dispatched` event.
#[must_use]
pub fn dispatch_note(record: &DispatchRecord) -> String {
    serde_json::to_string(record).unwrap_or_default()
}

/// The reply text of a dispatch.
#[must_use]
pub fn dispatch_text(record: &DispatchRecord) -> String {
    format!(
        "pool dispatch {job}\nslot {slot}  model {model}  budget {budget}s\nfrom {from} ({start})  branch {branch}",
        job = record.job,
        slot = record.slot,
        model = record.model,
        budget = record.budget,
        from = record.from,
        start = record.start.get(..12).unwrap_or(&record.start),
        branch = record.branch,
    )
}

/// Open dispatches among `events` (oldest first): a task's latest
/// `pool_dispatched` not followed by a `pool_collected`.
#[must_use]
pub fn fold_open(events: &[PoolEventRow]) -> Vec<OpenDispatch> {
    let mut open: Vec<OpenDispatch> = Vec::new();
    for event in events {
        open.retain(|d| d.task != event.handle);
        if event.event_type != "pool_dispatched" {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<DispatchRecord>(&event.note) {
            open.push(OpenDispatch {
                task: event.handle.clone(),
                record,
            });
        }
    }
    open
}

/// The JSON note of a `pool_collected` event.
#[must_use]
pub fn collected_note(dispatch: &OpenDispatch, outcome: &Outcome) -> String {
    let record = &dispatch.record;
    let mut note = serde_json::json!({
        "job": record.job,
        "slot": record.slot,
        "outcome": outcome.word(),
    });
    if let Outcome::Finished(done) = outcome {
        for (key, value) in &done.result.fields {
            note[key] = serde_json::Value::String(value.clone());
        }
    }
    note.to_string()
}

/// The reply text of a result: the facts, then the agent's receipt.
#[must_use]
pub fn collected_text(dispatch: &OpenDispatch, outcome: &Outcome) -> String {
    let record = &dispatch.record;
    let mut text = format!(
        "pool result {job}: {word}\nslot {slot}  model {model}\nfrom {from}  branch {branch}",
        job = record.job,
        word = outcome.word(),
        slot = record.slot,
        model = record.model,
        from = record.from,
        branch = record.branch,
    );
    match outcome {
        Outcome::Finished(done) => text.push_str(&finished_text(done)),
        Outcome::Timeout => {
            text.push_str("\nno result within the ");
            text.push_str(&record.budget.to_string());
            text.push_str("s budget plus grace; the slot was stopped");
        }
        Outcome::Lost => text.push_str("\nthe slot stopped without writing a result"),
    }
    text
}

fn finished_text(done: &Collected) -> String {
    let mut text = String::from("\n");
    text.push_str(&done.result.summary());
    for (title, body) in [
        ("receipt", &done.receipt),
        ("stderr tail", &done.stderr),
        ("runner log tail", &done.log),
    ] {
        if !body.trim().is_empty() {
            text.push_str("\n\n--- ");
            text.push_str(title);
            text.push('\n');
            text.push_str(body.trim_end());
        }
    }
    text
}

#[cfg(test)]
#[path = "record_test.rs"]
mod tests;
