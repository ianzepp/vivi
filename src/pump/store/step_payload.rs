//! The `step_payloads` table of `pump.db`: the payload a finished flow step
//! carried, kept so code decisions and later briefs can read it.
//!
//! The table can be rebuilt from the step replies, so there is no migration of
//! old rows.

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::VivariumError;

use super::{PumpStore, db_err};

/// One step's stored payload.
#[derive(Debug, Clone, PartialEq)]
pub struct StepPayloadRow {
    pub need: String,
    pub run: u32,
    pub node: String,
    pub class: String,
    pub task: String,
    pub payload: serde_json::Value,
}

const DDL: &str = "
CREATE TABLE IF NOT EXISTS step_payloads (
    task TEXT PRIMARY KEY,
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    node TEXT NOT NULL,
    class TEXT NOT NULL,
    payload_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS step_payloads_run ON step_payloads (need, run);
";

const COLUMNS: &str = "need, run, node, class, task, payload_json";

pub(super) fn ensure_table(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(DDL)
        .map_err(db_err("create step payload table"))
}

fn raw_row(row: &Row<'_>) -> rusqlite::Result<(StepPayloadRow, String)> {
    Ok((
        StepPayloadRow {
            need: row.get(0)?,
            run: row.get(1)?,
            node: row.get(2)?,
            class: row.get(3)?,
            task: row.get(4)?,
            payload: serde_json::Value::Null,
        },
        row.get(5)?,
    ))
}

fn decode((mut row, text): (StepPayloadRow, String)) -> Result<StepPayloadRow, VivariumError> {
    row.payload = serde_json::from_str(&text).map_err(db_err("decode step payload"))?;
    Ok(row)
}

impl PumpStore {
    /// Store a step's payload, replacing the earlier payload of the same task.
    /// A replaced row keeps its position.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be encoded or written.
    pub fn record_step_payload(&self, row: &StepPayloadRow) -> Result<(), VivariumError> {
        let text = serde_json::to_string(&row.payload).map_err(db_err("encode step payload"))?;
        self.conn
            .execute(
                "INSERT INTO step_payloads (task, need, run, node, class, payload_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(task) DO UPDATE SET
                     need = ?2, run = ?3, node = ?4, class = ?5, payload_json = ?6",
                params![row.task, row.need, row.run, row.node, row.class, text],
            )
            .map_err(db_err("write step payload"))?;
        Ok(())
    }

    /// Read the payload of one step task.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read or decoded.
    pub fn step_payload(&self, task: &str) -> Result<Option<StepPayloadRow>, VivariumError> {
        let raw = self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM step_payloads WHERE task = ?1"),
                [task],
                raw_row,
            )
            .optional()
            .map_err(db_err("read step payload"))?;
        raw.map(decode).transpose()
    }

    /// Every stored payload of one run, oldest first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when a row cannot be read or decoded.
    pub fn step_payloads_of_run(
        &self,
        need: &str,
        run: u32,
    ) -> Result<Vec<StepPayloadRow>, VivariumError> {
        self.payload_rows(
            &format!(
                "SELECT {COLUMNS} FROM step_payloads WHERE need = ?1 AND run = ?2 ORDER BY rowid"
            ),
            params![need, run],
        )
    }

    /// Every stored payload of one need, by run and then oldest first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when a row cannot be read or decoded.
    pub fn step_payloads_of_need(&self, need: &str) -> Result<Vec<StepPayloadRow>, VivariumError> {
        self.payload_rows(
            &format!("SELECT {COLUMNS} FROM step_payloads WHERE need = ?1 ORDER BY run, rowid"),
            params![need],
        )
    }

    /// The need and run of a step task, read from the flow step table.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn step_run_of(&self, task: &str) -> Result<Option<(String, u32)>, VivariumError> {
        self.conn
            .query_row(
                "SELECT need, run FROM flow_steps WHERE task = ?1",
                [task],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?)),
            )
            .optional()
            .map_err(db_err("read step run"))
    }

    fn payload_rows(
        &self,
        sql: &str,
        args: impl rusqlite::Params,
    ) -> Result<Vec<StepPayloadRow>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(db_err("prepare step payload read"))?;
        let raw = stmt
            .query_map(args, raw_row)
            .map_err(db_err("read step payloads"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err("read step payloads"))?;
        raw.into_iter().map(decode).collect()
    }
}

#[cfg(test)]
#[path = "step_payload_test.rs"]
mod tests;
