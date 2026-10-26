//! The continuation table of `pump.db`: the park-task-to-source-task link.
//!
//! A park step may record which executable task's verdict parked the run.
//! The origin resolver in `flow::continuation` prefers the recorded link and
//! walks the topology for park tasks written before the link existed. The
//! table is created with `IF NOT EXISTS` on every open and is not part of
//! the `State` save, so `save()` and `clear()` never touch it. The schema
//! version is not bumped here.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::VivariumError;

use super::{PumpStore, db_err};

const CONTINUATION_DDL: &str = "
CREATE TABLE IF NOT EXISTS flow_continuations (
    task TEXT PRIMARY KEY,
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    node TEXT NOT NULL,
    source TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS flow_continuations_run ON flow_continuations (need, run);
";

/// Create the continuation table when it is missing.
pub(super) fn ensure_tables(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(CONTINUATION_DDL)
        .map_err(db_err("create flow continuation table"))
}

/// One park-task-to-source-task link of one need and run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationLink {
    /// The park step task the link belongs to.
    pub task: String,
    /// The need of the run both steps belong to.
    pub need: String,
    /// The run both steps belong to.
    pub run: u32,
    /// The park step's node id.
    pub node: String,
    /// The executable source task the park continued from.
    pub source: String,
}

const LINK_COLUMNS: &str = "task, need, run, node, source";

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContinuationLink> {
    Ok(ContinuationLink {
        task: row.get(0)?,
        need: row.get(1)?,
        run: row.get(2)?,
        node: row.get(3)?,
        source: row.get(4)?,
    })
}

impl PumpStore {
    /// Record the continuation link of one park step task. Writing the same
    /// link again succeeds unchanged; a second, different source for a park
    /// task that already has one is refused. A link is only valid when both
    /// of its steps sit in `flow_steps` under the link's need and run, and
    /// the link's node is the park step's own node.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when validation fails or the row cannot be
    /// written.
    pub fn link_continuation(&self, link: &ContinuationLink) -> Result<(), VivariumError> {
        self.validate_link(link)?;
        if let Some(current) = self.continuation_of(&link.task)? {
            return finish_repeat(&current, link);
        }
        self.conn
            .execute(
                &format!(
                    "INSERT INTO flow_continuations ({LINK_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5)"
                ),
                params![link.task, link.need, link.run, link.node, link.source],
            )
            .map_err(db_err("write flow continuation"))?;
        Ok(())
    }

    /// The continuation link recorded for one park step task, when one is.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn continuation_of(&self, task: &str) -> Result<Option<ContinuationLink>, VivariumError> {
        self.conn
            .query_row(
                &format!("SELECT {LINK_COLUMNS} FROM flow_continuations WHERE task = ?1"),
                [task],
                read_row,
            )
            .optional()
            .map_err(db_err("read flow continuation"))
    }

    /// Both steps of a link must sit in `flow_steps` under the link's need
    /// and run, and the link's node must be the park step's own node.
    fn validate_link(&self, link: &ContinuationLink) -> Result<(), VivariumError> {
        match self.step_place(&link.task)? {
            Some((need, run, node)) if need == link.need && run == link.run => {
                if node != link.node {
                    return Err(VivariumError::Other(format!(
                        "continuation link: park task {} sits at node {node}, not {}",
                        link.task, link.node
                    )));
                }
            }
            _ => {
                return Err(VivariumError::Other(format!(
                    "continuation link: park task {} is not a step of need {} r{}",
                    link.task, link.need, link.run
                )));
            }
        }
        self.check_source(link)
    }

    /// The source step must also sit in the same need and run.
    fn check_source(&self, link: &ContinuationLink) -> Result<(), VivariumError> {
        let ok = self
            .step_place(&link.source)?
            .is_some_and(|(need, run, _)| need == link.need && run == link.run);
        if ok {
            return Ok(());
        }
        Err(VivariumError::Other(format!(
            "continuation link: source {} is not a step of need {} r{}",
            link.source, link.need, link.run
        )))
    }

    /// Where a step sits: `(need, run, node)`, or `None` for no such step.
    fn step_place(&self, task: &str) -> Result<Option<(String, u32, String)>, VivariumError> {
        self.conn
            .query_row(
                "SELECT need, run, node FROM flow_steps WHERE task = ?1",
                [task],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(db_err("read flow step place"))
    }
}

/// A repeated write of the identical link is a no-op; any other second link
/// for the same park task is refused.
fn finish_repeat(current: &ContinuationLink, link: &ContinuationLink) -> Result<(), VivariumError> {
    if current == link {
        return Ok(());
    }
    Err(VivariumError::Other(format!(
        "park task {}: already linked to source {}; refusing a link to {}",
        link.task, current.source, link.source
    )))
}

#[cfg(test)]
#[path = "flow_continuation_test.rs"]
mod tests;
