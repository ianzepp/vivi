//! One consistent read of the whole store.
//!
//! `load` runs many SELECTs; between two of them the pump may commit a new
//! save, so a reader could mix two saves. Running them inside one deferred
//! read transaction pins a single WAL snapshot. The transaction only reads and
//! is always rolled back, so it takes no write lock and never blocks the pump.

use crate::pump::state::State;
use vivi_error::VivariumError;

use super::build_waits::BuildWait;
use super::{PumpStore, db_err};

/// Exception rows as [`PumpStore::exceptions`] returns them.
pub type ExceptionRows = Vec<(u64, String, String, String)>;

/// State, exception rows and queued build waits, all from one snapshot.
pub struct Snapshot {
    /// The pump state.
    pub state: State,
    /// Exception rows, oldest first.
    pub exceptions: ExceptionRows,
    /// Queued build waits, in need-handle order.
    pub waits: Vec<BuildWait>,
}

impl PumpStore {
    /// The state, the exception rows and the queued build waits, all from one
    /// snapshot. A database that predates the wait table reads as no waits.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the transaction or a read fails.
    pub fn load_snapshot(&self) -> Result<Snapshot, VivariumError> {
        self.conn()
            .execute_batch("BEGIN DEFERRED")
            .map_err(db_err("begin read"))?;
        let read = self.load().and_then(|state| {
            Ok(Snapshot {
                state,
                exceptions: self.exceptions()?,
                waits: self.build_waits()?,
            })
        });
        let conn = self.conn();
        let ended = conn.execute_batch("ROLLBACK").map_err(db_err("end read"));
        let read = read?;
        ended?;
        Ok(read)
    }
}

#[cfg(test)]
#[path = "snapshot_test.rs"]
mod tests;
