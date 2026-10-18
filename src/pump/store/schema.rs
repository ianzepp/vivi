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
