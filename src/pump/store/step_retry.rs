//! Durable retry intents and per-generation redispatch budgets of `pump.db`.
//!
//! Before a failed step job's source can be marked applied, the deciding path
//! reserves one immutable retry intent here: the owner execution key and
//! generation, the failed job, the typed failure class with its evidence
//! line, the retry ordinal, the stable replacement identity, the complete
//! serialized [`JobSpec`], and the chosen dispatch deadline, plus the
//! materialization / cancellation / settlement state.
//!
//! Each input generation — one execution of one task of one run under one
//! phase — holds a budget of [`REDISPATCH_BUDGET`] reservations. The budget
//! is counted from the stored rows, so a replayed reservation, a store
//! reload or a repeated materialization never resets or multiplies it,
//! while a genuinely new generation and every other phase start fresh.
//! Reservations are answered from the stored decision: a replay returns the
//! stored intent unchanged, never a rewrite of its inputs or deadline.
//!
//! Like the other flow tables, these tables are created with `IF NOT EXISTS`
//! on every open and are not part of the [`State`](crate::pump::state::State)
//! save, so `save()` and `clear()` never touch them and the schema version is
//! not bumped.

use rusqlite::{Connection, params};

use super::step_execution::ExecutionKey;
use super::{PumpStore, db_err, from_u32, from_u64, json};
use crate::error::VivariumError;
use crate::pump::jobs::JobSpec;

/// Reservations one input generation may hold: at most two redispatches.
pub const REDISPATCH_BUDGET: u32 = 2;

/// One requested retry redispatch, reserved before its source is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryIntent {
    /// The execution key of the failed input.
    pub key: ExecutionKey,
    /// The input generation the failed execution belongs to.
    pub generation: u32,
    /// The identity of the failed execution; kept for traceability.
    pub execution: String,
    /// The failed job this intent replaces.
    pub failed_job: String,
    /// The typed failure class (`lost`, `timeout`, `throttled`, `auth`, ...).
    pub failure_class: String,
    /// One line of machine evidence for the class.
    pub evidence: String,
    /// The stable identity of the replacement job.
    pub replacement_job: String,
    /// The complete replacement dispatch input.
    pub spec: JobSpec,
    /// Earliest epoch second the replacement may dispatch.
    pub deadline: u64,
}

/// One immutable stored retry intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryIntentRow {
    pub need: String,
    pub run: u32,
    pub task: String,
    pub phase: String,
    pub generation: u32,
    pub execution: String,
    pub failed_job: String,
    pub failure_class: String,
    pub evidence: String,
    /// The 1-based position of this reservation in the generation's budget.
    pub ordinal: u32,
    pub replacement_job: String,
    pub spec: JobSpec,
    pub deadline: u64,
    pub materialized_at: Option<u64>,
    pub cancelled_at: Option<u64>,
    pub settled_at: Option<u64>,
}

impl RetryIntentRow {
    /// This row's execution key.
    pub fn key(&self) -> ExecutionKey {
        ExecutionKey {
            need: self.need.clone(),
            run: self.run,
            task: self.task.clone(),
            phase: self.phase.clone(),
        }
    }

    /// True while the intent still owes a redispatch or a result: neither
    /// cancelled nor settled.
    #[must_use]
    pub fn is_incomplete(&self) -> bool {
        self.cancelled_at.is_none() && self.settled_at.is_none()
    }
}

/// The outcome of one reserve attempt against a generation's budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryReservation {
    /// A new intent consumed the next ordinal of the generation's budget.
    Reserved(RetryIntentRow),
    /// The same reservation replayed; the stored intent answers unchanged.
    Existing(RetryIntentRow),
    /// The generation already spent its budget; carries the ordered history.
    Exhausted(Vec<RetryIntentRow>),
}

const DDL: &str = "
CREATE TABLE IF NOT EXISTS step_retry_intents (
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    task TEXT NOT NULL,
    phase TEXT NOT NULL,
    generation INTEGER NOT NULL,
    execution TEXT NOT NULL,
    failed_job TEXT NOT NULL,
    failure_class TEXT NOT NULL,
    evidence TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    replacement_job TEXT NOT NULL UNIQUE,
    spec_json TEXT NOT NULL,
    deadline INTEGER NOT NULL,
    materialized_at INTEGER,
    cancelled_at INTEGER,
    settled_at INTEGER,
    created_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (need, run, task, phase, generation, failed_job)
);
CREATE INDEX IF NOT EXISTS step_retry_intents_run ON step_retry_intents (need, run);
";

/// Create the retry intent table when it is missing.
pub(super) fn ensure_tables(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(DDL)
        .map_err(db_err("create step retry tables"))
}

const COLUMNS: &str = "need, run, task, phase, generation, execution, failed_job, \
     failure_class, evidence, ordinal, replacement_job, spec_json, deadline, \
     materialized_at, cancelled_at, settled_at";

fn intent_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(RetryIntentRow, String)> {
    Ok((
        RetryIntentRow {
            need: row.get(0)?,
            run: from_u32(row.get(1)?),
            task: row.get(2)?,
            phase: row.get(3)?,
            generation: from_u32(row.get(4)?),
            execution: row.get(5)?,
            failed_job: row.get(6)?,
            failure_class: row.get(7)?,
            evidence: row.get(8)?,
            ordinal: from_u32(row.get(9)?),
            replacement_job: row.get(10)?,
            spec: JobSpec::new("", "", "", "", ""),
            deadline: from_u64(row.get(12)?),
            materialized_at: row.get::<_, Option<i64>>(13)?.map(from_u64),
            cancelled_at: row.get::<_, Option<i64>>(14)?.map(from_u64),
            settled_at: row.get::<_, Option<i64>>(15)?.map(from_u64),
        },
        row.get(11)?,
    ))
}

fn finish_intent(
    (mut intent, spec_json): (RetryIntentRow, String),
) -> Result<RetryIntentRow, VivariumError> {
    intent.spec = serde_json::from_str(&spec_json).map_err(|e| {
        VivariumError::Other(format!(
            "pump.db step_retry_intents: decode spec of {}: {e}",
            intent.replacement_job
        ))
    })?;
    Ok(intent)
}

fn now() -> &'static str {
    "CAST(strftime('%s', 'now') AS INTEGER)"
}

fn no_such_intent(replacement_job: &str) -> VivariumError {
    VivariumError::Other(format!("retry intent {replacement_job}: no such intent"))
}

fn query_intents(
    conn: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<RetryIntentRow>, VivariumError> {
    let mut stmt = conn.prepare(sql).map_err(db_err("prepare retry intents"))?;
    let rows = stmt
        .query_map(args, intent_from_row)
        .map_err(db_err("read retry intents"))?;
    let decoded: Vec<_> = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read retry intent row"))?
        .into_iter()
        .map(finish_intent)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(decoded)
}

fn generation_intents(
    conn: &Connection,
    key: &ExecutionKey,
    generation: u32,
) -> Result<Vec<RetryIntentRow>, VivariumError> {
    query_intents(
        conn,
        &format!(
            "SELECT {COLUMNS} FROM step_retry_intents \
             WHERE need = ?1 AND run = ?2 AND task = ?3 AND phase = ?4 AND generation = ?5 \
             ORDER BY ordinal"
        ),
        params![key.need, key.run, key.task, key.phase, generation],
    )
}

fn insert_intent(
    conn: &Connection,
    intent: &RetryIntent,
    ordinal: u32,
) -> Result<(), VivariumError> {
    conn.execute(
        &format!(
            "INSERT INTO step_retry_intents \
               (need, run, task, phase, generation, execution, failed_job, \
                failure_class, evidence, ordinal, replacement_job, spec_json, \
                deadline, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, {})",
            now()
        ),
        params![
            intent.key.need,
            intent.key.run,
            intent.key.task,
            intent.key.phase,
            intent.generation,
            intent.execution,
            intent.failed_job,
            intent.failure_class,
            intent.evidence,
            ordinal,
            intent.replacement_job,
            json(&intent.spec)?,
            i64::try_from(intent.deadline).unwrap_or(i64::MAX),
        ],
    )
    .map_err(db_err("insert retry intent"))?;
    Ok(())
}

impl PumpStore {
    /// Reserve one retry redispatch of the input generation `intent` names.
    ///
    /// The first reservation of a `(key, generation, failed_job)` triple is
    /// inserted transactionally with the generation's next ordinal, at most
    /// [`REDISPATCH_BUDGET`] per generation; the budget counts every stored
    /// intent, whatever its later state. A replay — the same triple again,
    /// whatever inputs or deadline it now carries — returns the stored intent
    /// unchanged and writes nothing. A spent budget answers
    /// [`RetryReservation::Exhausted`] with the generation's ordered history.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read or written.
    pub fn reserve_retry(&self, intent: &RetryIntent) -> Result<RetryReservation, VivariumError> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin retry reservation"))?;
        let key = &intent.key;
        let existing = query_intents(
            &tx,
            &format!(
                "SELECT {COLUMNS} FROM step_retry_intents \
                 WHERE need = ?1 AND run = ?2 AND task = ?3 AND phase = ?4 \
                   AND generation = ?5 AND failed_job = ?6"
            ),
            params![
                key.need,
                key.run,
                key.task,
                key.phase,
                intent.generation,
                intent.failed_job
            ],
        )?;
        if let Some(row) = existing.into_iter().next() {
            return Ok(RetryReservation::Existing(row));
        }
        let history = generation_intents(&tx, key, intent.generation)?;
        if history.len() >= usize::try_from(REDISPATCH_BUDGET).unwrap_or(0) {
            return Ok(RetryReservation::Exhausted(history));
        }
        let ordinal = u32::try_from(history.len() + 1).unwrap_or(u32::MAX);
        insert_intent(&tx, intent, ordinal)?;
        tx.commit().map_err(db_err("commit retry reservation"))?;
        Ok(RetryReservation::Reserved(RetryIntentRow {
            need: key.need.clone(),
            run: key.run,
            task: key.task.clone(),
            phase: key.phase.clone(),
            generation: intent.generation,
            execution: intent.execution.clone(),
            failed_job: intent.failed_job.clone(),
            failure_class: intent.failure_class.clone(),
            evidence: intent.evidence.clone(),
            ordinal,
            replacement_job: intent.replacement_job.clone(),
            spec: intent.spec.clone(),
            deadline: intent.deadline,
            materialized_at: None,
            cancelled_at: None,
            settled_at: None,
        }))
    }

    /// The generation's stored intents, oldest ordinal first: the ordered
    /// decision history a collector replays from.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn retry_intents(
        &self,
        key: &ExecutionKey,
        generation: u32,
    ) -> Result<Vec<RetryIntentRow>, VivariumError> {
        generation_intents(&self.conn, key, generation)
    }

    /// The intent a replacement identity names, current or settled.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn retry_intent(
        &self,
        replacement_job: &str,
    ) -> Result<Option<RetryIntentRow>, VivariumError> {
        Ok(query_intents(
            &self.conn,
            &format!("SELECT {COLUMNS} FROM step_retry_intents WHERE replacement_job = ?1"),
            params![replacement_job],
        )?
        .into_iter()
        .next())
    }

    /// Every intent that still owes a redispatch or a result, creation order.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn incomplete_retry_intents(&self) -> Result<Vec<RetryIntentRow>, VivariumError> {
        query_intents(
            &self.conn,
            &format!(
                "SELECT {COLUMNS} FROM step_retry_intents \
                 WHERE cancelled_at IS NULL AND settled_at IS NULL \
                 ORDER BY created_at, replacement_job"
            ),
            [],
        )
    }

    /// Record that the replacement was materialized. Idempotent: the first
    /// stamp wins and no later call rewrites it.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the intent is unknown or the row
    /// cannot be written.
    pub fn mark_retry_materialized(
        &self,
        replacement_job: &str,
        at: u64,
    ) -> Result<(), VivariumError> {
        self.stamp_intent(
            replacement_job,
            "materialized_at",
            at,
            "materialize retry intent",
        )
    }

    /// Record that the intent was cancelled without its redispatch. Idempotent.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the intent is unknown or the row
    /// cannot be written.
    pub fn cancel_retry_intent(&self, replacement_job: &str, at: u64) -> Result<(), VivariumError> {
        self.stamp_intent(replacement_job, "cancelled_at", at, "cancel retry intent")
    }

    /// Record that the replacement's result was applied. Idempotent.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the intent is unknown or the row
    /// cannot be written.
    pub fn settle_retry_intent(&self, replacement_job: &str, at: u64) -> Result<(), VivariumError> {
        self.stamp_intent(replacement_job, "settled_at", at, "settle retry intent")
    }

    /// Set one lifecycle stamp, keeping the first value ever written.
    fn stamp_intent(
        &self,
        replacement_job: &str,
        column: &str,
        at: u64,
        what: &'static str,
    ) -> Result<(), VivariumError> {
        let changed = self
            .conn
            .execute(
                &format!(
                    "UPDATE step_retry_intents SET {column} = COALESCE({column}, ?2) \
                     WHERE replacement_job = ?1"
                ),
                params![replacement_job, i64::try_from(at).unwrap_or(i64::MAX)],
            )
            .map_err(db_err(what))?;
        if changed == 0 {
            return Err(no_such_intent(replacement_job));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "step_retry_test.rs"]
mod tests;
