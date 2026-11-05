//! Flow link tables of `pump.db`: the fanout step open in a run, the
//! fanout and dependencies of each slice step task, and the entry a restart
//! of a run starts the next run at.

use rusqlite::{Connection, OptionalExtension, params};

use crate::pump::flow::index::RunRow;
use vivi_error::VivariumError;

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
CREATE TABLE IF NOT EXISTS fanout_starts (
    fanout TEXT PRIMARY KEY,
    tip TEXT
);
";

/// Create the flow link tables when they are missing.
pub(super) fn ensure_tables(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(LINKS_DDL)
        .map_err(db_err("create flow link tables"))?;
    // Additive checks are required even when user_version already says v8.
    let mut stmt = conn
        .prepare("PRAGMA table_info(restart_entries)")
        .map_err(db_err("inspect restart entries"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(db_err("inspect restart entries"))?
        .collect::<Result<std::collections::BTreeSet<_>, _>>()
        .map_err(db_err("inspect restart entries"))?;
    drop(stmt);
    for (name, declaration) in [
        ("origin_record", "TEXT"),
        ("action_id", "TEXT"),
        ("unlock_task", "TEXT"),
    ] {
        if !columns.contains(name) {
            conn.execute_batch(&format!(
                "ALTER TABLE restart_entries ADD COLUMN {name} {declaration}"
            ))
            .map_err(db_err("extend restart entries"))?;
        }
    }
    Ok(())
}

/// Provenance attached to a durable restart request. Optional fields preserve
/// autonomous requests and compatibility with legacy entry-only rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartMetadata {
    pub need: String,
    pub run: u32,
    pub entry: String,
    pub origin_record: Option<String>,
    pub action_id: Option<String>,
    pub unlock_task: Option<String>,
}

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

impl SliceLink {
    /// A link for a slice task of `run`: need and run come from the run row.
    pub fn for_run(
        run: &RunRow,
        task: &str,
        fanout: Option<String>,
        deps: Vec<String>,
    ) -> SliceLink {
        SliceLink {
            task: task.to_string(),
            need: run.need.clone(),
            run: run.run,
            fanout,
            deps,
        }
    }
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

fn restart_replay(
    tx: &rusqlite::Transaction<'_>,
    request: &RestartMetadata,
) -> Result<bool, VivariumError> {
    let Some(action) = &request.action_id else {
        return Ok(false);
    };
    let receipt: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM applied_actions WHERE letter_id=?1)",
            [action],
            |r| r.get(0),
        )
        .map_err(db_err("check restart receipt"))?;
    if !receipt {
        return Ok(false);
    }
    let old = restart_row(tx, request)?;
    let same = old.is_some_and(|v| {
        v.0 == request.entry
            && v.1 == request.origin_record
            && v.2.as_ref() == Some(action)
            && v.3 == request.unlock_task
    });
    if same {
        Ok(true)
    } else {
        Err(VivariumError::Other(
            "restart action already applied to a different request".into(),
        ))
    }
}

fn restart_row(
    tx: &rusqlite::Transaction<'_>,
    request: &RestartMetadata,
) -> Result<Option<(String, Option<String>, Option<String>, Option<String>)>, VivariumError> {
    let old = tx.query_row("SELECT entry, origin_record, action_id, unlock_task FROM restart_entries WHERE need=?1 AND run=?2", params![request.need, request.run], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional().map_err(db_err("read restart request"))?;
    let predecessor: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM flow_runs WHERE need=?1 AND run=?2)",
            params![request.need, request.run],
            |r| r.get(0),
        )
        .map_err(db_err("check restart predecessor"))?;
    if !predecessor {
        return Err(VivariumError::Other(
            "restart predecessor does not exist".into(),
        ));
    }
    Ok(old)
}

impl PumpStore {
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn set_open_fanout(&self, need: &str, run: u32, fanout: &str) -> Result<(), VivariumError> {
        self.conn()
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
        self.conn()
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
        self.conn()
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
        let conn = self.conn();
        let raw = conn
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
        let conn = self.conn();
        let mut stmt = conn
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
        let conn = self.conn();
        let changed = conn
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

    /// Load an entry and its optional provenance.
    pub fn restart_metadata(
        &self,
        need: &str,
        run: u32,
    ) -> Result<Option<RestartMetadata>, VivariumError> {
        self.conn().query_row(
            "SELECT entry, origin_record, action_id, unlock_task FROM restart_entries WHERE need=?1 AND run=?2",
            params![need, run], |r| Ok(RestartMetadata { need: need.to_owned(), run, entry: r.get(0)?, origin_record: r.get(1)?, action_id: r.get(2)?, unlock_task: r.get(3)? })
        ).optional().map_err(db_err("read restart metadata"))
    }

    /// Atomically persist restart intent, predecessor state, and optional action receipt.
    /// Replaying the identical action/request is a no-op; conflicting replays fail.
    pub fn request_restart(
        &self,
        request: &RestartMetadata,
        at: u64,
    ) -> Result<bool, VivariumError> {
        if request.need.is_empty()
            || request.entry.is_empty()
            || request.run == 0
            || request.action_id.as_deref() == Some("")
        {
            return Err(VivariumError::Other("inconsistent restart request".into()));
        }
        let conn = self.conn();
        let tx = conn
            .unchecked_transaction()
            .map_err(db_err("begin restart request"))?;
        if restart_replay(&tx, request)? {
            tx.commit().map_err(db_err("commit restart replay"))?;
            return Ok(false);
        }
        let old = restart_row(&tx, request)?;
        if let Some(v) = old {
            if v.0 != request.entry
                || v.1 != request.origin_record
                || v.2 != request.action_id
                || v.3 != request.unlock_task
            {
                return Err(VivariumError::Other("conflicting restart request".into()));
            }
        } else {
            tx.execute("INSERT INTO restart_entries (need,run,entry,origin_record,action_id,unlock_task) VALUES (?1,?2,?3,?4,?5,?6)", params![request.need, request.run, request.entry, request.origin_record, request.action_id, request.unlock_task]).map_err(db_err("write restart request"))?;
        }
        let changed = tx.execute("UPDATE flow_runs SET state='restarting', state_at=CAST(strftime('%s','now') AS INTEGER) WHERE need=?1 AND run=?2", params![request.need, request.run]).map_err(db_err("mark restart predecessor"))?;
        if changed == 0 {
            return Err(VivariumError::Other(
                "restart predecessor does not exist".into(),
            ));
        }
        if let Some(action) = &request.action_id {
            tx.execute(
                "INSERT INTO applied_actions (letter_id,at) VALUES (?1,?2)",
                params![action, i64::try_from(at).unwrap_or(i64::MAX)],
            )
            .map_err(db_err("record restart action"))?;
        }
        tx.commit().map_err(db_err("commit restart request"))?;
        Ok(true)
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
        self.conn()
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
        self.conn()
            .query_row(
                "SELECT entry FROM restart_entries WHERE need = ?1 AND run = ?2",
                params![need, run],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err("read restart entry"))
    }

    /// Record the need branch tip when a fanout started (`None`: no base yet).
    /// The first start of a fanout wins; a later call changes nothing.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn set_fanout_start(&self, fanout: &str, tip: Option<&str>) -> Result<(), VivariumError> {
        self.conn()
            .execute(
                "INSERT OR IGNORE INTO fanout_starts (fanout, tip) VALUES (?1, ?2)",
                params![fanout, tip],
            )
            .map_err(db_err("write fanout start"))?;
        Ok(())
    }

    /// The tip recorded when a fanout started: `None` when no start is
    /// recorded, `Some(None)` when it started with no need branch base.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn fanout_start(&self, fanout: &str) -> Result<Option<Option<String>>, VivariumError> {
        self.conn()
            .query_row(
                "SELECT tip FROM fanout_starts WHERE fanout = ?1",
                [fanout],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err("read fanout start"))
    }
}

#[cfg(test)]
#[path = "flow_links_test.rs"]
mod tests;
