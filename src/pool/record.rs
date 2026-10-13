//! What a dispatch and its result look like on a task.
//!
//! Both are events on the task (`pool_dispatched`, `pool_collected`) whose note
//! is a JSON record, paired with a human-readable reply in the task's thread.
//! A dispatch is open until a later `pool_collected` event closes it.

use serde::{Deserialize, Serialize};

use super::collect::{Collected, DISCARDED_PATH_PREFIX, RESTORED_PATH_PREFIX, Outcome};
use super::facts;
use crate::mailspace::PoolEventRow;

/// The machine-readable record of a dispatch, as written to the task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchRecord {
    /// The pool the slot belongs to. A record made before pools were named
    /// has none (empty) and reads as a pool that no longer exists.
    #[serde(default)]
    pub pool: String,
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
    /// The job publishes nothing (`branch` is then empty). A record made
    /// before read-only jobs existed reads as `false`.
    #[serde(default)]
    pub read_only: bool,
    /// What the host gave the slot, when it says (a rented pod's GPU and
    /// price). A record made before hosts said so has none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hardware: String,
    /// Pods the host made and terminated before the one that works, because
    /// they did not become reachable in time. A record made before hosts said
    /// so reads as none.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub abandoned: u32,
    /// Their approximate total charge in millionths of a dollar (hourly price
    /// times the time each was waited for); zero when none was abandoned or
    /// no price was known.
    #[serde(default, skip_serializing_if = "is_zero_cost")]
    pub abandoned_cost_micros: u64,
}

/// `skip_serializing_if` takes a reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_cost(n: &u64) -> bool {
    *n == 0
}

/// Millionths of a dollar as `0.12`, rounded to the cent.
#[must_use]
pub fn dollars(micros: u64) -> String {
    let cents = micros.saturating_add(5_000) / 10_000;
    format!("{}.{:02}", cents / 100, cents % 100)
}

/// What a dispatch tells about the pods it gave up on: nothing when none, else
/// `abandoned 2 pod(s), approx $0.12`.
#[must_use]
pub fn abandoned_text(record: &DispatchRecord) -> String {
    if record.abandoned == 0 {
        return String::new();
    }
    let cost = if record.abandoned_cost_micros == 0 {
        "price unknown".to_string()
    } else {
        format!("approx ${}", dollars(record.abandoned_cost_micros))
    };
    format!(
        "abandoned {} pod(s) that did not become reachable, {cost}",
        record.abandoned
    )
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
        "pool dispatch {job}\npool {pool}  slot {slot}  model {model}  budget {budget}s{hardware}\nfrom {from} ({start})  {target}{abandoned}",
        job = record.job,
        pool = record.pool,
        slot = record.slot,
        model = record.model,
        budget = record.budget,
        from = record.from,
        start = record.start.get(..12).unwrap_or(&record.start),
        target = target_text(record),
        hardware = hardware_text(record),
        abandoned = abandoned_line(record),
    )
}

/// `\n<abandoned text>` when pods were abandoned, else nothing.
fn abandoned_line(record: &DispatchRecord) -> String {
    let text = abandoned_text(record);
    if text.is_empty() {
        text
    } else {
        format!("\n{text}")
    }
}

/// `  hardware <what>` when the host named any, else nothing.
fn hardware_text(record: &DispatchRecord) -> String {
    if record.hardware.is_empty() {
        String::new()
    } else {
        format!("  hardware {}", record.hardware)
    }
}

/// `branch <name>`, or `read-only (nothing is committed or pushed)`.
#[must_use]
pub fn target_text(record: &DispatchRecord) -> String {
    if record.read_only {
        "read-only (nothing is committed or pushed)".to_string()
    } else {
        format!("branch {}", record.branch)
    }
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
        "pool": record.pool,
        "slot": record.slot,
        "outcome": outcome.word(),
    });
    if record.read_only {
        note["read_only"] = serde_json::Value::Bool(true);
    }
    if let Outcome::Finished(done) = outcome {
        for (key, value) in &done.result.fields {
            if !key.starts_with(DISCARDED_PATH_PREFIX)
                && !key.starts_with(RESTORED_PATH_PREFIX)
                && !key.starts_with(facts::KEY_PREFIX)
            {
                note[key] = serde_json::Value::String(value.clone());
            }
        }
        let recorded = done.result.facts();
        if !recorded.is_empty() {
            note["facts"] = serde_json::json!(recorded);
        }
        let discarded = done.result.discarded_paths();
        if !discarded.is_empty() {
            note["discarded"] = serde_json::json!(discarded);
        }
        let restored = done.result.restored_paths();
        if !restored.is_empty() {
            note["restored"] = serde_json::json!(restored);
        }
    }
    note.to_string()
}

/// The reply text of a result: the facts, then the agent's receipt.
#[must_use]
pub fn collected_text(dispatch: &OpenDispatch, outcome: &Outcome) -> String {
    let record = &dispatch.record;
    let mut text = format!(
        "pool result {job}: {word}\npool {pool}  slot {slot}  model {model}\nfrom {from}  {target}",
        job = record.job,
        word = outcome.word(),
        pool = record.pool,
        slot = record.slot,
        model = record.model,
        from = record.from,
        target = target_text(record),
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
    let restored = done.result.restored_paths();
    if !restored.is_empty() {
        text.push_str(&format!(
            "\nrestored {} path{} before commit: {}",
            restored.len(),
            if restored.len() == 1 { "" } else { "s" },
            restored.join(", ")
        ));
    }
    let discarded = done.result.discarded_paths();
    if !discarded.is_empty() {
        text.push_str(&format!(
            "\ndiscarded paths ({} of {}):",
            discarded.len(),
            done.result.get("discarded_count")
        ));
        for path in &discarded {
            text.push_str("\n  ");
            text.push_str(path);
        }
    }
    for (title, body) in [
        ("facts", &facts::text(&done.result.facts())),
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
