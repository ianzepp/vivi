//! Durable decisions and replayable publication work for completed slice hands.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use vivi_error::VivariumError;

use super::{PumpStore, db_err};

const DDL: &str = "CREATE TABLE IF NOT EXISTS slice_scope_results (
 original_hand TEXT PRIMARY KEY, accepted_job TEXT NOT NULL, need TEXT NOT NULL,
 run INTEGER NOT NULL, task TEXT NOT NULL, target TEXT NOT NULL, source_branch TEXT NOT NULL,
 dispatch_base TEXT NOT NULL, authorization_base TEXT NOT NULL, candidate TEXT NOT NULL,
 original_scope_json TEXT NOT NULL, changed_paths_json TEXT NOT NULL, additions_json TEXT NOT NULL,
 granted_paths_json TEXT NOT NULL, held_paths_json TEXT NOT NULL, owner_tasks_json TEXT NOT NULL,
 disposition TEXT NOT NULL, verification_reason TEXT NOT NULL, dependency_conflict_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS slice_publication (
 original_hand TEXT NOT NULL, item TEXT NOT NULL, progress TEXT NOT NULL DEFAULT 'pending',
 payload_json TEXT NOT NULL DEFAULT 'null', PRIMARY KEY(original_hand,item),
 FOREIGN KEY(original_hand) REFERENCES slice_scope_results(original_hand)
);";

/// A complete scope decision. JSON-valued fields retain the exact evidence snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SliceScopeResult {
    pub original_hand: String,
    pub accepted_job: String,
    pub need: String,
    pub run: u32,
    pub task: String,
    pub target: String,
    pub source_branch: String,
    pub dispatch_base: String,
    pub authorization_base: String,
    pub candidate: String,
    pub original_scope: Vec<String>,
    pub changed_paths: Vec<String>,
    pub additions: Vec<String>,
    pub granted_paths: Vec<String>,
    pub held_paths: Vec<String>,
    pub owner_tasks: Vec<String>,
    pub disposition: String,
    pub verification_reason: String,
    pub dependency_conflict: Vec<String>,
}

/// Replay state for one external publication operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationProgress {
    pub item: String,
    pub progress: String,
    pub payload_json: String,
}

pub(super) fn ensure_table(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(DDL)
        .map_err(db_err("create slice scope result tables"))
}

const DISPOSITIONS: &[&str] = &[
    "approved",
    "held",
    "unverified",
    "handoff-pending",
    "handed-off",
    "released",
    "refused",
];

impl PumpStore {
    /// Store the immutable evidence and its decision. Exact retries are accepted; changed retries fail.
    pub fn record_slice_scope_result(&self, row: &SliceScopeResult) -> Result<(), VivariumError> {
        if !DISPOSITIONS.contains(&row.disposition.as_str()) {
            return Err(VivariumError::Parse(format!(
                "unknown scope disposition {}",
                row.disposition
            )));
        }
        let conn = self.conn();
        let encode =
            |v: &Vec<String>| serde_json::to_string(v).map_err(db_err("encode scope result"));
        conn.execute("INSERT OR IGNORE INTO slice_scope_results VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)", params![row.original_hand,row.accepted_job,row.need,row.run,row.task,row.target,row.source_branch,row.dispatch_base,row.authorization_base,row.candidate,encode(&row.original_scope)?,encode(&row.changed_paths)?,encode(&row.additions)?,encode(&row.granted_paths)?,encode(&row.held_paths)?,encode(&row.owner_tasks)?,row.disposition,row.verification_reason,encode(&row.dependency_conflict)?]).map_err(db_err("write scope result"))?;
        let saved = self.slice_scope_result(&row.original_hand)?;
        if saved.as_ref() != Some(row) {
            return Err(VivariumError::Other(format!(
                "scope result identity mismatch for {}",
                row.original_hand
            )));
        }
        Ok(())
    }

    /// Read the durable decision without recalculating its original evidence.
    pub fn slice_scope_result(
        &self,
        hand: &str,
    ) -> Result<Option<SliceScopeResult>, VivariumError> {
        let c = self.conn();
        let raw = c.query_row("SELECT original_hand,accepted_job,need,run,task,target,source_branch,dispatch_base,authorization_base,candidate,original_scope_json,changed_paths_json,additions_json,granted_paths_json,held_paths_json,owner_tasks_json,disposition,verification_reason,dependency_conflict_json FROM slice_scope_results WHERE original_hand=?1", [hand], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,u32>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,String>(8)?,r.get::<_,String>(9)?,r.get::<_,String>(10)?,r.get::<_,String>(11)?,r.get::<_,String>(12)?,r.get::<_,String>(13)?,r.get::<_,String>(14)?,r.get::<_,String>(15)?,r.get::<_,String>(16)?,r.get::<_,String>(17)?,r.get::<_,String>(18)?))).optional().map_err(db_err("read scope result"))?;
        raw.map(|r| {
            Ok(SliceScopeResult {
                original_hand: r.0,
                accepted_job: r.1,
                need: r.2,
                run: r.3,
                task: r.4,
                target: r.5,
                source_branch: r.6,
                dispatch_base: r.7,
                authorization_base: r.8,
                candidate: r.9,
                original_scope: serde_json::from_str(&r.10)
                    .map_err(db_err("decode original scope"))?,
                changed_paths: serde_json::from_str(&r.11)
                    .map_err(db_err("decode changed paths"))?,
                additions: serde_json::from_str(&r.12).map_err(db_err("decode additions"))?,
                granted_paths: serde_json::from_str(&r.13).map_err(db_err("decode grants"))?,
                held_paths: serde_json::from_str(&r.14).map_err(db_err("decode held paths"))?,
                owner_tasks: serde_json::from_str(&r.15).map_err(db_err("decode owners"))?,
                disposition: r.16,
                verification_reason: r.17,
                dependency_conflict: serde_json::from_str(&r.18)
                    .map_err(db_err("decode dependency conflict"))?,
            })
        })
        .transpose()
    }

    /// Atomically persist a decision plus replayable publication intents. External effects are performed by callers after commit.
    pub fn record_scope_decision_with_publication(
        &self,
        row: &SliceScopeResult,
        items: &[(&str, &str)],
    ) -> Result<(), VivariumError> {
        self.record_slice_scope_result(row)?;
        let c = self.conn();
        let tx = c
            .unchecked_transaction()
            .map_err(db_err("begin publication intents"))?;
        for (item, payload) in items {
            tx.execute("INSERT OR IGNORE INTO slice_publication(original_hand,item,payload_json) VALUES (?1,?2,?3)", params![row.original_hand,item,payload]).map_err(db_err("write publication intent"))?;
        }
        tx.commit().map_err(db_err("commit publication intents"))?;
        Ok(())
    }

    /// Update one publication stage after its external operation succeeds; retries are idempotent.
    pub fn mark_slice_publication(
        &self,
        hand: &str,
        item: &str,
        progress: &str,
    ) -> Result<(), VivariumError> {
        let changed = self
            .conn()
            .execute(
                "UPDATE slice_publication SET progress=?3 WHERE original_hand=?1 AND item=?2",
                params![hand, item, progress],
            )
            .map_err(db_err("update publication progress"))?;
        if changed == 0 {
            return Err(VivariumError::Parse(format!(
                "no publication intent {item} for {hand}"
            )));
        }
        Ok(())
    }

    /// Read publication work in stable item order.
    pub fn slice_publication(&self, hand: &str) -> Result<Vec<PublicationProgress>, VivariumError> {
        let c = self.conn();
        let mut s=c.prepare("SELECT item,progress,payload_json FROM slice_publication WHERE original_hand=?1 ORDER BY item").map_err(db_err("read publication"))?;
        s.query_map([hand], |r| {
            Ok(PublicationProgress {
                item: r.get(0)?,
                progress: r.get(1)?,
                payload_json: r.get(2)?,
            })
        })
        .map_err(db_err("read publication"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read publication"))
    }
}

#[path = "slice_scope_result_test.rs"]
mod tests;
