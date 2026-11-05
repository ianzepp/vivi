//! Immutable provenance for hand dispatches and their accepted inputs.
use super::{PumpStore, db_err};
use rusqlite::{Connection, OptionalExtension, params};
use vivi_error::VivariumError;

const DDL: &str = "CREATE TABLE IF NOT EXISTS slice_provenance (
 hand TEXT PRIMARY KEY, need TEXT NOT NULL, run INTEGER NOT NULL, task TEXT NOT NULL UNIQUE,
 target TEXT NOT NULL, dispatch_base TEXT NOT NULL, authorization_base TEXT NOT NULL,
 predecessor_hand TEXT, source_hand TEXT, handoff TEXT, candidate TEXT,
 verification_failure TEXT, preparation_failure TEXT NOT NULL DEFAULT '',
 job TEXT UNIQUE
);
CREATE INDEX IF NOT EXISTS slice_provenance_source ON slice_provenance(source_hand);
CREATE INDEX IF NOT EXISTS slice_provenance_task ON slice_provenance(task);";

/// Immutable dispatch identity and its write-once completion receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SliceProvenance {
    pub hand: String,
    pub need: String,
    pub run: u64,
    pub task: String,
    pub target: String,
    pub dispatch_base: String,
    pub authorization_base: String,
    pub predecessor_hand: Option<String>,
    pub source_hand: Option<String>,
    pub handoff: Option<String>,
    pub candidate: Option<String>,
    pub verification_failure: Option<String>,
    pub preparation_failure: String,
}

pub(super) fn ensure_table(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(DDL)
        .map_err(db_err("create slice provenance tables"))
}

/// Transaction-friendly immutable insert/replay verifier.
pub fn record(conn: &Connection, row: &SliceProvenance) -> Result<(), VivariumError> {
    let run = i64::try_from(row.run).map_err(db_err("encode slice provenance run"))?;
    conn.execute("INSERT OR IGNORE INTO slice_provenance
      (hand,need,run,task,target,dispatch_base,authorization_base,predecessor_hand,source_hand,handoff,candidate,verification_failure,preparation_failure)
      VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)", params![row.hand,row.need,run,row.task,row.target,row.dispatch_base,row.authorization_base,row.predecessor_hand,row.source_hand,row.handoff,row.candidate,row.verification_failure,row.preparation_failure]).map_err(db_err("record slice provenance"))?;
    let existing = load_by_hand(conn, &row.hand)?;
    let Some(existing) = existing else {
        return Err(conflict("hand identity"));
    };
    if identity(&existing) != identity(row)
        || row
            .candidate
            .as_ref()
            .is_some_and(|candidate| existing.candidate.as_ref() != Some(candidate))
        || existing.verification_failure != row.verification_failure
        || existing.preparation_failure != row.preparation_failure
    {
        return Err(conflict("immutable fields"));
    }
    Ok(())
}

fn identity(
    r: &SliceProvenance,
) -> (
    &str,
    &str,
    u64,
    &str,
    &str,
    &str,
    &str,
    Option<&str>,
    Option<&str>,
    Option<&str>,
) {
    (
        &r.hand,
        &r.need,
        r.run,
        &r.task,
        &r.target,
        &r.dispatch_base,
        &r.authorization_base,
        r.predecessor_hand.as_deref(),
        r.source_hand.as_deref(),
        r.handoff.as_deref(),
    )
}
fn conflict(what: &str) -> VivariumError {
    VivariumError::Other(format!("pump.db slice provenance {what} mismatch"))
}

fn load_by_hand(conn: &Connection, hand: &str) -> Result<Option<SliceProvenance>, VivariumError> {
    conn.query_row("SELECT hand,need,run,task,target,dispatch_base,authorization_base,predecessor_hand,source_hand,handoff,candidate,verification_failure,preparation_failure FROM slice_provenance WHERE hand=?1", [hand], |r| Ok(SliceProvenance {hand:r.get(0)?,need:r.get(1)?,run:u64::try_from(r.get::<_,i64>(2)?).unwrap_or(0),task:r.get(3)?,target:r.get(4)?,dispatch_base:r.get(5)?,authorization_base:r.get(6)?,predecessor_hand:r.get(7)?,source_hand:r.get(8)?,handoff:r.get(9)?,candidate:r.get(10)?,verification_failure:r.get(11)?,preparation_failure:r.get(12)?})).optional().map_err(db_err("read slice provenance"))
}

impl PumpStore {
    /// Record a dispatch, accepting identical replay but refusing identity changes.
    pub fn record_slice_provenance(&self, row: &SliceProvenance) -> Result<(), VivariumError> {
        record(&self.conn(), row)
    }
    /// Lookup the original provenance for a hand, task, or source hand.
    pub fn slice_provenance_by_hand(
        &self,
        hand: &str,
    ) -> Result<Option<SliceProvenance>, VivariumError> {
        load_by_hand(&self.conn(), hand)
    }
    pub fn slice_provenance_by_task(
        &self,
        task: &str,
    ) -> Result<Option<SliceProvenance>, VivariumError> {
        let hand: Option<String> = self
            .conn()
            .query_row(
                "SELECT hand FROM slice_provenance WHERE task=?1",
                [task],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err("find slice provenance task"))?;
        hand.map(|h| self.slice_provenance_by_hand(&h))
            .transpose()
            .map(Option::flatten)
    }
    pub fn slice_provenance_by_source(
        &self,
        source: &str,
    ) -> Result<Vec<SliceProvenance>, VivariumError> {
        let conn = self.conn();
        let mut s = conn
            .prepare("SELECT hand FROM slice_provenance WHERE source_hand=?1 ORDER BY run,task")
            .map_err(db_err("prepare source provenance"))?;
        let hands = s
            .query_map([source], |r| r.get::<_, String>(0))
            .map_err(db_err("read source provenance"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err("read source provenance"))?;
        hands
            .iter()
            .map(|h| load_by_hand(&conn, h)?.ok_or_else(|| conflict("missing row")))
            .collect()
    }
    /// Store the completed candidate once; repeated completion must match exactly.
    pub fn complete_slice_candidate(
        &self,
        hand: &str,
        candidate: &str,
    ) -> Result<(), VivariumError> {
        let conn = self.conn();
        let changed = conn
            .execute(
                "UPDATE slice_provenance SET candidate=?2 WHERE hand=?1 AND candidate IS NULL",
                params![hand, candidate],
            )
            .map_err(db_err("complete slice candidate"))?;
        if changed == 0 {
            let old: Option<String> = conn
                .query_row(
                    "SELECT candidate FROM slice_provenance WHERE hand=?1",
                    [hand],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db_err("verify slice candidate"))?;
            if old.as_deref() != Some(candidate) {
                return Err(conflict("candidate"));
            }
        }
        Ok(())
    }
    /// Associate a stable job identity with a hand before enqueue; replay returns its original hand.
    pub fn prepare_slice_job(&self, job: &str, hand: &str) -> Result<String, VivariumError> {
        let conn = self.conn();
        conn.execute(
            "UPDATE slice_provenance SET job=?1 WHERE hand=?2 AND job IS NULL",
            params![job, hand],
        )
        .map_err(db_err("prepare slice job"))?;
        let existing: Option<String> = conn
            .query_row(
                "SELECT hand FROM slice_provenance WHERE job=?1",
                [job],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err("read prepared slice job"))?;
        if existing.as_deref() != Some(hand) {
            return Err(conflict("job identity"));
        }
        Ok(hand.to_string())
    }
}

#[cfg(test)]
#[path = "slice_provenance_test.rs"]
mod tests;
