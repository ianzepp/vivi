//! Replacement intents: the transactional claim that replaces one execution.
//!
//! [`PumpStore::schedule_replacement`] is the one atomic write behind a
//! replacement: it claims the trigger (first claim wins, so a repeated
//! trigger returns its original intent even after ownership advanced),
//! spends one unit of the owner-wide allowance, saves the intent, advances
//! ownership to the next sequence and updates the flow rows the replacement
//! touches. Any failure rolls the whole schedule back. Independent collectors
//! schedule without flow rows: the optional
//! [`StepJobRow`](crate::pump::flow::index::StepJobRow) is written only when
//! the caller supplies one, and no `step_jobs` entry is invented.
//!
//! The intent row keeps the replacement's coordinates for later callers: the
//! source settlement, the related task identities, the intent kind, the
//! eligibility deadline, cancellation and replay progress. Allowance is the
//! owner-wide balance on the owner rows — it is debited where the execution
//! stands, never inferred from sequence suffixes or attempt numbers.

#![cfg_attr(not(test), allow(dead_code))]

use rusqlite::{Connection, OptionalExtension, Row, params};
use sha2::{Digest, Sha256};

use crate::error::VivariumError;
use crate::pump::flow::index::StepJobRow;
use crate::pump::store::{PumpStore, db_err, from_u64};

use super::owners::ExecutionOwner;

/// The owner columns a replacement writes when it advances ownership.
const OWNER_COLUMNS: &str =
    "need, run, task, node, phase, seq, job, inputs_json, transient_json, allowance";

/// The intent columns, in read order.
const INTENT_COLUMNS: &str = "intent, collector, trigger_id, need, run, task, node, phase, seq, \
     kind, deadline, inputs_json, tasks_json, source_settled, cancelled, replay_seq, job, at";

/// One scheduled replacement of a step execution.
///
/// `seq` is the owner sequence the intent replaces; the advanced execution is
/// that sequence plus one. A repeated trigger returns the original row with
/// its original coordinates, untouched by later calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplacementIntent {
    pub intent: String,
    /// The collector that fired the trigger; claims are scoped per collector.
    pub collector: String,
    /// The trigger token, unique within its collector.
    pub trigger: String,
    pub need: String,
    pub run: u64,
    pub task: String,
    pub node: String,
    pub phase: String,
    /// The owner sequence the intent replaces.
    pub seq: u64,
    /// The caller's intent kind (for example a recollection flavor).
    pub kind: String,
    /// The eligibility deadline the caller declared (unix seconds).
    pub deadline: u64,
    /// The complete replacement inputs, saved with the advanced owner.
    pub inputs_json: String,
    /// The related task identities the intent names.
    pub tasks: Vec<String>,
    /// Whether the trigger's source is already settled.
    pub source_settled: bool,
    pub cancelled: bool,
    /// How far the replay over this intent has advanced.
    pub replay_seq: u64,
    /// The flow job bound to the advanced execution, when one was supplied.
    pub job: Option<String>,
    pub at: u64,
}

/// Everything one replacement schedule needs from its caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplacementRequest {
    /// The execution the caller believes is current: a mismatch refuses the
    /// replacement as stale work.
    pub expected: ExecutionOwner,
    pub collector: String,
    pub trigger: String,
    pub kind: String,
    pub deadline: u64,
    pub inputs_json: String,
    pub tasks: Vec<String>,
    pub source_settled: bool,
    pub job: Option<StepJobRow>,
    pub at: u64,
}

/// The two owner fields a replacement reads and moves: the current sequence
/// and the owner-wide allowance balance.
struct CurrentExecution {
    seq: u64,
    allowance: u64,
}

/// The deterministic id of an intent: a digest over the claim coordinates, so
/// the same claim always names the same intent and distinct claims never do.
fn intent_id(req: &ReplacementRequest) -> String {
    let expected = &req.expected;
    let seed = format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        req.collector,
        req.trigger,
        expected.need,
        expected.run,
        expected.task,
        expected.node,
        expected.phase,
        expected.seq
    );
    let digest = Sha256::digest(seed.as_bytes());
    format!("ri-{}", hex::encode(&digest[..16]))
}

impl PumpStore {
    /// Schedule the replacement of one logical execution atomically: claim
    /// the trigger, spend one unit of the owner-wide allowance, save the
    /// intent, advance ownership to the next sequence and update the flow
    /// rows. Any failure rolls every write back.
    ///
    /// A trigger that was already claimed — by the same collector, however
    /// long ago — returns its original intent and changes nothing. A request
    /// whose expected execution is not the current one is refused as stale.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the expected execution is stale or any
    /// write in the transaction fails.
    pub(crate) fn schedule_replacement(
        &self,
        req: &ReplacementRequest,
    ) -> Result<ReplacementIntent, VivariumError> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin replacement schedule"))?;
        if let Some(claimed) = claimed_intent(&tx, &req.collector, &req.trigger)? {
            return Ok(claimed);
        }
        let current = current_execution(&tx, req)?;
        let remaining = debit_allowance(&tx, req, &current)?;
        let intent = insert_intent(&tx, &intent_id(req), req)?;
        advance_owner(&tx, req, remaining)?;
        update_flow_rows(&tx, req)?;
        tx.commit().map_err(db_err("commit replacement schedule"))?;
        Ok(intent)
    }

    /// Every replacement intent of one run, in claim order, cancelled ones
    /// included.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the table cannot be read.
    pub(crate) fn replacement_intents(
        &self,
        need: &str,
        run: u64,
    ) -> Result<Vec<ReplacementIntent>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {INTENT_COLUMNS} FROM recovery_intents
                 WHERE need = ?1 AND run = ?2 ORDER BY rowid"
            ))
            .map_err(db_err("prepare recovery intents"))?;
        let rows = stmt
            .query_map(params![need, i64::try_from(run).unwrap_or(0)], read_intent)
            .map_err(db_err("read recovery intents"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(db_err("read recovery intent row"))
    }

    /// Cancel every open intent of one execution owner. Returns how many
    /// intents flipped to cancelled.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the table cannot be written.
    pub(crate) fn cancel_owner_intents(
        &self,
        need: &str,
        run: u64,
        task: &str,
        node: &str,
        phase: &str,
    ) -> Result<u64, VivariumError> {
        self.cancel_intents(
            "WHERE need = ?1 AND run = ?2 AND task = ?3 AND node = ?4 AND phase = ?5",
            params![need, i64::try_from(run).unwrap_or(0), task, node, phase,],
        )
    }

    /// Cancel every open intent of one run. Returns how many intents flipped
    /// to cancelled.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the table cannot be written.
    pub(crate) fn cancel_run_intents(&self, need: &str, run: u64) -> Result<u64, VivariumError> {
        self.cancel_intents(
            "WHERE need = ?1 AND run = ?2",
            params![need, i64::try_from(run).unwrap_or(0)],
        )
    }

    /// Record how far the replay over one intent has advanced.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub(crate) fn set_intent_replay(
        &self,
        intent: &str,
        replay_seq: u64,
    ) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "UPDATE recovery_intents SET replay_seq = ?2 WHERE intent = ?1",
                params![intent, i64::try_from(replay_seq).unwrap_or(i64::MAX)],
            )
            .map_err(db_err("write recovery intent replay"))?;
        Ok(())
    }

    /// Flip `cancelled` on every open intent matched by `filter`.
    fn cancel_intents(
        &self,
        filter: &str,
        bound: impl rusqlite::Params,
    ) -> Result<u64, VivariumError> {
        let changed = self
            .conn
            .execute(
                &format!("UPDATE recovery_intents SET cancelled = 1 {filter} AND cancelled = 0"),
                bound,
            )
            .map_err(db_err("cancel recovery intents"))?;
        Ok(from_u64(i64::try_from(changed).unwrap_or(0)))
    }
}

/// The intent an earlier schedule already claimed for this collector and
/// trigger, or `None` when the trigger is fresh.
fn claimed_intent(
    tx: &Connection,
    collector: &str,
    trigger: &str,
) -> Result<Option<ReplacementIntent>, VivariumError> {
    tx.query_row(
        &format!(
            "SELECT {INTENT_COLUMNS} FROM recovery_intents
             WHERE collector = ?1 AND trigger_id = ?2"
        ),
        params![collector, trigger],
        read_intent,
    )
    .optional()
    .map_err(db_err("read recovery intent claim"))
}

/// The current execution of the request's owner, or a stale-refusal error.
fn current_execution(
    tx: &Connection,
    req: &ReplacementRequest,
) -> Result<CurrentExecution, VivariumError> {
    let expected = &req.expected;
    let current = tx
        .query_row(
            "SELECT seq, allowance FROM recovery_owners
             WHERE need = ?1 AND run = ?2 AND task = ?3 AND node = ?4 AND phase = ?5
             ORDER BY seq DESC LIMIT 1",
            params![
                expected.need,
                i64::try_from(expected.run).unwrap_or(0),
                expected.task,
                expected.node,
                expected.phase
            ],
            |row| {
                Ok(CurrentExecution {
                    seq: from_u64(row.get(0)?),
                    allowance: from_u64(row.get(1)?),
                })
            },
        )
        .optional()
        .map_err(db_err("read current recovery execution"))?;
    match current {
        Some(current) if current.seq == expected.seq => Ok(current),
        Some(current) => Err(VivariumError::Other(format!(
            "recovery replacement: trigger {}/{}, expects {} seq {}, but seq {} is current",
            req.collector, req.trigger, expected.task, expected.seq, current.seq
        ))),
        None => Err(VivariumError::Other(format!(
            "recovery replacement: trigger {}/{}, expects {} seq {}, but the phase never ran",
            req.collector, req.trigger, expected.task, expected.seq
        ))),
    }
}

/// Spend one unit of the owner-wide allowance on the current execution and
/// return the remaining balance. A balance of zero spends nothing: allowance
/// is what the owner row grants, never a sequence or attempt suffix.
fn debit_allowance(
    tx: &Connection,
    req: &ReplacementRequest,
    current: &CurrentExecution,
) -> Result<u64, VivariumError> {
    let expected = &req.expected;
    let remaining = current.allowance.saturating_sub(1);
    tx.execute(
        "UPDATE recovery_owners SET allowance = ?7
         WHERE need = ?1 AND run = ?2 AND task = ?3 AND node = ?4 AND phase = ?5 AND seq = ?6",
        params![
            expected.need,
            i64::try_from(expected.run).unwrap_or(0),
            expected.task,
            expected.node,
            expected.phase,
            i64::try_from(current.seq).unwrap_or(0),
            i64::try_from(remaining).unwrap_or(i64::MAX),
        ],
    )
    .map_err(db_err("debit recovery allowance"))?;
    Ok(remaining)
}

/// Save the intent row and return the intent it stores.
fn insert_intent(
    tx: &Connection,
    intent: &str,
    req: &ReplacementRequest,
) -> Result<ReplacementIntent, VivariumError> {
    let expected = &req.expected;
    let tasks_json = serde_json::to_string(&req.tasks)
        .map_err(|e| VivariumError::Other(format!("recovery intent: encode tasks: {e}")))?;
    let built = ReplacementIntent {
        intent: intent.to_string(),
        collector: req.collector.clone(),
        trigger: req.trigger.clone(),
        need: expected.need.clone(),
        run: expected.run,
        task: expected.task.clone(),
        node: expected.node.clone(),
        phase: expected.phase.clone(),
        seq: expected.seq,
        kind: req.kind.clone(),
        deadline: req.deadline,
        inputs_json: req.inputs_json.clone(),
        tasks: req.tasks.clone(),
        source_settled: req.source_settled,
        cancelled: false,
        replay_seq: 0,
        job: req.job.as_ref().map(|row| row.job.clone()),
        at: req.at,
    };
    tx.execute(
        &format!(
            "INSERT INTO recovery_intents ({INTENT_COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)"
        ),
        params![
            built.intent,
            built.collector,
            built.trigger,
            built.need,
            i64::try_from(built.run).unwrap_or(0),
            built.task,
            built.node,
            built.phase,
            i64::try_from(built.seq).unwrap_or(0),
            built.kind,
            i64::try_from(built.deadline).unwrap_or(i64::MAX),
            built.inputs_json,
            tasks_json,
            built.source_settled,
            built.cancelled,
            i64::try_from(built.replay_seq).unwrap_or(0),
            built.job,
            i64::try_from(built.at).unwrap_or(0),
        ],
    )
    .map_err(db_err("write recovery intent"))?;
    Ok(built)
}

/// Append the next owner sequence with the replacement inputs; the remaining
/// allowance balance and a fresh transient usage carry the new execution.
fn advance_owner(
    tx: &Connection,
    req: &ReplacementRequest,
    remaining: u64,
) -> Result<(), VivariumError> {
    let expected = &req.expected;
    tx.execute(
        &format!(
            "INSERT INTO recovery_owners ({OWNER_COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
        ),
        params![
            expected.need,
            i64::try_from(expected.run).unwrap_or(0),
            expected.task,
            expected.node,
            expected.phase,
            i64::try_from(expected.seq.saturating_add(1)).unwrap_or(0),
            req.job.as_ref().map(|row| row.job.clone()),
            req.inputs_json,
            "{}",
            i64::try_from(remaining).unwrap_or(i64::MAX),
        ],
    )
    .map_err(db_err("advance recovery owner"))?;
    Ok(())
}

/// Update the flow rows the replacement touches: the step runs again, and the
/// optional flow job is recorded only when the caller supplied one.
fn update_flow_rows(tx: &Connection, req: &ReplacementRequest) -> Result<(), VivariumError> {
    tx.execute(
        "UPDATE flow_steps SET state = 'running' WHERE task = ?1",
        params![req.expected.task],
    )
    .map_err(db_err("advance flow step for replacement"))?;
    if let Some(row) = &req.job {
        tx.execute(
            "INSERT INTO step_jobs (job, need, run, node, task, attempt) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                row.job,
                req.expected.need,
                row.run,
                row.node,
                row.task,
                row.attempt
            ],
        )
        .map_err(db_err("insert replacement step job"))?;
    }
    Ok(())
}

fn read_intent(row: &Row<'_>) -> rusqlite::Result<ReplacementIntent> {
    let tasks_json: String = row.get(12)?;
    let tasks = serde_json::from_str(&tasks_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(12, rusqlite::types::Type::Text, e.into())
    })?;
    Ok(ReplacementIntent {
        intent: row.get(0)?,
        collector: row.get(1)?,
        trigger: row.get(2)?,
        need: row.get(3)?,
        run: from_u64(row.get(4)?),
        task: row.get(5)?,
        node: row.get(6)?,
        phase: row.get(7)?,
        seq: from_u64(row.get(8)?),
        kind: row.get(9)?,
        deadline: from_u64(row.get(10)?),
        inputs_json: row.get(11)?,
        tasks,
        source_settled: from_u64(row.get(13)?) != 0,
        cancelled: from_u64(row.get(14)?) != 0,
        replay_seq: from_u64(row.get(15)?),
        job: row.get(16)?,
        at: from_u64(row.get(17)?),
    })
}

#[cfg(test)]
#[path = "../recovery_intent_test.rs"]
mod tests;
