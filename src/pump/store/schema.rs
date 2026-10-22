//! Pump database schema-version and migration-lock helpers.

use std::fs::{File, OpenOptions};
use std::path::Path;

use rusqlite::Connection;

use crate::error::VivariumError;

use super::SCHEMA_VERSION;

pub(super) fn schema_version(conn: &Connection) -> Result<i64, VivariumError> {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| VivariumError::Other(format!("pump.db read user_version: {error}")))
}

pub(super) fn newer_schema(version: i64) -> VivariumError {
    VivariumError::Message(format!(
        "pump.db schema {version} is newer than this binary ({SCHEMA_VERSION}); refusing to open"
    ))
}

pub(super) fn take_migration_lock(path: &Path) -> Result<File, VivariumError> {
    let lock_path = path.parent().unwrap_or_else(|| Path::new(".")).join("lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&lock_path)?;
    lock.try_lock().map_err(|error| {
        VivariumError::Message(format!(
            "cannot migrate pump.db while another pump owns {}: {error}",
            lock_path.display()
        ))
    })?;
    Ok(lock)
}

/// The operator request queues schema 8 retired. Requeue, withdraw and replan
/// are actions on the pump record now, so these tables have no reader.
const RETIRED_QUEUES: [&str; 3] = ["requeue_requests", "withdraw_requests", "replan_requests"];

/// Drop the retired operator request tables, discarding any request the old
/// pump had not applied yet (each is a log line: the operator re-issues it
/// against the need's pump record). A store that never had them is left
/// alone.
pub(super) fn drop_request_queues(conn: &Connection) -> Result<(), VivariumError> {
    for table in RETIRED_QUEUES {
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .map_err(|error| VivariumError::Other(format!("pump.db look for {table}: {error}")))?;
        if exists == 0 {
            continue;
        }
        let unapplied: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE applied_at IS NULL"),
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if unapplied > 0 {
            tracing::warn!(
                table,
                unapplied,
                "pump.db schema {SCHEMA_VERSION} dropped unapplied operator requests; re-issue them against the need's open pump record"
            );
        }
        conn.execute(&format!("DROP TABLE {table}"), [])
            .map_err(|error| VivariumError::Other(format!("pump.db drop {table}: {error}")))?;
    }
    Ok(())
}
