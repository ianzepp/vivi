//! The `main_state` table of `pump.db`: small key/value facts about the
//! integration target's main branch.

use rusqlite::{OptionalExtension, params};

use crate::error::VivariumError;

use super::{PumpStore, db_err};

impl PumpStore {
    /// Read one main-state value.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn main_state(&self, key: &str) -> Result<Option<String>, VivariumError> {
        self.conn
            .query_row(
                "SELECT value FROM main_state WHERE key = ?1",
                [key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(db_err("read main state"))
    }

    /// Store a main-state value, replacing any earlier one.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn set_main_state(&self, key: &str, value: &str) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO main_state (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .map_err(db_err("write main state"))?;
        Ok(())
    }

    /// Remove a main-state value; a missing key is not an error.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be deleted.
    pub fn clear_main_state(&self, key: &str) -> Result<(), VivariumError> {
        self.conn
            .execute("DELETE FROM main_state WHERE key = ?1", [key])
            .map_err(db_err("clear main state"))?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "main_state_test.rs"]
mod tests;
