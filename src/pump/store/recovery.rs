//! Frozen transient recovery intents of `pump.db`.
//!
//! A transient step failure may be worth one deterministic retry. Before the
//! pump acts on that allowance it freezes an intent here: the failed step and
//! job, the input identity the job ran on, the complete replacement request,
//! and the absolute deadline the replacement must land inside. Replay-safe by
//! construction — inserting a failed job that already has an intent returns
//! the stored row unchanged, so a replay can never move a deadline, spend a
//! second ordinal, or rewrite the frozen request.
//!
//! Independently collected reslice and slice-merge jobs stay distinguishable
//! from indexed step jobs: the step-job binding is `None` for the former,
//! and the replacement request's kind is a lookup column of its own.
//!
//! The table sits beside the v8 schema like the flow tables: created with
//! `IF NOT EXISTS` on every open and never touched by the `State` save or
//! clear, so no schema version moves and no migration is needed.

/// Parked origins waiting at their gates, and the continuation journals that
/// replay them; [`pause::ensure_tables`] runs beside the intent tables below.
pub mod pause;

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::VivariumError;
use crate::pump::flow::index::StepJobRow;
use crate::pump::jobs::JobSpec;

use super::{PumpStore, db_err, from_u32, from_u64};

/// The replay/application state vocabulary of a recovery intent.
pub mod recovery_state {
    /// Frozen; the recovery has not fired yet.
    pub const PENDING: &str = "pending";
    /// The replacement request was minted from this intent.
    pub const APPLIED: &str = "applied";

    /// Every state word, for error text.
    pub const ALL: &[&str] = &[PENDING, APPLIED];

    /// Whether `word` is in the vocabulary.
    #[must_use]
    pub fn is_known(word: &str) -> bool {
        ALL.contains(&word)
    }
}

/// One frozen recovery intent.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryIntentRow {
    /// The need whose flow run failed.
    pub need: String,
    /// The flow run of the failed step.
    pub run: u32,
    /// The flow node of the failed step.
    pub node: String,
    /// The failed step task.
    pub task: String,
    /// The caller's identity of the step input the failed job ran on; one
    /// retry ordinal is spent per step input.
    pub input: String,
    /// The failed job's id; one intent per failed job.
    pub failed_job: String,
    /// The deterministic id of the replacement job.
    pub replacement_job: String,
    /// The replacement request's durable kind label, for lookup by kind.
    pub kind: String,
    /// Which retry of this step input the intent is: 1 for the first.
    pub ordinal: u32,
    /// The transient class that froze the intent.
    pub reason: String,
    /// Absolute unix-second deadline; a replay never moves it.
    pub deadline: u64,
    /// The complete frozen replacement request.
    pub request: JobSpec,
    /// The replacement's step-job binding when it is an indexed step job;
    /// `None` for independently collected reslice and slice-merge jobs.
    pub step_job: Option<StepJobRow>,
    /// Replay/application state; see [`recovery_state`].
    pub state: String,
    /// When the intent was applied, if it was.
    pub applied_at: Option<u64>,
}

const DDL: &str = "
CREATE TABLE IF NOT EXISTS recovery_intents (
    replacement_job TEXT PRIMARY KEY,
    failed_job TEXT NOT NULL UNIQUE,
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    node TEXT NOT NULL,
    task TEXT NOT NULL,
    input TEXT NOT NULL,
    kind TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    reason TEXT NOT NULL,
    deadline INTEGER NOT NULL,
    request_json TEXT NOT NULL,
    bind_job TEXT,
    bind_run INTEGER,
    bind_node TEXT,
    bind_task TEXT,
    bind_attempt INTEGER,
    state TEXT NOT NULL DEFAULT 'pending',
    applied_at INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS recovery_intents_input
    ON recovery_intents (task, input, ordinal);
CREATE INDEX IF NOT EXISTS recovery_intents_need
    ON recovery_intents (need, kind, state);
";

const COLUMNS: &str = "replacement_job, failed_job, need, run, node, task, input, kind, ordinal, \
                       reason, deadline, request_json, bind_job, bind_run, bind_node, bind_task, \
                       bind_attempt, state, applied_at";

/// Create the recovery tables when they are missing.
pub(super) fn ensure_tables(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(DDL)
        .map_err(db_err("create recovery intent tables"))?;
    pause::ensure_tables(conn)
}

/// The optional step-job binding: `None` while the bind job column is null.
fn bind_row(row: &Row<'_>) -> rusqlite::Result<Option<StepJobRow>> {
    match row.get::<_, Option<String>>(12)? {
        None => Ok(None),
        Some(job) => Ok(Some(StepJobRow {
            job,
            run: row.get::<_, i64>(13).map(from_u32)?,
            node: row.get(14)?,
            task: row.get(15)?,
            attempt: row.get::<_, i64>(16).map(from_u32)?,
        })),
    }
}

/// Read one result row into the intent, its request JSON, and its applied
/// stamp; [`decode`] finishes the intent.
fn raw_row(row: &Row<'_>) -> rusqlite::Result<(RecoveryIntentRow, String, Option<i64>)> {
    Ok((
        RecoveryIntentRow {
            replacement_job: row.get(0)?,
            failed_job: row.get(1)?,
            need: row.get(2)?,
            run: row.get::<_, i64>(3).map(from_u32)?,
            node: row.get(4)?,
            task: row.get(5)?,
            input: row.get(6)?,
            kind: row.get(7)?,
            ordinal: row.get::<_, i64>(8).map(from_u32)?,
            reason: row.get(9)?,
            deadline: row.get::<_, i64>(10).map(from_u64)?,
            request: JobSpec::new("", "", "", "", ""),
            step_job: bind_row(row)?,
            state: row.get(17)?,
            applied_at: None,
        },
        row.get(11)?,
        row.get(18)?,
    ))
}

fn decode(
    (mut intent, request_json, applied_at): (RecoveryIntentRow, String, Option<i64>),
) -> Result<RecoveryIntentRow, VivariumError> {
    intent.request =
        serde_json::from_str(&request_json).map_err(db_err("decode recovery request"))?;
    intent.applied_at = applied_at.map(from_u64);
    if !recovery_state::is_known(&intent.state) {
        return Err(VivariumError::Other(format!(
            "pump.db recovery_intents: unknown intent state {:?}",
            intent.state
        )));
    }
    Ok(intent)
}

fn check_state(word: &str) -> Result<(), VivariumError> {
    if recovery_state::is_known(word) {
        Ok(())
    } else {
        Err(VivariumError::Other(format!(
            "recovery intent state: expected one of {}; actual: {word}",
            recovery_state::ALL.join(", ")
        )))
    }
}

/// `at` as a stored stamp, saturating at the largest second.
fn stamp(at: u64) -> i64 {
    i64::try_from(at).unwrap_or(i64::MAX)
}

impl PumpStore {
    /// Freeze one recovery intent, or return the stored one on replay.
    ///
    /// A replay names a failed job that already has an intent: the stored row
    /// is returned unchanged, so a replay can never move the deadline, spend
    /// another ordinal, or replace the frozen request — even when it presents
    /// a different replacement id, reason or spec. A *different* failed job
    /// claiming an occupied replacement id, or an ordinal the same step input
    /// already spent, is refused as a hard insert error.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the state word is unknown, the request
    /// cannot be encoded, or the insert violates one intent per failed job or
    /// one ordinal per step input.
    pub fn insert_recovery_intent(
        &self,
        row: &RecoveryIntentRow,
    ) -> Result<RecoveryIntentRow, VivariumError> {
        if let Some(stored) = self.recovery_intent_of_failed_job(&row.failed_job)? {
            return Ok(stored);
        }
        check_state(&row.state)?;
        let request =
            serde_json::to_string(&row.request).map_err(db_err("encode recovery request"))?;
        let (bind_job, bind_run, bind_node, bind_task, bind_attempt) = match &row.step_job {
            None => (None, None, None, None, None),
            Some(job) => (
                Some(job.job.as_str()),
                Some(i64::from(job.run)),
                Some(job.node.as_str()),
                Some(job.task.as_str()),
                Some(i64::from(job.attempt)),
            ),
        };
        self.conn
            .execute(
                "INSERT INTO recovery_intents (replacement_job, failed_job, need, run, node, \
                 task, input, kind, ordinal, reason, deadline, request_json, bind_job, bind_run, \
                 bind_node, bind_task, bind_attempt, state, applied_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
                 ?17, ?18, ?19)",
                params![
                    row.replacement_job,
                    row.failed_job,
                    row.need,
                    i64::from(row.run),
                    row.node,
                    row.task,
                    row.input,
                    row.kind,
                    i64::from(row.ordinal),
                    row.reason,
                    stamp(row.deadline),
                    request,
                    bind_job,
                    bind_run,
                    bind_node,
                    bind_task,
                    bind_attempt,
                    row.state,
                    row.applied_at.map(stamp),
                ],
            )
            .map_err(db_err("write recovery intent"))?;
        Ok(row.clone())
    }

    /// The intent frozen for replacement job `replacement_job`, if any.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read or decoded.
    pub fn recovery_intent(
        &self,
        replacement_job: &str,
    ) -> Result<Option<RecoveryIntentRow>, VivariumError> {
        self.one_intent("replacement_job = ?1", params![replacement_job])
    }

    /// The intent frozen for failed job `failed_job`, if any.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read or decoded.
    pub fn recovery_intent_of_failed_job(
        &self,
        failed_job: &str,
    ) -> Result<Option<RecoveryIntentRow>, VivariumError> {
        self.one_intent("failed_job = ?1", params![failed_job])
    }

    fn one_intent(
        &self,
        filter: &str,
        args: impl rusqlite::Params,
    ) -> Result<Option<RecoveryIntentRow>, VivariumError> {
        let sql = format!("SELECT {COLUMNS} FROM recovery_intents WHERE {filter}");
        self.conn
            .query_row(&sql, args, raw_row)
            .optional()
            .map_err(db_err("read recovery intent"))?
            .map(decode)
            .transpose()
    }

    /// Mark the intent of `replacement_job` applied: its replacement request
    /// was minted. Idempotent — a repeated mark keeps the first stamp.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when no intent carries the replacement job
    /// or the update cannot be written.
    pub fn mark_recovery_applied(
        &self,
        replacement_job: &str,
        at: u64,
    ) -> Result<(), VivariumError> {
        let Some(existing) = self.recovery_intent(replacement_job)? else {
            return Err(VivariumError::Other(format!(
                "pump.db: no recovery intent names replacement job {replacement_job:?}"
            )));
        };
        if existing.state == recovery_state::APPLIED {
            return Ok(());
        }
        self.conn
            .execute(
                "UPDATE recovery_intents SET state = ?2, applied_at = ?3 \
                 WHERE replacement_job = ?1",
                params![replacement_job, recovery_state::APPLIED, stamp(at)],
            )
            .map_err(db_err("mark recovery applied"))?;
        Ok(())
    }

    /// The highest retry ordinal already spent on `(task, input)`. Absence
    /// means no allowance is spent, so the first retry is still legal.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the maximum cannot be read.
    pub fn recovery_allowance(&self, task: &str, input: &str) -> Result<u32, VivariumError> {
        let spent: Option<i64> = self
            .conn
            .query_row(
                "SELECT MAX(ordinal) FROM recovery_intents WHERE task = ?1 AND input = ?2",
                params![task, input],
                |row| row.get(0),
            )
            .map_err(db_err("read recovery allowance"))?;
        Ok(from_u32(spent.unwrap_or(0)))
    }

    /// Every unfinished (pending) intent, soonest deadline first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when a row cannot be read or decoded.
    pub fn unfinished_recovery_intents(&self) -> Result<Vec<RecoveryIntentRow>, VivariumError> {
        self.intent_rows(
            &format!(
                "SELECT {COLUMNS} FROM recovery_intents WHERE state = ?1 \
                 ORDER BY deadline, replacement_job"
            ),
            params![recovery_state::PENDING],
        )
    }

    /// Every pending intent of `need` whose replacement request has `kind`,
    /// lowest ordinal first. Applied intents are never answered.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when a row cannot be read or decoded.
    pub fn pending_recoveries(
        &self,
        need: &str,
        kind: &str,
    ) -> Result<Vec<RecoveryIntentRow>, VivariumError> {
        self.intent_rows(
            &format!(
                "SELECT {COLUMNS} FROM recovery_intents WHERE state = ?1 AND need = ?2 \
                 AND kind = ?3 ORDER BY task, ordinal, replacement_job"
            ),
            params![recovery_state::PENDING, need, kind],
        )
    }

    fn intent_rows(
        &self,
        sql: &str,
        args: impl rusqlite::Params,
    ) -> Result<Vec<RecoveryIntentRow>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(db_err("prepare recovery intent read"))?;
        let raw = stmt
            .query_map(args, raw_row)
            .map_err(db_err("read recovery intents"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err("read recovery intents"))?;
        raw.into_iter().map(decode).collect()
    }
}

#[cfg(test)]
#[path = "recovery_test.rs"]
mod tests;
