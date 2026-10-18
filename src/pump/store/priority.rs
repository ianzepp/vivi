//! Need-priority rows of `pump.db`.
//!
//! Priority is operator state that sits beside `pump_control`: a CLI writes
//! it while a pump runs, and the pump reads it each tick, so it is never part
//! of [`State`](crate::pump::state::State) and no state save touches it.

use rusqlite::params;

use crate::error::VivariumError;

use super::{PumpStore, db_err};

impl PumpStore {
    /// Set one need's dispatch priority; `level` 0 removes the row. Priority
    /// is operator state beside `pump_control`, never part of [`State`].
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn set_need_priority(
        &mut self,
        need: &str,
        level: i64,
        at: u64,
    ) -> Result<(), VivariumError> {
        if level == 0 {
            return self.clear_need_priority(need);
        }
        self.conn
            .execute(
                "INSERT INTO need_priority (need, level, set_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(need) DO UPDATE SET level = ?2, set_at = ?3",
                params![need, level, i64::try_from(at).unwrap_or(i64::MAX)],
            )
            .map_err(db_err("write need priority"))?;
        Ok(())
    }

    /// Remove one need's priority row.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be deleted.
    pub fn clear_need_priority(&mut self, need: &str) -> Result<(), VivariumError> {
        self.conn
            .execute("DELETE FROM need_priority WHERE need = ?1", [need])
            .map_err(db_err("clear need priority"))?;
        Ok(())
    }

    /// Every need priority, highest level first, then by need handle.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the table cannot be read.
    pub fn need_priorities(&self) -> Result<Vec<(String, i64)>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare("SELECT need, level FROM need_priority ORDER BY level DESC, need")
            .map_err(db_err("prepare need priorities"))?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(db_err("read need priorities"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err("read need priority row"))
    }
}
