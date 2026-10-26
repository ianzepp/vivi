//! The flow-retry tables of `pump.db`: durable bounded-retry decisions for
//! failed executable step jobs, kept independently of job snapshots.
//!
//! A step whose executable job failed may be redispatched at most
//! [`RETRY_LIMIT`] times per input generation. The allowance is shared by
//! every failure class and by correction executions of the same step and
//! generation: only the step coordinates and the generation decide it, so a
//! new cause or a reopened store never resets a half-spent budget. Each
//! decision is keyed idempotently by the failed job, so a repeated request
//! returns the originally recorded decision even when called with a later
//! clock. The tables live beside the state tables, so ordinary [`PumpStore`]
//! `save` and `load` never touch them and no schema-version migration is
//! needed.

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::VivariumError;

use super::{PumpStore, db_err, from_u32, from_u64};

/// Replacement redispatches one step and input generation may consume,
/// shared across failure classes and correction executions.
pub const RETRY_LIMIT: u32 = 3;

const DDL: &str = "
CREATE TABLE IF NOT EXISTS flow_retry_decisions (
    failed_job TEXT PRIMARY KEY,
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    node TEXT NOT NULL,
    task TEXT NOT NULL,
    generation INTEGER NOT NULL,
    failure_class TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    replacement_job TEXT,
    replacement_order INTEGER,
    deadline INTEGER NOT NULL,
    allowed INTEGER NOT NULL CHECK (allowed IN (0, 1)),
    decided_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS flow_retry_decisions_step
    ON flow_retry_decisions (need, run, node, task, generation);
";

/// One failed executable job asking for a bounded retry of its step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowRetryRequest {
    pub need: String,
    pub run: u32,
    pub node: String,
    pub task: String,
    /// The input generation the failed job ran under; new generations get a
    /// fresh [`RETRY_LIMIT`] allowance.
    pub generation: u32,
    /// The id of the failed executable job: the idempotency key of a decision.
    pub failed_job: String,
    /// Machine-readable cause of the failure, e.g. `budget` or `env`. Never
    /// part of the budget key.
    pub failure_class: String,
    /// The replacement job the caller already means to enqueue, when known;
    /// `None` leaves the decision partially applied until
    /// [`PumpStore::set_flow_retry_replacement`] records it.
    pub replacement_job: Option<String>,
    /// The fixed deadline the replacement must finish by (unix seconds).
    pub deadline: u64,
}

/// One recorded decision: whether a failed job may be redispatched, and what
/// its replacement is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowRetryDecision {
    pub need: String,
    pub run: u32,
    pub node: String,
    pub task: String,
    pub generation: u32,
    pub failed_job: String,
    pub failure_class: String,
    /// The retry ordinal of this decision: `1..=[RETRY_LIMIT]` when allowed,
    /// one past the spent allowance when exhausted.
    pub ordinal: u32,
    /// The replacement job identity, when it is already known.
    pub replacement_job: Option<String>,
    /// The replacement's order among the replacements of the step and
    /// generation; present exactly when `replacement_job` is.
    pub replacement_order: Option<u32>,
    /// The fixed deadline recorded with the decision (unix seconds).
    pub deadline: u64,
    /// Whether the failed job may be redispatched.
    pub allowed: bool,
    /// When the decision was recorded (unix seconds).
    pub decided_at: u64,
}

const COLUMNS: &str = "failed_job, need, run, node, task, generation, failure_class, ordinal, \
                       replacement_job, replacement_order, deadline, allowed, decided_at";

/// Create the flow-retry tables when they are missing.
pub(super) fn ensure_tables(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(DDL)
        .map_err(db_err("create flow retry tables"))
}

type DecisionRow = (
    String,
    String,
    i64,
    String,
    String,
    i64,
    String,
    i64,
    Option<String>,
    Option<i64>,
    i64,
    bool,
    i64,
);

fn read_row(row: &Row<'_>) -> rusqlite::Result<DecisionRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
    ))
}

fn decode(raw: DecisionRow) -> FlowRetryDecision {
    let (
        failed_job,
        need,
        run,
        node,
        task,
        generation,
        failure_class,
        ordinal,
        replacement_job,
        replacement_order,
        deadline,
        allowed,
        decided_at,
    ) = raw;
    FlowRetryDecision {
        need,
        run: from_u32(run),
        node,
        task,
        generation: from_u32(generation),
        failed_job,
        failure_class,
        ordinal: from_u32(ordinal),
        replacement_job,
        replacement_order: replacement_order.map(from_u32),
        deadline: from_u64(deadline),
        allowed,
        decided_at: from_u64(decided_at),
    }
}

/// The decision recorded for `failed_job`, when any.
fn read_decision(
    conn: &Connection,
    failed_job: &str,
) -> Result<Option<FlowRetryDecision>, VivariumError> {
    let raw = conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM flow_retry_decisions WHERE failed_job = ?1"),
            [failed_job],
            read_row,
        )
        .optional()
        .map_err(db_err("read flow retry decision"))?;
    Ok(raw.map(decode))
}

/// The allowance slots the step and input generation have already spent,
/// across every failure class and correction execution.
fn spent_allowance(conn: &Connection, req: &FlowRetryRequest) -> Result<u32, VivariumError> {
    conn.query_row(
        "SELECT COUNT(*) FROM flow_retry_decisions
         WHERE need = ?1 AND run = ?2 AND node = ?3 AND task = ?4
           AND generation = ?5 AND allowed = 1",
        params![
            req.need,
            i64::from(req.run),
            req.node,
            req.task,
            i64::from(req.generation)
        ],
        |row| row.get::<_, i64>(0),
    )
    .map(from_u32)
    .map_err(db_err("count flow retry decisions"))
}

fn insert_decision(
    tx: &Connection,
    req: &FlowRetryRequest,
    ordinal: u32,
    replacement_order: Option<u32>,
    allowed: bool,
    now: u64,
) -> Result<(), VivariumError> {
    tx.execute(
        "INSERT INTO flow_retry_decisions (
             failed_job, need, run, node, task, generation, failure_class, ordinal,
             replacement_job, replacement_order, deadline, allowed, decided_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            req.failed_job,
            req.need,
            i64::from(req.run),
            req.node,
            req.task,
            i64::from(req.generation),
            req.failure_class,
            i64::from(ordinal),
            req.replacement_job,
            replacement_order.map(i64::from),
            i64::try_from(req.deadline).unwrap_or(0),
            allowed,
            i64::try_from(now).unwrap_or(0),
        ],
    )
    .map_err(db_err("write flow retry decision"))?;
    Ok(())
}

fn decision_of(
    req: &FlowRetryRequest,
    ordinal: u32,
    replacement_order: Option<u32>,
    allowed: bool,
    now: u64,
) -> FlowRetryDecision {
    FlowRetryDecision {
        need: req.need.clone(),
        run: req.run,
        node: req.node.clone(),
        task: req.task.clone(),
        generation: req.generation,
        failed_job: req.failed_job.clone(),
        failure_class: req.failure_class.clone(),
        ordinal,
        replacement_job: req.replacement_job.clone(),
        replacement_order,
        deadline: req.deadline,
        allowed,
        decided_at: now,
    }
}

impl PumpStore {
    /// Decide — once — whether the failed executable job `req` describes may
    /// be redispatched, consuming one of the step's [`RETRY_LIMIT`] allowance
    /// slots when so.
    ///
    /// The decision and the budget consumption happen in one transaction,
    /// before any replacement the caller enqueues from it. A request for an
    /// already-decided failed job returns the stored decision unchanged: a
    /// later clock, another deadline, cause or replacement never rewrite it.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the transaction or any write fails.
    pub fn decide_flow_retry(
        &self,
        req: &FlowRetryRequest,
        now: u64,
    ) -> Result<FlowRetryDecision, VivariumError> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin flow retry decision"))?;
        if let Some(existing) = read_decision(&tx, &req.failed_job)? {
            return Ok(existing);
        }
        let used = spent_allowance(&tx, req)?;
        let allowed = used < RETRY_LIMIT;
        let ordinal = used + 1;
        let replacement_order = req.replacement_job.as_ref().map(|_| ordinal);
        insert_decision(&tx, req, ordinal, replacement_order, allowed, now)?;
        tx.commit().map_err(db_err("commit flow retry decision"))?;
        Ok(decision_of(req, ordinal, replacement_order, allowed, now))
    }

    /// The recorded decision of one failed job, when any: also the recovery
    /// read for a caller that crashed between allocation and enqueue.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn flow_retry_decision(
        &self,
        failed_job: &str,
    ) -> Result<Option<FlowRetryDecision>, VivariumError> {
        read_decision(&self.conn, failed_job)
    }

    /// Attach the replacement job of an allocated decision, finishing a
    /// partially applied one. Returns the updated decision, or `None` when
    /// `failed_job` has no decision.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the transaction or any write fails.
    pub fn set_flow_retry_replacement(
        &self,
        failed_job: &str,
        replacement_job: &str,
    ) -> Result<Option<FlowRetryDecision>, VivariumError> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin flow retry replacement"))?;
        let Some(decision) = read_decision(&tx, failed_job)? else {
            return Ok(None);
        };
        tx.execute(
            "UPDATE flow_retry_decisions SET replacement_job = ?2, replacement_order = ?3
             WHERE failed_job = ?1",
            params![failed_job, replacement_job, i64::from(decision.ordinal)],
        )
        .map_err(db_err("write flow retry replacement"))?;
        tx.commit()
            .map_err(db_err("commit flow retry replacement"))?;
        Ok(Some(FlowRetryDecision {
            replacement_job: Some(replacement_job.to_owned()),
            replacement_order: Some(decision.ordinal),
            ..decision
        }))
    }

    /// Every decision of one step and input generation, oldest first: the
    /// ordered failed-job history with class, ordinal and outcome of each.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn flow_retry_failures(
        &self,
        need: &str,
        run: u32,
        node: &str,
        task: &str,
        generation: u32,
    ) -> Result<Vec<FlowRetryDecision>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {COLUMNS} FROM flow_retry_decisions
                 WHERE need = ?1 AND run = ?2 AND node = ?3 AND task = ?4 AND generation = ?5
                 ORDER BY rowid"
            ))
            .map_err(db_err("read flow retry failures"))?;
        let rows = stmt
            .query_map(
                params![need, i64::from(run), node, task, i64::from(generation)],
                read_row,
            )
            .map_err(db_err("read flow retry failures"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err("read flow retry failures"))?;
        Ok(rows.into_iter().map(decode).collect())
    }

    /// Allowed decisions of one step and generation whose replacement job was
    /// never recorded: work a caller may still have to finish after a crash
    /// between allocation and enqueue.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn unfinished_flow_retries(
        &self,
        need: &str,
        run: u32,
        node: &str,
        task: &str,
        generation: u32,
    ) -> Result<Vec<FlowRetryDecision>, VivariumError> {
        Ok(self
            .flow_retry_failures(need, run, node, task, generation)?
            .into_iter()
            .filter(|decision| decision.allowed && decision.replacement_job.is_none())
            .collect())
    }
}

#[cfg(test)]
#[path = "flow_retry_test.rs"]
mod tests;
