//! Read-only pool event history from one mailspace database.
//!
//! The monitor reads a project's dispatch/collect history without opening the
//! read-write [`crate::storage::Storage`], whose open runs schema-write
//! transactions. The connection here is read-only at every level:
//! `SQLITE_OPEN_READ_ONLY`, a query-only pragma, and a short busy timeout —
//! it never blocks a writer.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Duration;

use rusqlite::params_from_iter;

use crate::error::VivariumError;
use crate::mailspace::{POOL_EVENT_TYPES, PoolEventRow};
use crate::pool::record::{DispatchRecord, OpenDispatch, fold_open};

/// Subject and addressee role of a task seen in the pool event log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskInfo {
    pub subject: String,
    pub role: String,
}

/// One collected dispatch in a project's history.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FinishedDispatch {
    pub task: String,
    pub subject: String,
    pub role: String,
    pub outcome: String,
    pub model: String,
    pub pool: String,
    pub slot: String,
    pub read_only: bool,
    /// Raw RFC3339 `occurred_at` of the `pool_collected` event.
    pub ended_at: String,
}

/// The pool history of one project, read without the read-write `Storage`.
#[derive(Debug, Clone, Default)]
pub struct ProjectEvents {
    pub open: Vec<OpenDispatch>,
    pub task_info: BTreeMap<String, TaskInfo>,
    pub finished: Vec<FinishedDispatch>,
}

/// One mailspace event as the raw columns the history query needs.
struct EventRow {
    occurred_at: String,
    event_type: String,
    to_role: String,
    to_identity: String,
    subject: String,
    note: String,
    message_id: String,
}

/// The role a task event belongs to: the identity the task copy is held
/// under (`to_identity`, e.g. `hand` — the addressee the mailspace's
/// `pool_task` reports), falling back to the mailbox folder (`to_role`).
fn role_of(row: &EventRow) -> String {
    if row.to_identity.is_empty() {
        row.to_role.clone()
    } else {
        row.to_identity.clone()
    }
}

/// The display handle of a message, as [`crate::storage::Storage`]
/// `display_handle` computes it: the short handle of an active message, else
/// the raw id.
fn display_of(handles: &HashMap<String, String>, message_id: &str) -> String {
    handles
        .get(message_id)
        .cloned()
        .unwrap_or_else(|| message_id.to_string())
}

/// Open a mailspace database strictly for reading.
fn open_read_only(db: &Path) -> Result<rusqlite::Connection, VivariumError> {
    let conn = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| VivariumError::Other(format!("failed to open {} read-only: {e}", db.display())))?;
    conn.busy_timeout(Duration::from_secs(1))
        .map_err(|e| VivariumError::Other(format!("failed to set busy timeout: {e}")))?;
    conn.execute_batch("PRAGMA query_only = true")
        .map_err(|e| VivariumError::Other(format!("failed to set query_only: {e}")))?;
    Ok(conn)
}

/// The message-id to display-handle map for active messages.
fn active_handles(conn: &rusqlite::Connection) -> Result<HashMap<String, String>, VivariumError> {
    let mut stmt = conn
        .prepare("SELECT message_id FROM messages WHERE deleted_at IS NULL ORDER BY message_id")
        .map_err(|e| VivariumError::Other(format!("failed to prepare message id query: {e}")))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| VivariumError::Other(format!("failed to query message ids: {e}")))?;
    let ids = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| VivariumError::Other(format!("failed to read message id row: {e}")))?;
    Ok(crate::storage::short_handle_map(&ids))
}

/// The pool events, oldest first.
fn read_rows(conn: &rusqlite::Connection) -> Result<Vec<EventRow>, VivariumError> {
    let placeholders = POOL_EVENT_TYPES.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT occurred_at, event_type, to_role, to_identity, subject, note, message_id \
         FROM mailspace_events WHERE event_type IN ({placeholders}) ORDER BY event_id"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| VivariumError::Other(format!("failed to prepare event query: {e}")))?;
    let rows = stmt
        .query_map(params_from_iter(POOL_EVENT_TYPES.iter()), |row| {
            Ok(EventRow {
                occurred_at: row.get(0)?,
                event_type: row.get(1)?,
                to_role: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                to_identity: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                subject: row.get(4)?,
                note: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                message_id: row.get(6)?,
            })
        })
        .map_err(|e| VivariumError::Other(format!("failed to query events: {e}")))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| VivariumError::Other(format!("failed to read event row: {e}")))
}

/// `<outcome> <k>=<v>` pairs for the result keys a collect note carries.
fn outcome_text(note: &str) -> String {
    let value = match serde_json::from_str::<serde_json::Value>(note) {
        Ok(value) => value,
        Err(_) => return String::new(),
    };
    let mut text = value
        .get("outcome")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    for key in ["agent_rc", "agent_s", "push", "branch"] {
        let Some(item) = value.get(key) else {
            continue;
        };
        if item.is_null() {
            continue;
        }
        let rendered = item.as_str().map_or_else(|| item.to_string(), str::to_string);
        text.push_str(&format!(" {key}={rendered}"));
    }
    text
}

/// Collected dispatches, oldest first: each `pool_collected` paired with the
/// remembered `pool_dispatched` record of its task.
fn finished_rows(
    rows: &[EventRow],
    handles: &HashMap<String, String>,
) -> Vec<FinishedDispatch> {
    let mut dispatched: HashMap<String, DispatchRecord> = HashMap::new();
    let mut finished = Vec::new();
    for row in rows {
        let handle = display_of(handles, &row.message_id);
        if row.event_type == "pool_dispatched" {
            if let Ok(record) = serde_json::from_str::<DispatchRecord>(&row.note) {
                dispatched.insert(handle, record);
            }
            continue;
        }
        let Some(record) = dispatched.remove(&handle) else {
            continue;
        };
        finished.push(FinishedDispatch {
            task: handle,
            subject: row.subject.clone(),
            role: role_of(row),
            outcome: outcome_text(&row.note),
            model: record.model,
            pool: record.pool,
            slot: record.slot,
            read_only: record.read_only,
            ended_at: row.occurred_at.clone(),
        });
    }
    finished
}

/// Read the pool history of the mailspace database at `db`.
///
/// A missing database reads as an empty history. `last_finished` keeps only
/// the newest that many collected dispatches.
///
/// # Errors
/// Returns an error if the database cannot be opened read-only or a query
/// fails.
pub fn read(db: &Path, last_finished: usize) -> Result<ProjectEvents, VivariumError> {
    if !db.exists() {
        return Ok(ProjectEvents::default());
    }
    let conn = open_read_only(db)?;
    let handles = active_handles(&conn)?;
    let rows = read_rows(&conn)?;
    let mut task_info: BTreeMap<String, TaskInfo> = BTreeMap::new();
    let mut pool_rows = Vec::with_capacity(rows.len());
    for row in &rows {
        let handle = display_of(&handles, &row.message_id);
        task_info.insert(
            handle.clone(),
            TaskInfo {
                subject: row.subject.clone(),
                role: role_of(row),
            },
        );
        pool_rows.push(PoolEventRow {
            handle,
            event_type: row.event_type.clone(),
            note: row.note.clone(),
        });
    }
    let mut finished = finished_rows(&rows, &handles);
    let skip = finished.len().saturating_sub(last_finished);
    finished.drain(..skip);
    Ok(ProjectEvents {
        open: fold_open(&pool_rows),
        task_info,
        finished,
    })
}

#[cfg(test)]
#[path = "events_test.rs"]
mod tests;
