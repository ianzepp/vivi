//! One consistent read of the whole store.
//!
//! `load` runs many SELECTs; between two of them the pump may commit a new
//! save, so a reader could mix two saves. Running them inside one deferred
//! read transaction pins a single WAL snapshot. The transaction only reads and
//! is always rolled back, so it takes no write lock and never blocks the pump.

use crate::error::VivariumError;
use crate::pump::state::State;

use super::{PumpStore, db_err};

/// Exception rows as [`PumpStore::exceptions`] returns them.
pub type ExceptionRows = Vec<(u64, String, String, String)>;

impl PumpStore {
    /// The state and the exception rows, both from one snapshot.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the transaction or a read fails.
    pub fn load_snapshot(&self) -> Result<(State, ExceptionRows), VivariumError> {
        self.conn
            .execute_batch("BEGIN DEFERRED")
            .map_err(db_err("begin read"))?;
        let read = self
            .load()
            .and_then(|state| Ok((state, self.exceptions()?)));
        let ended = self
            .conn
            .execute_batch("ROLLBACK")
            .map_err(db_err("end read"));
        let read = read?;
        ended?;
        Ok(read)
    }
}
