//! Queued build waits of `pump.db`.
//!
//! A need that cannot be built yet records why here: the open pump record it
//! belongs to and the prerequisites that have not landed, each with the state
//! that classified it. Like need priority and job release, a wait is operator
//! state beside `pump_control`, never part of
//! [`State`](crate::pump::state::State), so no state save touches it.

use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};

use vivi_error::VivariumError;

use super::{PumpStore, db_err};

const DDL: &str = "
CREATE TABLE IF NOT EXISTS build_waits (
    need TEXT PRIMARY KEY,
    record TEXT NOT NULL,
    prerequisites_json TEXT NOT NULL,
    observed_at INTEGER NOT NULL
);
";

pub(super) fn ensure_tables(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(DDL)
        .map_err(db_err("create build waits table"))
}

/// How a build prerequisite stood when the wait was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrerequisiteState {
    /// The prerequisite is ready, so it blocks nothing now.
    Open,
    /// The prerequisite is held, so the dependent waits for a release.
    Held,
    /// The prerequisite finished without landing, so nothing will release it.
    ClosedUnlanded,
}

/// One prerequisite that kept a need from building.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildPrerequisite {
    /// The prerequisite identity: the item handle.
    pub handle: String,
    /// The state that classified this prerequisite.
    pub state: PrerequisiteState,
    /// Why this state was read, when the classification carried a detail.
    pub detail: Option<String>,
    /// The outcome this prerequisite reported, when it reported one.
    pub outcome: Option<String>,
}

/// One queued build wait: a dependent need, the open pump record that queued it
/// and the prerequisites that had not landed when it was observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildWait {
    /// The dependent need that could not build.
    pub need: String,
    /// The handle of the open pump record for that need.
    pub record: String,
    /// The prerequisites that had not landed.
    pub prerequisites: Vec<BuildPrerequisite>,
    /// When the wait was observed, as a unix timestamp.
    pub observed_at: u64,
}

const COLUMNS: &str = "need, record, prerequisites_json, observed_at";

type RawRow = (String, String, String, u64);

fn raw_row(row: &Row<'_>) -> rusqlite::Result<RawRow> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn decode(
    (need, record, prerequisites_json, observed_at): RawRow,
) -> Result<BuildWait, VivariumError> {
    let prerequisites = serde_json::from_str(&prerequisites_json)
        .map_err(db_err("decode build wait prerequisites"))?;
    Ok(BuildWait {
        need,
        record,
        prerequisites,
        observed_at,
    })
}

/// True when `table` exists in this database. A read-only store that predates
/// the table reads as empty rather than creating it.
fn has_table(conn: &Connection, table: &str) -> Result<bool, VivariumError> {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
    .map_err(db_err("look for build waits table"))
}

impl PumpStore {
    /// Record `wait` as the wait of its need, replacing the need's earlier wait.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be encoded or written.
    pub fn upsert_build_wait(&self, wait: &BuildWait) -> Result<(), VivariumError> {
        let prerequisites = serde_json::to_string(&wait.prerequisites)
            .map_err(db_err("encode build wait prerequisites"))?;
        self.conn()
            .execute(
                "INSERT INTO build_waits (need, record, prerequisites_json, observed_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(need) DO UPDATE SET
                     record = ?2, prerequisites_json = ?3, observed_at = ?4",
                params![
                    wait.need,
                    wait.record,
                    prerequisites,
                    i64::try_from(wait.observed_at).unwrap_or(i64::MAX),
                ],
            )
            .map_err(db_err("write build wait"))?;
        Ok(())
    }

    /// Every queued build wait, in need-handle order so two reads agree.
    /// A database without the table has no waits.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the table cannot be read or a stored wait
    /// cannot be decoded.
    pub fn build_waits(&self) -> Result<Vec<BuildWait>, VivariumError> {
        let conn = self.conn();
        if !has_table(&conn, "build_waits")? {
            return Ok(Vec::new());
        }
        let mut stmt = conn
            .prepare(&format!("SELECT {COLUMNS} FROM build_waits ORDER BY need"))
            .map_err(db_err("prepare build waits"))?;
        let raw = stmt
            .query_map([], raw_row)
            .map_err(db_err("read build waits"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err("read build wait row"))?;
        raw.into_iter().map(decode).collect()
    }

    /// Drop the wait queued for `need`.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be removed.
    pub fn remove_build_wait(&self, need: &str) -> Result<(), VivariumError> {
        self.conn()
            .execute("DELETE FROM build_waits WHERE need = ?1", [need])
            .map_err(db_err("remove build wait"))?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "build_waits_test.rs"]
mod tests;
