//! Flow link tables of `pump.db`: the fanout step open in a run, the
//! fanout and dependencies of each slice step task, and the entry a restart
//! of a run starts the next run at.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::VivariumError;

use super::{PumpStore, db_err};

const LINKS_DDL: &str = "
CREATE TABLE IF NOT EXISTS fanout_open (
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    fanout TEXT NOT NULL,
    PRIMARY KEY (need, run)
);
CREATE TABLE IF NOT EXISTS slice_links (
    task TEXT PRIMARY KEY,
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    fanout TEXT,
    deps_json TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX IF NOT EXISTS slice_links_run ON slice_links (need, run);
CREATE TABLE IF NOT EXISTS restart_entries (
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    entry TEXT NOT NULL,
    PRIMARY KEY (need, run)
);
";

/// Create the flow link tables when they are missing.
pub(super) fn ensure_tables(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(LINKS_DDL)
        .map_err(db_err("create flow link tables"))
}

/// The fanout and dependencies recorded for one slice (or reslice) step task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceLink {
    pub task: String,
    pub need: String,
    pub run: u32,
    /// The fanout step task this slice belongs to; `None` when none is recorded.
    pub fanout: Option<String>,
    /// Slice step tasks this one depends on.
    pub deps: Vec<String>,
}

type LinkRow = (String, String, u32, Option<String>, String);

fn decode(raw: LinkRow) -> Result<SliceLink, VivariumError> {
    let (task, need, run, fanout, deps_json) = raw;
    let deps = serde_json::from_str(&deps_json).map_err(db_err("decode slice deps"))?;
    Ok(SliceLink {
        task,
        need,
        run,
        fanout,
        deps,
    })
}

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LinkRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

const LINK_COLUMNS: &str = "task, need, run, fanout, deps_json";

impl PumpStore {
    /// Record the fanout step task currently open in a run.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn set_open_fanout(&self, need: &str, run: u32, fanout: &str) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO fanout_open (need, run, fanout) VALUES (?1, ?2, ?3)",
                params![need, run, fanout],
            )
            .map_err(db_err("write open fanout"))?;
        Ok(())
    }

    /// The fanout step task open in a run, when one is recorded.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn open_fanout(&self, need: &str, run: u32) -> Result<Option<String>, VivariumError> {
        self.conn
            .query_row(
                "SELECT fanout FROM fanout_open WHERE need = ?1 AND run = ?2",
                params![need, run],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err("read open fanout"))
    }

    /// Insert or replace the link of one slice step task.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn link_slice(&self, link: &SliceLink) -> Result<(), VivariumError> {
        let deps = serde_json::to_string(&link.deps).map_err(db_err("encode slice deps"))?;
        self.conn
            .execute(
                "INSERT INTO slice_links (task, need, run, fanout, deps_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(task) DO UPDATE SET
                     need = ?2, run = ?3, fanout = ?4, deps_json = ?5",
                params![link.task, link.need, link.run, link.fanout, deps],
            )
            .map_err(db_err("write slice link"))?;
        Ok(())
    }

    /// The link of one slice step task.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read or decoded.
    pub fn slice_link(&self, task: &str) -> Result<Option<SliceLink>, VivariumError> {
        let raw = self
            .conn
            .query_row(
                &format!("SELECT {LINK_COLUMNS} FROM slice_links WHERE task = ?1"),
                [task],
                read_row,
            )
            .optional()
            .map_err(db_err("read slice link"))?;
        raw.map(decode).transpose()
    }

    /// Every slice link of a run, in insertion order.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read or decoded.
    pub fn slice_links_of_run(
        &self,
        need: &str,
        run: u32,
    ) -> Result<Vec<SliceLink>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {LINK_COLUMNS} FROM slice_links
                 WHERE need = ?1 AND run = ?2 ORDER BY rowid"
            ))
            .map_err(db_err("read slice links"))?;
        let rows = stmt
            .query_map(params![need, run], read_row)
            .map_err(db_err("read slice links"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err("read slice links"))?;
        rows.into_iter().map(decode).collect()
    }

    /// Replace the dependencies of a linked slice step task.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the task has no link or the row cannot be written.
    pub fn set_slice_deps(&self, task: &str, deps: &[String]) -> Result<(), VivariumError> {
        let json = serde_json::to_string(deps).map_err(db_err("encode slice deps"))?;
        let changed = self
            .conn
            .execute(
                "UPDATE slice_links SET deps_json = ?2 WHERE task = ?1",
                params![task, json],
            )
            .map_err(db_err("write slice deps"))?;
        if changed == 0 {
            return Err(VivariumError::Parse(format!(
                "no slice link recorded for task {task}"
            )));
        }
        Ok(())
    }

    /// Record the entry a restart of run `run` starts the next run at.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn set_restart_entry(
        &self,
        need: &str,
        run: u32,
        entry: &str,
    ) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO restart_entries (need, run, entry) VALUES (?1, ?2, ?3)",
                params![need, run, entry],
            )
            .map_err(db_err("write restart entry"))?;
        Ok(())
    }

    /// The entry recorded for a restart of run `run`.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn restart_entry(&self, need: &str, run: u32) -> Result<Option<String>, VivariumError> {
        self.conn
            .query_row(
                "SELECT entry FROM restart_entries WHERE need = ?1 AND run = ?2",
                params![need, run],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err("read restart entry"))
    }
}

#[cfg(test)]
#[path = "flow_links_test.rs"]
mod tests;
