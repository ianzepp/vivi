//! `slot_runs`: one derived row per pool job, kept in step with the pool events.
//!
//! The `pool_dispatched` and `pool_collected` events stay the source of truth.
//! The row is written in the same transaction as the event, so the index never
//! drifts from the log, and [`Storage::rebuild_slot_runs`] replays the log to
//! make it again.

use rusqlite::{Transaction, params};
use serde::Deserialize;
use serde_json::Value;

use super::{Storage, VivariumError};

mod read;

pub use read::SlotRun;

#[cfg(test)]
#[path = "slot_runs_test.rs"]
mod tests;

/// The `slot_runs` table and its indexes.
pub const SLOT_RUNS_DDL: &str = "CREATE TABLE IF NOT EXISTS slot_runs (
  job TEXT PRIMARY KEY,
  task_message_id TEXT NOT NULL,
  need TEXT,
  pool TEXT NOT NULL,
  slot TEXT NOT NULL,
  machine TEXT NOT NULL DEFAULT '',
  model TEXT NOT NULL DEFAULT '',
  kind TEXT,
  target TEXT,
  branch TEXT NOT NULL DEFAULT '',
  start_sha TEXT NOT NULL DEFAULT '',
  budget_s INTEGER,
  read_only INTEGER NOT NULL DEFAULT 0,
  dispatched_at INTEGER NOT NULL,
  collected_at INTEGER,
  outcome TEXT,
  push TEXT,
  sha TEXT,
  agent_rc INTEGER,
  agent_s INTEGER,
  check_rc INTEGER,
  check_s INTEGER,
  job_s INTEGER,
  agent_cpu_s INTEGER,
  check_cpu_s INTEGER,
  landing TEXT,
  landing_at INTEGER,
  landing_reason TEXT
);
CREATE INDEX IF NOT EXISTS slot_runs_slot_idx ON slot_runs(pool, slot, dispatched_at);
CREATE INDEX IF NOT EXISTS slot_runs_need_idx ON slot_runs(need, dispatched_at);
CREATE INDEX IF NOT EXISTS slot_runs_outcome_idx ON slot_runs(outcome, collected_at);
CREATE INDEX IF NOT EXISTS slot_runs_dispatched_idx ON slot_runs(dispatched_at);
CREATE INDEX IF NOT EXISTS slot_runs_collected_idx ON slot_runs(collected_at);
CREATE INDEX IF NOT EXISTS slot_runs_task_idx ON slot_runs(task_message_id);
";

/// The fields of a `pool_dispatched` note that `slot_runs` keeps.
#[derive(Debug, Deserialize)]
struct Dispatched {
    job: String,
    #[serde(default)]
    pool: String,
    slot: String,
    #[serde(default)]
    machine: String,
    #[serde(default)]
    model: String,
    need: Option<String>,
    kind: Option<String>,
    target: Option<String>,
    #[serde(default)]
    branch: String,
    #[serde(default)]
    start: String,
    budget: Option<i64>,
    at: i64,
    #[serde(default)]
    read_only: bool,
}

/// One event as the index sees it.
pub(super) struct PoolEvent<'a> {
    pub event_type: &'a str,
    pub note: Option<&'a str>,
    pub message_id: &'a str,
    /// When the store stamped the event (RFC 3339).
    pub occurred_at: &'a str,
}

/// Fold one event into `slot_runs`. Events of other types change nothing. A
/// note that is not the expected JSON is skipped with a warning: the event is
/// already written, and a bad note must never fail or undo it.
///
/// # Errors
/// Returns an error only when the table write itself fails.
pub(super) fn apply_event(
    tx: &Transaction<'_>,
    event: &PoolEvent<'_>,
) -> Result<(), VivariumError> {
    let outcome = match event.event_type {
        "pool_dispatched" => upsert_dispatched(tx, event),
        "pool_collected" => upsert_collected(tx, event),
        _ => return Ok(()),
    };
    match outcome {
        Ok(()) => Ok(()),
        Err(Skip::Note(reason)) => {
            tracing::warn!(
                message_id = event.message_id,
                "slot_runs: skipped a {} note: {reason}",
                event.event_type
            );
            Ok(())
        }
        Err(Skip::Store(error)) => Err(error),
    }
}

/// [`apply_event`] for the live event write: a failed table write is logged
/// and dropped, never returned. The events are the source of truth and the
/// index is derived, so a fault in the index must not undo a pool event (and
/// with it fail a dispatch or lose a result). `vivi slot rebuild` repairs any
/// drift this leaves.
pub(super) fn apply_event_or_warn(tx: &Transaction<'_>, event: &PoolEvent<'_>) {
    if let Err(error) = apply_event(tx, event) {
        tracing::warn!(
            message_id = event.message_id,
            "slot_runs: index not updated for a {} event: {error}; run `vivi slot rebuild`",
            event.event_type
        );
    }
}

/// Why an event did not become a row.
enum Skip {
    /// The note is not the record the event type promises.
    Note(String),
    /// The table write failed.
    Store(VivariumError),
}

impl From<rusqlite::Error> for Skip {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(VivariumError::Other(format!(
            "failed to write slot_runs: {error}"
        )))
    }
}

fn note_of<'a>(event: &PoolEvent<'a>) -> Result<&'a str, Skip> {
    event
        .note
        .filter(|note| !note.trim().is_empty())
        .ok_or_else(|| Skip::Note("the event has no note".into()))
}

fn upsert_dispatched(tx: &Transaction<'_>, event: &PoolEvent<'_>) -> Result<(), Skip> {
    let record: Dispatched =
        serde_json::from_str(note_of(event)?).map_err(|error| Skip::Note(error.to_string()))?;
    if record.job.is_empty() {
        return Err(Skip::Note("the dispatch record names no job".into()));
    }
    tx.execute(
        "INSERT INTO slot_runs (
           job, task_message_id, need, pool, slot, machine, model, kind, target,
           branch, start_sha, budget_s, read_only, dispatched_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(job) DO UPDATE SET
           task_message_id = excluded.task_message_id, need = excluded.need,
           pool = excluded.pool, slot = excluded.slot, machine = excluded.machine,
           model = excluded.model, kind = excluded.kind, target = excluded.target,
           branch = excluded.branch, start_sha = excluded.start_sha,
           budget_s = excluded.budget_s, read_only = excluded.read_only,
           dispatched_at = excluded.dispatched_at",
        params![
            record.job,
            event.message_id,
            record.need,
            record.pool,
            record.slot,
            record.machine,
            record.model,
            record.kind,
            record.target,
            record.branch,
            record.start,
            record.budget,
            record.read_only,
            record.at,
        ],
    )?;
    Ok(())
}

/// A string field of a collect note; empty or missing is `None`.
fn text(note: &Value, key: &str) -> Option<String> {
    note.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// An integer field of a collect note: a numeric string or a number reads;
/// empty, missing or anything else is `None`.
fn number(note: &Value, key: &str) -> Option<i64> {
    match note.get(key)? {
        Value::String(value) => match value.trim().parse() {
            Ok(number) => Some(number),
            Err(_) => None,
        },
        Value::Number(value) => value.as_i64(),
        _ => None,
    }
}

fn upsert_collected(tx: &Transaction<'_>, event: &PoolEvent<'_>) -> Result<(), Skip> {
    let note: Value =
        serde_json::from_str(note_of(event)?).map_err(|error| Skip::Note(error.to_string()))?;
    let (Some(job), Some(slot)) = (text(&note, "job"), text(&note, "slot")) else {
        return Err(Skip::Note("the collect record names no job or slot".into()));
    };
    let collected_at = chrono::DateTime::parse_from_rfc3339(event.occurred_at)
        .map_err(|error| Skip::Note(format!("event time {}: {error}", event.occurred_at)))?
        .timestamp();
    tx.execute(
        "INSERT INTO slot_runs (
           job, task_message_id, pool, slot, read_only, dispatched_at, collected_at,
           outcome, push, sha, agent_rc, agent_s, check_rc, check_s, job_s,
           agent_cpu_s, check_cpu_s
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT(job) DO UPDATE SET
           collected_at = excluded.collected_at, outcome = excluded.outcome,
           push = excluded.push, sha = excluded.sha, agent_rc = excluded.agent_rc,
           agent_s = excluded.agent_s, check_rc = excluded.check_rc,
           check_s = excluded.check_s, job_s = excluded.job_s,
           agent_cpu_s = excluded.agent_cpu_s, check_cpu_s = excluded.check_cpu_s",
        params![
            job,
            event.message_id,
            text(&note, "pool").unwrap_or_default(),
            slot,
            note.get("read_only")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            collected_at,
            text(&note, "outcome"),
            text(&note, "push"),
            text(&note, "sha"),
            number(&note, "agent_rc"),
            number(&note, "agent_s"),
            number(&note, "check_rc"),
            number(&note, "check_s"),
            number(&note, "job_s"),
            number(&note, "agent_cpu_s"),
            number(&note, "check_cpu_s"),
        ],
    )?;
    Ok(())
}

/// One stored pool event: time, type, message id, note.
type StoredPoolEvent = (String, String, String, Option<String>);

fn read_pool_events(tx: &Transaction<'_>) -> Result<Vec<StoredPoolEvent>, VivariumError> {
    let fail =
        |e: rusqlite::Error| VivariumError::Other(format!("failed to read pool events: {e}"));
    let mut stmt = tx
        .prepare(
            "SELECT occurred_at, event_type, message_id, note FROM mailspace_events
             WHERE event_type IN ('pool_dispatched', 'pool_collected')
             ORDER BY occurred_at, event_id",
        )
        .map_err(fail)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .map_err(fail)?;
    rows.collect::<Result<_, _>>().map_err(fail)
}

impl Storage {
    /// Rebuild `slot_runs` from the pool events: in one transaction, drop every
    /// row and replay each `pool_dispatched` and `pool_collected` event, oldest
    /// first, through the code that writes the row as the event is recorded.
    /// Running it again gives the same rows. Returns the number of rows.
    ///
    /// # Errors
    /// Returns an error if the events cannot be read or a row cannot be
    /// written; the table is then left as it was.
    pub fn rebuild_slot_runs(&self) -> Result<usize, VivariumError> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn
            .transaction()
            .map_err(|e| VivariumError::Other(format!("failed to open slot_runs rebuild: {e}")))?;
        let events = read_pool_events(&tx)?;
        tx.execute("DELETE FROM slot_runs", [])
            .map_err(|e| VivariumError::Other(format!("failed to clear slot_runs: {e}")))?;
        for (occurred_at, event_type, message_id, note) in &events {
            let event = PoolEvent {
                event_type,
                note: note.as_deref(),
                message_id,
                occurred_at,
            };
            apply_event(&tx, &event)?;
        }
        let rows: i64 = tx
            .query_row("SELECT COUNT(*) FROM slot_runs", [], |row| row.get(0))
            .map_err(|e| VivariumError::Other(format!("failed to count slot_runs: {e}")))?;
        tx.commit().map_err(|e| {
            VivariumError::Other(format!("failed to commit slot_runs rebuild: {e}"))
        })?;
        Ok(usize::try_from(rows).unwrap_or(0))
    }
}
