//! Read-only access to a project's pump store (`pump.db`).
//!
//! The pump home is the `[pump] home` setting, or `<root>/.vivi/pump` when
//! unset (`PumpConfig::home_dir`). The config is read without opening the
//! mailspace, so nothing is minted or written. The connection is the
//! mailspace helper's (`store::open_reader`): it can never write, and it runs
//! one read transaction per call.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use vivi_core::pump::store::DB_FILENAME;
use vivi_error::VivariumError;

use super::config;
use super::store::{db_err, open_reader};

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
    /// but cannot be opened for reading (see `store::open_reader`).
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
        Ok(Some(Self {
            conn: open_reader(path, "the pump store")?,
        }))
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

#[cfg(test)]
#[path = "pump_db_test.rs"]
mod tests;
