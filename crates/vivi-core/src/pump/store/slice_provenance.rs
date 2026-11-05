//! Immutable dispatch provenance for hand jobs.

use rusqlite::{Connection, OptionalExtension, params};
use vivi_error::VivariumError;

use super::{PumpStore, db_err};

const DDL: &str = "CREATE TABLE IF NOT EXISTS slice_provenance (
 original_hand TEXT PRIMARY KEY, need TEXT NOT NULL, run INTEGER NOT NULL,
 task TEXT NOT NULL, target TEXT NOT NULL, dispatch_base TEXT NOT NULL,
 authorization_base TEXT NOT NULL, predecessor_hand TEXT, source_hand TEXT,
 handoff TEXT, candidate TEXT, job_id TEXT NOT NULL UNIQUE,
 preparation_error TEXT, verification_error TEXT NOT NULL DEFAULT ''
); CREATE INDEX IF NOT EXISTS slice_provenance_task_idx ON slice_provenance(task);
 CREATE INDEX IF NOT EXISTS slice_provenance_source_idx ON slice_provenance(source_hand);";

/// Immutable dispatch identity and mutable write-once completion evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SliceProvenance {
    pub original_hand: String,
    pub need: String,
    pub run: u32,
    pub task: String,
    pub target: String,
    pub dispatch_base: String,
    pub authorization_base: String,
    pub predecessor_hand: Option<String>,
    pub source_hand: Option<String>,
    pub handoff: Option<String>,
    pub candidate: Option<String>,
    pub job_id: String,
    pub preparation_error: Option<String>,
    pub verification_error: String,
}

pub(super) fn ensure_table(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(DDL)
        .map_err(db_err("create slice provenance table"))
}

impl PumpStore {
    /// Persist a dispatch, accepting exact replays only.
    pub fn record_slice_provenance(&self, row: &SliceProvenance) -> Result<(), VivariumError> {
        let conn = self.conn();
        conn.execute("INSERT OR IGNORE INTO slice_provenance
          (original_hand,need,run,task,target,dispatch_base,authorization_base,predecessor_hand,source_hand,handoff,candidate,job_id,preparation_error,verification_error)
          VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,NULL,?11,?12,?13)", params![row.original_hand,row.need,i64::from(row.run),row.task,row.target,row.dispatch_base,row.authorization_base,row.predecessor_hand,row.source_hand,row.handoff,row.job_id,row.preparation_error,row.verification_error]).map_err(db_err("write slice provenance"))?;
        let existing = get_by_hand(&conn, &row.original_hand)?;
        if existing
            .as_ref()
            .is_none_or(|saved| !identity_matches(saved, row))
        {
            return Err(VivariumError::Other(format!(
                "slice provenance identity mismatch for {}",
                row.original_hand
            )));
        }
        Ok(())
    }

    /// Complete a hand's candidate once; repeat completion must match.
    pub fn complete_slice_candidate(
        &self,
        hand: &str,
        candidate: &str,
    ) -> Result<(), VivariumError> {
        let conn = self.conn();
        conn.execute(
            "UPDATE slice_provenance SET candidate=COALESCE(candidate,?2) WHERE original_hand=?1",
            params![hand, candidate],
        )
        .map_err(db_err("complete slice candidate"))?;
        let saved: Option<String> = conn
            .query_row(
                "SELECT candidate FROM slice_provenance WHERE original_hand=?1",
                [hand],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err("read slice candidate"))?
            .flatten();
        if saved.as_deref() != Some(candidate) {
            return Err(VivariumError::Other(format!(
                "slice candidate mismatch or unknown hand {hand}"
            )));
        }
        Ok(())
    }

    /// Lookup dispatch provenance by original hand identity.
    pub fn slice_provenance(&self, hand: &str) -> Result<Option<SliceProvenance>, VivariumError> {
        get_by_hand(&self.conn(), hand)
    }

    /// Find provenance through task identity or predecessor/source identity.
    pub fn slice_provenance_related(
        &self,
        task: Option<&str>,
        source: Option<&str>,
    ) -> Result<Vec<SliceProvenance>, VivariumError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT original_hand FROM slice_provenance WHERE (?1 IS NOT NULL AND task=?1) OR (?2 IS NOT NULL AND (source_hand=?2 OR predecessor_hand=?2)) ORDER BY original_hand").map_err(db_err("prepare related provenance"))?;
        let hands = stmt
            .query_map(params![task, source], |r| r.get::<_, String>(0))
            .map_err(db_err("query related provenance"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err("read related provenance"))?;
        hands
            .iter()
            .map(|h| {
                get_by_hand(&conn, h)?
                    .ok_or_else(|| VivariumError::Other(format!("missing provenance row {h}")))
            })
            .collect()
    }

    /// Reuse a preparation keyed by stable job identity before enqueue.
    pub fn slice_preparation(
        &self,
        job_id: &str,
    ) -> Result<Option<SliceProvenance>, VivariumError> {
        let conn = self.conn();
        let hand: Option<String> = conn
            .query_row(
                "SELECT original_hand FROM slice_provenance WHERE job_id=?1",
                [job_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err("read preparation identity"))?;
        match hand {
            Some(h) => get_by_hand(&conn, &h),
            None => Ok(None),
        }
    }
}

fn identity_matches(a: &SliceProvenance, b: &SliceProvenance) -> bool {
    a.original_hand == b.original_hand
        && a.need == b.need
        && a.run == b.run
        && a.task == b.task
        && a.target == b.target
        && a.dispatch_base == b.dispatch_base
        && a.authorization_base == b.authorization_base
        && a.predecessor_hand == b.predecessor_hand
        && a.source_hand == b.source_hand
        && a.handoff == b.handoff
        && a.job_id == b.job_id
        && a.preparation_error == b.preparation_error
        && a.verification_error == b.verification_error
}

fn get_by_hand(conn: &Connection, hand: &str) -> Result<Option<SliceProvenance>, VivariumError> {
    conn.query_row("SELECT original_hand,need,run,task,target,dispatch_base,authorization_base,predecessor_hand,source_hand,handoff,candidate,job_id,preparation_error,verification_error FROM slice_provenance WHERE original_hand=?1",[hand],|r| Ok(SliceProvenance { original_hand:r.get(0)?, need:r.get(1)?, run:u32::try_from(r.get::<_,i64>(2)?).unwrap_or(0), task:r.get(3)?, target:r.get(4)?, dispatch_base:r.get(5)?, authorization_base:r.get(6)?, predecessor_hand:r.get(7)?, source_hand:r.get(8)?, handoff:r.get(9)?, candidate:r.get(10)?, job_id:r.get(11)?, preparation_error:r.get(12)?, verification_error:r.get(13)? })).optional().map_err(db_err("read slice provenance"))
}

#[cfg(test)]
#[path = "slice_provenance_test.rs"]
mod tests;
