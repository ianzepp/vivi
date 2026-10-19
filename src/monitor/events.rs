//! Read-only pool event history from one mailspace database.
//!
//! The monitor reads a project's dispatch/collect history without opening the
//! read-write [`crate::storage::Storage`], whose open runs schema-write
//! transactions. The connection here is read-only at every level:
//! `SQLITE_OPEN_READ_ONLY`, a query-only pragma, and a short busy timeout —
//! it never blocks a writer.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use rusqlite::params_from_iter;

use crate::error::VivariumError;
use crate::mailspace::{POOL_EVENT_TYPES, PoolEventRow};
use crate::pool::record::{DispatchRecord, OpenDispatch, fold_open};
use crate::storage::Storage;

/// Subject and addressee role of a task seen in the pool event log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskInfo {
    pub subject: String,
    pub role: String,
    pub need: Option<String>,
    pub kind: Option<String>,
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
    pub need: Option<String>,
    pub kind: Option<String>,
    pub slot: String,
    pub machine: String,
    pub started_at: u64,
    pub budget_secs: u64,
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
    active: bool,
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
    let dir = db.parent().ok_or_else(|| {
        VivariumError::Other(format!(
            "mail database path has no parent: {}",
            db.display()
        ))
    })?;
    let conn = Storage::open_mailspace_connection_read_only(dir)?;
    conn.busy_timeout(Duration::from_secs(1))
        .map_err(|e| VivariumError::Other(format!("failed to set busy timeout: {e}")))?;
    conn.execute_batch("PRAGMA query_only = true")
        .map_err(|e| VivariumError::Other(format!("failed to set query_only: {e}")))?;
    Ok(conn)
}

/// The pool events, oldest first.
fn read_rows(conn: &rusqlite::Connection) -> Result<Vec<EventRow>, VivariumError> {
    let placeholders = POOL_EVENT_TYPES
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT e.occurred_at, e.event_type, e.to_role, e.to_identity, e.subject, e.note, \
                e.message_id, m.deleted_at IS NULL \
         FROM mailspace_events e JOIN messages m ON m.message_id = e.message_id \
         WHERE e.event_type IN ({placeholders}) ORDER BY e.event_id"
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
                active: row.get(7)?,
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
        let rendered = item
            .as_str()
            .map_or_else(|| item.to_string(), str::to_string);
        text.push_str(&format!(" {key}={rendered}"));
    }
    text
}

/// Collected dispatches, oldest first: each `pool_collected` paired with the
/// remembered `pool_dispatched` record of its task.
fn finished_rows(rows: &[EventRow], handles: &HashMap<String, String>) -> Vec<FinishedDispatch> {
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
            need: record.need,
            kind: record.kind,
            slot: record.slot,
            machine: record.machine,
            started_at: record.at,
            budget_secs: record.budget,
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
    let rows = read_rows(&conn)?;
    let mut ids: Vec<String> = rows
        .iter()
        .filter(|row| row.active)
        .map(|row| row.message_id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    let handles = crate::storage::short_handle_map(&ids);
    let mut task_info: BTreeMap<String, TaskInfo> = BTreeMap::new();
    let mut pool_rows = Vec::with_capacity(rows.len());
    for row in &rows {
        let handle = display_of(&handles, &row.message_id);
        task_info.insert(
            handle.clone(),
            TaskInfo {
                subject: row.subject.clone(),
                role: role_of(row),
                need: None,
                kind: None,
            },
        );
        pool_rows.push(PoolEventRow {
            handle,
            event_type: row.event_type.clone(),
            note: row.note.clone(),
        });
    }
    let open = fold_open(&pool_rows);
    let mut finished = finished_rows(&rows, &handles);
    let skip = finished.len().saturating_sub(last_finished);
    finished.drain(..skip);
    let relevant: HashSet<&str> = open
        .iter()
        .map(|dispatch| dispatch.task.as_str())
        .chain(finished.iter().map(|dispatch| dispatch.task.as_str()))
        .collect();
    task_info.retain(|handle, _| relevant.contains(handle.as_str()));
    add_record_metadata(&mut task_info, &open, &finished);
    let dir = db.parent().ok_or_else(|| {
        VivariumError::Other(format!(
            "mail database path has no parent: {}",
            db.display()
        ))
    })?;
    if task_info.values().any(|info| info.need.is_none()) {
        add_task_need_metadata(&mut task_info, dir)?;
    }
    Ok(ProjectEvents {
        open,
        task_info,
        finished,
    })
}

fn add_record_metadata(
    task_info: &mut BTreeMap<String, TaskInfo>,
    open: &[OpenDispatch],
    finished: &[FinishedDispatch],
) {
    for dispatch in open {
        let info = task_info.entry(dispatch.task.clone()).or_default();
        info.need.clone_from(&dispatch.record.need);
        info.kind.clone_from(&dispatch.record.kind);
    }
    for dispatch in finished {
        let info = task_info.entry(dispatch.task.clone()).or_default();
        info.need.clone_from(&dispatch.need);
        info.kind.clone_from(&dispatch.kind);
    }
}

fn add_task_need_metadata(
    task_info: &mut BTreeMap<String, TaskInfo>,
    dir: &Path,
) -> Result<(), VivariumError> {
    let storage = Storage::open_mailspace_read_only(dir)?;
    for (handle, info) in task_info.iter_mut().filter(|(_, info)| info.need.is_none()) {
        let Ok(id) = storage.resolve_message_token(handle) else {
            continue;
        };
        let metadata = storage.item_metadata(&id)?;
        if metadata.get("source_kind").map(String::as_str) == Some("need") {
            info.need = metadata
                .get("source_handle")
                .filter(|handle| !handle.is_empty())
                .cloned();
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "events_test.rs"]
mod tests;
