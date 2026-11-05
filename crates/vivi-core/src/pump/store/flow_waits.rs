//! Persisted, read-only projection of prerequisite waits.
use super::{PumpStore, db_err};
use rusqlite::{Connection, OptionalExtension, params};
use vivi_error::VivariumError;

pub(super) fn ensure_tables(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS flow_waits (need TEXT PRIMARY KEY, record TEXT NOT NULL, handles_json TEXT NOT NULL, states_json TEXT NOT NULL, details_json TEXT NOT NULL, evaluated_at INTEGER NOT NULL);")
        .map_err(db_err("create flow waits"))
}

/// One prerequisite handle's classified presentation state.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WaitingHandle {
    pub handle: String,
    pub state: String,
    pub detail: String,
}
/// Persisted prerequisite wait projection for a need and open pump record.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FlowWaitRow {
    pub need: String,
    pub record: String,
    pub handles: Vec<WaitingHandle>,
    pub evaluated_at: u64,
}

fn table_exists(conn: &Connection) -> Result<bool, VivariumError> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='flow_waits'",
        [],
        |_| Ok(()),
    )
    .optional()
    .map(|v| v.is_some())
    .map_err(db_err("check flow waits table"))
}
impl PumpStore {
    /// Insert or replace a need's wait projection.
    pub fn upsert_flow_wait(&self, row: &FlowWaitRow) -> Result<(), VivariumError> {
        let handles: Vec<_> = row.handles.iter().map(|h| &h.handle).collect();
        let states: Vec<_> = row.handles.iter().map(|h| &h.state).collect();
        let details: Vec<_> = row.handles.iter().map(|h| &h.detail).collect();
        let conn = self.conn();
        conn.execute("INSERT INTO flow_waits VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(need) DO UPDATE SET record=?2, handles_json=?3, states_json=?4, details_json=?5, evaluated_at=?6", params![row.need,row.record,json(&handles)?,json(&states)?,json(&details)?,row.evaluated_at]).map_err(db_err("write flow wait"))?;
        Ok(())
    }
    /// Remove the current projection for a need.
    pub fn remove_flow_wait(&self, need: &str) -> Result<(), VivariumError> {
        self.conn()
            .execute("DELETE FROM flow_waits WHERE need=?1", [need])
            .map_err(db_err("remove flow wait"))?;
        Ok(())
    }
    /// Remove rows not represented by the supplied open `(need, record)` identities.
    pub fn reconcile_flow_waits(&self, open: &[(String, String)]) -> Result<(), VivariumError> {
        let conn = self.conn();
        let rows = read_flow_waits(&conn)?;
        for row in rows {
            if !open
                .iter()
                .any(|(need, record)| need == &row.need && record == &row.record)
            {
                self.remove_flow_wait(&row.need)?;
            }
        }
        Ok(())
    }
    /// All waits, ordered by need; on legacy databases without the additive table, empty.
    pub fn flow_waits(&self) -> Result<Vec<FlowWaitRow>, VivariumError> {
        read_flow_waits(&self.conn())
    }
}
fn read_flow_waits(conn: &Connection) -> Result<Vec<FlowWaitRow>, VivariumError> {
    if !table_exists(conn)? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare("SELECT need,record,handles_json,states_json,details_json,evaluated_at FROM flow_waits ORDER BY need").map_err(db_err("read flow waits"))?;
    let raw = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })
        .map_err(db_err("read flow waits"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read flow waits"))?;
    raw.into_iter()
        .map(|(need, record, h, s, d, at)| {
            let handles: Vec<String> = from_json(h);
            let states: Vec<String> = from_json(s);
            let details: Vec<String> = from_json(d);
            if handles.len() != states.len() || handles.len() != details.len() {
                return Err(VivariumError::Parse("mismatched flow wait fields".into()));
            }
            Ok(FlowWaitRow {
                need,
                record,
                handles: handles
                    .into_iter()
                    .zip(states)
                    .zip(details)
                    .map(|((handle, state), detail)| WaitingHandle {
                        handle,
                        state,
                        detail,
                    })
                    .collect(),
                evaluated_at: from_u64(at),
            })
        })
        .collect()
}
use super::{from_json, from_u64, json};
#[cfg(test)]
#[path = "flow_waits_test.rs"]
mod tests;
