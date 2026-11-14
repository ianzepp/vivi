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

use crate::queries::{HAS_TABLE, QUERY_ONLY, SCHEMA_PROBE};

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
    /// Returns an error when the database exists but cannot be opened for
    /// reading (see [`open_reader`]).
    pub fn open(root: &Path) -> Result<Option<Self>, VivariumError> {
        let db = mailspace_dir(root).join(DB_FILE);
        if !db.exists() {
            return Ok(None);
        }
        Ok(Some(Self {
            conn: open_reader(&db, "the store")?,
        }))
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

/// Open an existing database for reading only.
///
/// The connection can never change the database: `query_only` refuses every
/// write, and the file is never created (`SQLITE_OPEN_CREATE` is not set).
/// It is opened read-write at the file level when it can be, so that SQLite
/// may recreate the WAL index (`-shm`) that it deletes whenever the last
/// connection closes: the server opens its stores only while it works, so
/// between requests that index is usually gone, and a strictly read-only
/// open could not read the store at all. The index is SQLite's own shared
/// memory, not data. When the file cannot be opened read-write (another
/// owner), it falls back to a read-only open, which needs the index present.
///
/// # Errors
/// Returns an error when neither open works, naming `what` was opened.
pub fn open_reader(db: &Path, what: &'static str) -> Result<Connection, VivariumError> {
    let writable = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = match Connection::open_with_flags(db, writable) {
        Ok(conn) => conn,
        Err(_) if wal_index_missing(db) => {
            return Err(VivariumError::Other(format!(
                "{what} cannot be read: it may not be opened for writing, and a read-only \
                 open needs its WAL index file, which is missing"
            )));
        }
        Err(_) => {
            let read_only = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
            Connection::open_with_flags(db, read_only).map_err(db_err("open a store"))?
        }
    };
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(db_err("set the busy timeout"))?;
    conn.execute_batch(QUERY_ONLY)
        .map_err(db_err("set query_only"))?;
    // SQLite reads the file lazily; read the schema now so a file that is
    // not a database (or not readable) fails here, not on the first page query.
    conn.query_row(SCHEMA_PROBE, [], |row| row.get::<_, i64>(0))
        .map_err(db_err("read the schema"))?;
    Ok(conn)
}

/// Whether a WAL-mode database at `db` lacks its `-shm` index file. Bytes 18
/// and 19 of the header are 2 in WAL mode.
fn wal_index_missing(db: &Path) -> bool {
    use std::io::Read;
    let mut shm = db.as_os_str().to_owned();
    shm.push("-shm");
    if Path::new(&shm).exists() {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(db) else {
        return false;
    };
    let mut header = [0_u8; 20];
    file.read_exact(&mut header).is_ok() && header[18] == 2 && header[19] == 2
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
