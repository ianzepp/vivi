//! Durable scope decisions and replayable publication progress.
use super::{PumpStore, db_err};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use vivi_error::VivariumError;

const DDL: &str = "CREATE TABLE IF NOT EXISTS slice_scope_results (
 hand TEXT PRIMARY KEY, reply_job TEXT NOT NULL, need TEXT NOT NULL, run INTEGER NOT NULL,
 task TEXT NOT NULL, target TEXT NOT NULL, source_branch TEXT NOT NULL,
 dispatch_base TEXT, authorization_base TEXT, candidate_json TEXT NOT NULL,
 original_scope_json TEXT NOT NULL, changed_paths_json TEXT NOT NULL, additions_json TEXT NOT NULL,
 granted_json TEXT NOT NULL, held_json TEXT NOT NULL, owners_json TEXT NOT NULL,
 disposition TEXT NOT NULL, verification_reasons_json TEXT NOT NULL, conflict_json TEXT);
 CREATE TABLE IF NOT EXISTS slice_publication (
 hand TEXT NOT NULL, stage TEXT NOT NULL, payload_json TEXT NOT NULL, done INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY(hand, stage));";

pub(super) fn ensure_table(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(DDL)
        .map_err(db_err("create scope result tables"))
}

/// Durable evidence and disposition for one original hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SliceScopeResult {
    pub hand: String,
    pub reply_job: String,
    pub need: String,
    pub run: u32,
    pub task: String,
    pub target: String,
    pub source_branch: String,
    pub dispatch_base: Option<String>,
    pub authorization_base: Option<String>,
    pub candidate: serde_json::Value,
    pub original_scope: Vec<String>,
    pub changed_paths: Vec<String>,
    pub additions: Vec<String>,
    pub granted: Vec<String>,
    pub held: Vec<String>,
    pub owners: Vec<String>,
    pub disposition: String,
    pub verification_reasons: Vec<String>,
    pub conflict: Option<serde_json::Value>,
}

/// Changes committed alongside the result, before external publication.
#[derive(Debug, Clone)]
pub struct ScopeTransition {
    pub result: SliceScopeResult,
    pub deps: Vec<String>,
    pub scope_state: String,
    pub holders: Vec<String>,
    pub step_state: String,
    /// Publication work is keyed by stage and payload; duplicate stages are rejected on mismatch.
    pub publication: Vec<(String, serde_json::Value)>,
}

impl PumpStore {
    /// Atomically persist immutable evidence, grants, dependency links, metadata, step state and intents.
    pub fn apply_scope_transition(&self, change: &ScopeTransition) -> Result<(), VivariumError> {
        let r = &change.result;
        let conn = self.conn();
        let tx = conn
            .unchecked_transaction()
            .map_err(db_err("begin scope transition"))?;
        let values = [
            r.candidate.to_string(),
            serde_json::to_string(&r.original_scope).map_err(db_err("encode scope result"))?,
            serde_json::to_string(&r.changed_paths).map_err(db_err("encode scope result"))?,
            serde_json::to_string(&r.additions).map_err(db_err("encode scope result"))?,
            serde_json::to_string(&r.granted).map_err(db_err("encode scope result"))?,
            serde_json::to_string(&r.held).map_err(db_err("encode scope result"))?,
            serde_json::to_string(&r.owners).map_err(db_err("encode scope result"))?,
            serde_json::to_string(&r.verification_reasons)
                .map_err(db_err("encode scope result"))?,
        ];
        let conflict = r.conflict.as_ref().map(ToString::to_string);
        let existing: Option<String> = tx
            .query_row(
                "SELECT candidate_json FROM slice_scope_results WHERE hand=?1",
                [&r.hand],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err("read scope evidence"))?;
        if existing.is_some() {
            let stored: SliceScopeResult = read_result(&tx, &r.hand)?;
            if stored != *r {
                return Err(VivariumError::Other(format!(
                    "scope evidence for hand {} is immutable",
                    r.hand
                )));
            }
        } else {
            tx.execute("INSERT INTO slice_scope_results VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)", params![r.hand,r.reply_job,r.need,r.run,r.task,r.target,r.source_branch,r.dispatch_base,r.authorization_base,values[0],values[1],values[2],values[3],values[4],values[5],values[6],r.disposition,values[7],conflict]).map_err(db_err("write scope result"))?;
        }
        let scope: Vec<String> = serde_json::from_str(
            &tx.query_row(
                "SELECT write_scope_json FROM slice_scope WHERE task=?1",
                [&r.task],
                |row| row.get::<_, String>(0),
            )
            .map_err(db_err("read transition scope"))?,
        )
        .map_err(db_err("decode transition scope"))?;
        let mut scope: Vec<String> = scope;
        for p in &r.granted {
            if !scope.contains(p) {
                scope.push(p.clone());
            }
        }
        tx.execute(
            "UPDATE slice_scope SET write_scope_json=?2, scope_state=?3, holders=?4 WHERE task=?1",
            params![
                r.task,
                serde_json::to_string(&scope).map_err(db_err("encode scope"))?,
                change.scope_state,
                serde_json::to_string(&change.holders).map_err(db_err("encode holders"))?
            ],
        )
        .map_err(db_err("update scope metadata"))?;
        let mut deps: Vec<String> = serde_json::from_str(
            &tx.query_row(
                "SELECT deps_json FROM slice_links WHERE task=?1",
                [&r.task],
                |row| row.get::<_, String>(0),
            )
            .map_err(db_err("read transition deps"))?,
        )
        .map_err(db_err("decode transition deps"))?;
        for d in &change.deps {
            if !deps.contains(d) {
                deps.push(d.clone());
            }
        }
        tx.execute(
            "UPDATE slice_links SET deps_json=?2 WHERE task=?1",
            params![
                r.task,
                serde_json::to_string(&deps).map_err(db_err("encode deps"))?
            ],
        )
        .map_err(db_err("update transition deps"))?;
        tx.execute(
            "UPDATE flow_steps SET state=?2 WHERE task=?1",
            params![r.task, change.step_state],
        )
        .map_err(db_err("update transition step"))?;
        for (stage, payload) in &change.publication {
            tx.execute("INSERT INTO slice_publication(hand,stage,payload_json) VALUES(?1,?2,?3) ON CONFLICT(hand,stage) DO NOTHING", params![r.hand,stage,payload.to_string()]).map_err(db_err("write publication intent"))?;
        }
        tx.commit().map_err(db_err("commit scope transition"))?;
        Ok(())
    }
}

fn read_result(conn: &Connection, hand: &str) -> Result<SliceScopeResult, VivariumError> {
    conn.query_row("SELECT hand,reply_job,need,run,task,target,source_branch,dispatch_base,authorization_base,candidate_json,original_scope_json,changed_paths_json,additions_json,granted_json,held_json,owners_json,disposition,verification_reasons_json,conflict_json FROM slice_scope_results WHERE hand=?1", [hand], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,u32>(3)?,row.get::<_,String>(4)?,row.get::<_,String>(5)?,row.get::<_,String>(6)?,row.get::<_,Option<String>>(7)?,row.get::<_,Option<String>>(8)?,row.get::<_,String>(9)?,row.get::<_,String>(10)?,row.get::<_,String>(11)?,row.get::<_,String>(12)?,row.get::<_,String>(13)?,row.get::<_,String>(14)?,row.get::<_,String>(15)?,row.get::<_,String>(16)?,row.get::<_,String>(17)?,row.get::<_,Option<String>>(18)?))).map_err(db_err("read scope result")).and_then(|x| Ok(SliceScopeResult{hand:x.0,reply_job:x.1,need:x.2,run:x.3,task:x.4,target:x.5,source_branch:x.6,dispatch_base:x.7,authorization_base:x.8,candidate:serde_json::from_str(&x.9).map_err(db_err("decode candidate"))?,original_scope:serde_json::from_str(&x.10).map_err(db_err("decode scope"))?,changed_paths:serde_json::from_str(&x.11).map_err(db_err("decode paths"))?,additions:serde_json::from_str(&x.12).map_err(db_err("decode additions"))?,granted:serde_json::from_str(&x.13).map_err(db_err("decode grants"))?,held:serde_json::from_str(&x.14).map_err(db_err("decode held"))?,owners:serde_json::from_str(&x.15).map_err(db_err("decode owners"))?,disposition:x.16,verification_reasons:serde_json::from_str(&x.17).map_err(db_err("decode reasons"))?,conflict:x.18.map(|s|serde_json::from_str(&s)).transpose().map_err(db_err("decode conflict"))?}))
}

#[cfg(test)]
#[path = "slice_scope_result_test.rs"]
mod tests;

/// Publication stages supported by a scope decision.
pub const PUBLICATION_STAGES: [&str; 5] = [
    "referenced-notes",
    "built-hook",
    "dependencies",
    "replacements",
    "complete",
];
/// Accepted disposition vocabulary.
pub const DISPOSITIONS: [&str; 7] = [
    "approved",
    "held",
    "unverified",
    "handoff-pending",
    "handed-off",
    "released",
    "refused",
];
