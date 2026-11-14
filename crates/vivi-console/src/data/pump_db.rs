//! Read-only access to a project's pump store (`pump.db`).
//!
//! The pump home is the `[pump] home` setting, or `<root>/.vivi/pump` when
//! unset (`PumpConfig::home_dir`). The config is read without opening the
//! mailspace, so nothing is minted or written. The connection follows the
//! mailspace helper in `store.rs`: read-only flags, the query-only setting, a
//! short busy timeout, and one read transaction per call.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use vivi_core::pump::store::DB_FILENAME;
use vivi_error::VivariumError;

use super::config;
use super::store::db_err;
use crate::queries::QUERY_ONLY;

const BUSY_TIMEOUT: Duration = Duration::from_millis(250);

/// The pump store path for the project at `root`, or `None` when the project
/// configures no pump.
///
/// # Errors
/// Returns an error when the project config cannot be read.
pub fn pump_db_path(root: &Path) -> Result<Option<PathBuf>, VivariumError> {
    let config = config::load(root)?;
    Ok(config
        .pump
        .map(|pump| pump.home_dir(root).join(DB_FILENAME)))
}

/// A pump store opened strictly for reading.
pub struct PumpRead {
    conn: Connection,
}

impl PumpRead {
    /// Open the pump store of the project at `root`; `None` when the project
    /// has no pump or the store does not exist yet.
    ///
    /// # Errors
    /// Returns an error when the config cannot be read, or the store exists
    /// but cannot be opened read-only, or would need a missing WAL sidecar.
    pub fn open(root: &Path) -> Result<Option<Self>, VivariumError> {
        let Some(path) = pump_db_path(root)? else {
            return Ok(None);
        };
        Self::open_path(&path)
    }

    /// Open the pump store at `path`; `None` when it does not exist.
    ///
    /// # Errors
    /// As [`PumpRead::open`].
    pub fn open_path(path: &Path) -> Result<Option<Self>, VivariumError> {
        if !path.exists() {
            return Ok(None);
        }
        if wal_index_missing(path) {
            return Err(VivariumError::Other(
                "the pump store cannot be read yet: its WAL index file is missing and \
                 the console never creates it"
                    .into(),
            ));
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn =
            Connection::open_with_flags(path, flags).map_err(db_err("open the pump store"))?;
        conn.busy_timeout(BUSY_TIMEOUT)
            .map_err(db_err("set the busy timeout"))?;
        conn.execute_batch(QUERY_ONLY)
            .map_err(db_err("set query_only"))?;
        Ok(Some(Self { conn }))
    }

    /// Run `f` inside one read transaction; it never writes.
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
}

/// Whether a WAL-mode database at `path` lacks its `-shm` sidecar, which a
/// read-only open would have to create. The header bytes 18 and 19 are 2 in
/// WAL mode.
fn wal_index_missing(path: &Path) -> bool {
    use std::io::Read;
    let mut shm = path.as_os_str().to_owned();
    shm.push("-shm");
    if Path::new(&shm).exists() {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut header = [0_u8; 20];
    file.read_exact(&mut header).is_ok() && header[18] == 2 && header[19] == 2
}

#[cfg(test)]
#[path = "pump_db_test.rs"]
mod tests;
