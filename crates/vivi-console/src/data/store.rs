//! Read-only access to a project's mailspace store.
//!
//! The connection is read-only at every level: `SQLITE_OPEN_READ_ONLY`,
//! the query-only setting, and a short busy timeout so a page never holds up a
//! writer. The store is never created, migrated or written. The pattern is
//! the monitor's (`vivi-monitor`, `monitor/events.rs`), with the same
//! refusal to create a missing WAL shared-memory sidecar.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use vivi_core::mailspace::MAILSPACE_DIR;
use vivi_error::VivariumError;
use vivi_storage::storage::Storage;

use crate::queries::{HAS_TABLE, QUERY_ONLY};

const DB_FILE: &str = "mail.sqlite";
const BUSY_TIMEOUT: Duration = Duration::from_millis(250);

/// The `.vivi` directory under a project root.
#[must_use]
pub fn mailspace_dir(root: &Path) -> PathBuf {
    root.join(MAILSPACE_DIR)
}

/// A mailspace database opened strictly for reading.
pub struct ReadStore {
    conn: Connection,
}

impl ReadStore {
    /// Open the store under `root`; `None` when the project has no store.
    ///
    /// # Errors
    /// Returns an error when the database exists but cannot be opened
    /// read-only, or would need a missing WAL sidecar created.
    pub fn open(root: &Path) -> Result<Option<Self>, VivariumError> {
        let dir = mailspace_dir(root);
        let db = dir.join(DB_FILE);
        if !db.exists() {
            return Ok(None);
        }
        if Storage::read_only_wal_index_missing(&dir) {
            return Err(VivariumError::Other(
                "the store cannot be read yet: its WAL index file is missing and the \
                 console never creates it"
                    .into(),
            ));
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(&db, flags).map_err(db_err("open the store"))?;
        conn.busy_timeout(BUSY_TIMEOUT)
            .map_err(db_err("set the busy timeout"))?;
        conn.execute_batch(QUERY_ONLY)
            .map_err(db_err("set query_only"))?;
        Ok(Some(Self { conn }))
    }

    /// Run `f` inside one read transaction. The transaction is rolled back
    /// when `f` returns; it never writes.
    ///
    /// # Errors
    /// Returns an error when the transaction cannot begin, or `f` fails.
    pub fn read<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, VivariumError>,
    ) -> Result<T, VivariumError> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin a read transaction"))?;
        f(&tx)
    }

    /// The raw connection, for tests that probe the read-only guards.
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

/// Whether the store has a table named `name`.
///
/// # Errors
/// Returns an error when the schema table cannot be read.
pub fn has_table(conn: &Connection, name: &str) -> Result<bool, VivariumError> {
    conn.prepare_cached(HAS_TABLE.sql)
        .and_then(|mut stmt| stmt.exists([name]))
        .map_err(db_err("look for a table"))
}

/// An error mapper that names what was being done.
pub fn db_err(context: &'static str) -> impl Fn(rusqlite::Error) -> VivariumError {
    move |error| VivariumError::Other(format!("failed to {context}: {error}"))
}

#[cfg(test)]
#[path = "store_test.rs"]
mod tests;
