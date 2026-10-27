//! Durable typed suspensions and reply-resume intents of `pump.db`.
//!
//! A step that cannot proceed can suspend instead of failing. One suspension
//! row records the provenance of that decision: the need and run, the actual
//! origin task with the execution identity and generation that suspended, the
//! accepted outcome word and reason line, the typed
//! [`SuspensionReason`] class, an optional park task, and the input-message
//! checkpoint captured with the origin inputs. Suspensions of one task
//! lineage are numbered from 1; a repark appends the next ordinal and every
//! historical generation stays readable, so repark handling can tell which
//! suspension a reply answers.
//!
//! A reply-resume intent is the durable decision to resume one suspension by
//! redispatching: the stable replacement identity, the complete immutable
//! replacement [`JobSpec`], the generation transition the resume performs,
//! the claimed source messages (each with its id, sender, date and content,
//! claimed from the arrival ledger), and the materialization /
//! completion / cancellation state.
//!
//! Messages enter through the arrival ledger (`store_message`, idempotent on
//! message id: first arrival wins); an intent claims each named message, and
//! a message can be claimed by at most one intent. Intent creation is
//! idempotent on the suspension generation: a replay returns the stored
//! intent unchanged — never a rewrite of its replacement identity, inputs or
//! transition — so replaying intent creation cannot allocate another
//! execution, and a second claim of a consumed message is refused before
//! anything is written.
//!
//! The store never mints execution generations: the caller performs the
//! transition with the execution APIs and records its from/to numbers here.
//! Like the other flow tables, these tables are created with
//! `IF NOT EXISTS` on every open and are not part of the
//! [`State`](crate::pump::state::State) save, so `save()` and `clear()` never
//! touch them and the schema version is not bumped.

use rusqlite::{Connection, OptionalExtension, params};

use super::step_execution::ExecutionKey;
use super::{PumpStore, db_err, from_u32, from_u64, json};
use crate::error::VivariumError;
use crate::pump::jobs::JobSpec;

/// Why a step suspended. The class decides what a reply can do: only a
/// [`SuspensionReason::RoleInputs`] suspension is resumable by a reusable
/// role-input reply; the others record what the step waits on or failed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuspensionReason {
    /// Reusable role-input suspension: the step waits for operator inputs a
    /// reply can carry, and the checkpoint lets a resume replay them.
    RoleInputs,
    /// A dispatch or pool lock is held elsewhere.
    Lock,
    /// The run is held at a milestone gate.
    Milestone,
    /// A dependency or scope wait outside the step's control.
    Wait,
    /// A mechanical failure a redispatch can retry.
    Mechanical,
    /// A permanent environment failure no redispatch can fix.
    Environment,
    /// Uncertain publication: the result may or may not have landed.
    Uncertain,
}

impl SuspensionReason {
    /// The stored label of this reason.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RoleInputs => "role_inputs",
            Self::Lock => "lock",
            Self::Milestone => "milestone",
            Self::Wait => "wait",
            Self::Mechanical => "mechanical",
            Self::Environment => "environment",
            Self::Uncertain => "uncertain",
        }
    }

    /// The reason for a stored label.
    ///
    /// # Errors
    /// Returns [`VivariumError`] for a label no shipped reason uses.
    pub fn parse(text: &str) -> Result<Self, VivariumError> {
        match text {
            "role_inputs" => Ok(Self::RoleInputs),
            "lock" => Ok(Self::Lock),
            "milestone" => Ok(Self::Milestone),
            "wait" => Ok(Self::Wait),
            "mechanical" => Ok(Self::Mechanical),
            "environment" => Ok(Self::Environment),
            "uncertain" => Ok(Self::Uncertain),
            other => Err(VivariumError::Other(format!(
                "pump.db step_suspensions: unknown suspension reason {other:?}"
            ))),
        }
    }
}

/// One suspension to record: the provenance of a step's decision to suspend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suspension {
    pub need: String,
    pub run: u32,
    /// The task that actually suspended.
    pub task: String,
    /// The owning phase of the origin execution.
    pub phase: String,
    /// The execution identity that suspended.
    pub execution: String,
    /// The execution generation at suspension time.
    pub generation: u32,
    pub reason: SuspensionReason,
    /// The accepted outcome word the step reported.
    pub outcome: String,
    /// The reason line the step reported.
    pub reason_detail: String,
    /// The park task holding the run, when one exists.
    pub park_task: Option<String>,
    /// The input-message checkpoint captured with the origin inputs.
    pub checkpoint: Option<String>,
}

/// One stored suspension. `suspension` is the 1-based ordinal of this
/// suspension within the task lineage; a repark appends the next ordinal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspensionRow {
    pub need: String,
    pub run: u32,
    pub task: String,
    pub suspension: u32,
    pub phase: String,
    pub execution: String,
    pub generation: u32,
    pub reason: SuspensionReason,
    pub outcome: String,
    pub reason_detail: String,
    pub park_task: Option<String>,
    pub checkpoint: Option<String>,
}

/// One source message as it arrived: the arrival ledger's ingest record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeMessage {
    /// Stable message identity; redelivery of the same id is a no-op.
    pub message_id: String,
    pub sender: String,
    /// Epoch second of the message date.
    pub sent_at: u64,
    pub content: String,
}

/// One stored source message with its claim state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeMessageRow {
    /// Arrival order within the store; breaks ties between same-second
    /// messages so lookups are deterministic.
    pub seq: u64,
    pub need: String,
    pub run: u32,
    pub task: String,
    pub message_id: String,
    pub sender: String,
    pub sent_at: u64,
    pub content: String,
    /// The replacement job that claimed this message, once claimed.
    pub consumed_by: Option<String>,
    pub consumed_at: Option<u64>,
}

/// One reply-resume intent to record: the decision to resume one suspension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeIntent {
    /// The execution key of the suspended lineage.
    pub key: ExecutionKey,
    /// The suspension generation this intent answers.
    pub suspension: u32,
    /// The execution identity that suspended.
    pub execution: String,
    /// The generation suspended (the transition's from side).
    pub from_generation: u32,
    /// The generation the resume performs (the transition's to side); the
    /// caller mints it with the execution APIs and records it here.
    pub to_generation: u32,
    /// The stable identity of the replacement job.
    pub replacement_job: String,
    /// The complete immutable replacement dispatch input.
    pub spec: JobSpec,
    /// The source messages consumed, by ledger id. Each must have been
    /// stored (`store_message`) and must still be unconsumed.
    pub messages: Vec<String>,
}

/// One stored reply-resume intent with the messages it claimed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeIntentRow {
    pub need: String,
    pub run: u32,
    pub task: String,
    pub phase: String,
    pub suspension: u32,
    pub execution: String,
    pub from_generation: u32,
    pub to_generation: u32,
    pub replacement_job: String,
    pub spec: JobSpec,
    /// The claimed source messages, oldest arrival first.
    pub messages: Vec<ResumeMessageRow>,
    pub materialized_at: Option<u64>,
    pub completed_at: Option<u64>,
    pub cancelled_at: Option<u64>,
}

impl ResumeIntentRow {
    /// True while the intent still owes a redispatch or a result: neither
    /// completed nor cancelled.
    #[must_use]
    pub fn is_incomplete(&self) -> bool {
        self.completed_at.is_none() && self.cancelled_at.is_none()
    }
}

/// The outcome of one intent-creation attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeCreation {
    /// A new intent consumed the named messages.
    Fresh(ResumeIntentRow),
    /// The suspension generation was already answered; the stored intent
    /// answers unchanged, whatever inputs the replay presented.
    Existing(ResumeIntentRow),
}

const DDL: &str = "
CREATE TABLE IF NOT EXISTS step_suspensions (
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    task TEXT NOT NULL,
    suspension INTEGER NOT NULL,
    phase TEXT NOT NULL,
    execution TEXT NOT NULL,
    generation INTEGER NOT NULL,
    reason TEXT NOT NULL,
    outcome TEXT NOT NULL,
    reason_detail TEXT NOT NULL,
    park_task TEXT,
    checkpoint TEXT,
    created_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (need, run, task, suspension)
);
CREATE INDEX IF NOT EXISTS step_suspensions_run ON step_suspensions (need, run);
CREATE TABLE IF NOT EXISTS step_resume_messages (
    message_seq INTEGER PRIMARY KEY AUTOINCREMENT,
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    task TEXT NOT NULL,
    message_id TEXT NOT NULL,
    sender TEXT NOT NULL,
    sent_at INTEGER NOT NULL,
    content TEXT NOT NULL,
    consumed_by TEXT,
    consumed_at INTEGER,
    created_at INTEGER NOT NULL DEFAULT 0,
    UNIQUE (need, run, task, message_id)
);
CREATE INDEX IF NOT EXISTS step_resume_messages_lineage
    ON step_resume_messages (need, run, task, message_seq);
CREATE INDEX IF NOT EXISTS step_resume_messages_consumer
    ON step_resume_messages (consumed_by);
CREATE TABLE IF NOT EXISTS step_resume_intents (
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    task TEXT NOT NULL,
    phase TEXT NOT NULL,
    suspension INTEGER NOT NULL,
    execution TEXT NOT NULL,
    from_generation INTEGER NOT NULL,
    to_generation INTEGER NOT NULL,
    replacement_job TEXT NOT NULL UNIQUE,
    spec_json TEXT NOT NULL,
    materialized_at INTEGER,
    completed_at INTEGER,
    cancelled_at INTEGER,
    created_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (need, run, task, phase, suspension)
);
CREATE INDEX IF NOT EXISTS step_resume_intents_run ON step_resume_intents (need, run);
";

/// Create the suspension, message ledger, and intent tables when missing.
pub(super) fn ensure_tables(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(DDL)
        .map_err(db_err("create step resume tables"))
}

const SUSPENSION_COLUMNS: &str = "need, run, task, suspension, phase, execution, generation, \
     reason, outcome, reason_detail, park_task, checkpoint";

const MESSAGE_COLUMNS: &str =
    "message_seq, need, run, task, message_id, sender, sent_at, content, consumed_by, consumed_at";

const INTENT_COLUMNS: &str = "need, run, task, phase, suspension, execution, from_generation, \
     to_generation, replacement_job, spec_json, materialized_at, completed_at, cancelled_at";

fn now() -> &'static str {
    "CAST(strftime('%s', 'now') AS INTEGER)"
}

fn suspension_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(SuspensionRow, String)> {
    Ok((
        SuspensionRow {
            need: row.get(0)?,
            run: from_u32(row.get(1)?),
            task: row.get(2)?,
            suspension: from_u32(row.get(3)?),
            phase: row.get(4)?,
            execution: row.get(5)?,
            generation: from_u32(row.get(6)?),
            reason: SuspensionReason::RoleInputs,
            outcome: row.get(8)?,
            reason_detail: row.get(9)?,
            park_task: row.get(10)?,
            checkpoint: row.get(11)?,
        },
        row.get(7)?,
    ))
}

fn finish_suspension(
    (mut row, reason): (SuspensionRow, String),
) -> Result<SuspensionRow, VivariumError> {
    row.reason = SuspensionReason::parse(&reason)?;
    Ok(row)
}

fn message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ResumeMessageRow> {
    Ok(ResumeMessageRow {
        seq: from_u64(row.get(0)?),
        need: row.get(1)?,
        run: from_u32(row.get(2)?),
        task: row.get(3)?,
        message_id: row.get(4)?,
        sender: row.get(5)?,
        sent_at: from_u64(row.get(6)?),
        content: row.get(7)?,
        consumed_by: row.get(8)?,
        consumed_at: row.get::<_, Option<i64>>(9)?.map(from_u64),
    })
}

fn intent_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(ResumeIntentRow, String)> {
    Ok((
        ResumeIntentRow {
            need: row.get(0)?,
            run: from_u32(row.get(1)?),
            task: row.get(2)?,
            phase: row.get(3)?,
            suspension: from_u32(row.get(4)?),
            execution: row.get(5)?,
            from_generation: from_u32(row.get(6)?),
            to_generation: from_u32(row.get(7)?),
            replacement_job: row.get(8)?,
            spec: JobSpec::new("", "", "", "", ""),
            messages: Vec::new(),
            materialized_at: row.get::<_, Option<i64>>(10)?.map(from_u64),
            completed_at: row.get::<_, Option<i64>>(11)?.map(from_u64),
            cancelled_at: row.get::<_, Option<i64>>(12)?.map(from_u64),
        },
        row.get(9)?,
    ))
}

fn decode_spec(row: &ResumeIntentRow, spec_json: &str) -> Result<JobSpec, VivariumError> {
    serde_json::from_str(spec_json).map_err(|e| {
        VivariumError::Other(format!(
            "pump.db step_resume_intents: decode spec of {}: {e}",
            row.replacement_job
        ))
    })
}

fn query_suspensions(
    conn: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<SuspensionRow>, VivariumError> {
    let mut stmt = conn.prepare(sql).map_err(db_err("prepare suspensions"))?;
    let rows = stmt
        .query_map(args, suspension_from_row)
        .map_err(db_err("read suspensions"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read suspension row"))?;
    rows.into_iter().map(finish_suspension).collect()
}

fn query_messages(
    conn: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<ResumeMessageRow>, VivariumError> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(db_err("prepare resume messages"))?;
    let rows = stmt
        .query_map(args, message_from_row)
        .map_err(db_err("read resume messages"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read resume message row"))?;
    Ok(rows)
}

fn query_intents(
    conn: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<ResumeIntentRow>, VivariumError> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(db_err("prepare resume intents"))?;
    let rows = stmt
        .query_map(args, intent_from_row)
        .map_err(db_err("read resume intents"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read resume intent row"))?;
    let mut out = Vec::with_capacity(rows.len());
    for (mut row, spec_json) in rows {
        row.spec = decode_spec(&row, &spec_json)?;
        row.messages = intent_messages(conn, &row.replacement_job)?;
        out.push(row);
    }
    Ok(out)
}

/// The messages an intent claimed, oldest arrival first.
fn intent_messages(
    conn: &Connection,
    replacement_job: &str,
) -> Result<Vec<ResumeMessageRow>, VivariumError> {
    query_messages(
        conn,
        &format!(
            "SELECT {MESSAGE_COLUMNS} FROM step_resume_messages \
             WHERE consumed_by = ?1 ORDER BY sent_at, message_seq"
        ),
        params![replacement_job],
    )
}

/// The next suspension ordinal of a task lineage.
fn next_suspension(
    conn: &Connection,
    need: &str,
    run: u32,
    task: &str,
) -> Result<u32, VivariumError> {
    conn.query_row(
        "SELECT COALESCE(MAX(suspension), 0) FROM step_suspensions \
         WHERE need = ?1 AND run = ?2 AND task = ?3",
        params![need, run, task],
        |row| row.get::<_, i64>(0),
    )
    .map(|max| from_u32(max) + 1)
    .map_err(db_err("read suspension ordinal"))
}

fn insert_suspension(conn: &Connection, row: &SuspensionRow) -> Result<(), VivariumError> {
    conn.execute(
        &format!(
            "INSERT INTO step_suspensions \
               ({SUSPENSION_COLUMNS}, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, {})",
            now()
        ),
        params![
            row.need,
            row.run,
            row.task,
            row.suspension,
            row.phase,
            row.execution,
            row.generation,
            row.reason.as_str(),
            row.outcome,
            row.reason_detail,
            row.park_task,
            row.checkpoint,
        ],
    )
    .map_err(db_err("record suspension"))?;
    Ok(())
}

fn insert_intent(conn: &Connection, intent: &ResumeIntent) -> Result<(), VivariumError> {
    conn.execute(
        &format!(
            "INSERT INTO step_resume_intents \
               (need, run, task, phase, suspension, execution, from_generation, \
                to_generation, replacement_job, spec_json, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, {})",
            now()
        ),
        params![
            intent.key.need,
            intent.key.run,
            intent.key.task,
            intent.key.phase,
            intent.suspension,
            intent.execution,
            intent.from_generation,
            intent.to_generation,
            intent.replacement_job,
            json(&intent.spec)?,
        ],
    )
    .map_err(db_err("insert resume intent"))?;
    Ok(())
}

/// Claim one named message for `intent`: it must be a stored, unconsumed
/// message of the intent's lineage. Refusals happen before any write.
fn claim_message(
    conn: &Connection,
    intent: &ResumeIntent,
    message_id: &str,
) -> Result<(), VivariumError> {
    let stored = conn
        .query_row(
            &format!(
                "SELECT {MESSAGE_COLUMNS} FROM step_resume_messages \
                 WHERE need = ?1 AND run = ?2 AND task = ?3 AND message_id = ?4"
            ),
            params![intent.key.need, intent.key.run, intent.key.task, message_id],
            message_from_row,
        )
        .optional()
        .map_err(db_err("read resume message"))?;
    let Some(stored) = stored else {
        return Err(VivariumError::Other(format!(
            "resume message {message_id}: not stored for {} r{} {}",
            intent.key.need, intent.key.run, intent.key.task
        )));
    };
    if let Some(consumer) = stored.consumed_by {
        return Err(VivariumError::Other(format!(
            "resume message {message_id}: already consumed by {consumer}"
        )));
    }
    conn.execute(
        &format!(
            "UPDATE step_resume_messages SET consumed_by = ?5, consumed_at = {} \
             WHERE need = ?1 AND run = ?2 AND task = ?3 AND message_id = ?4 \
               AND consumed_by IS NULL",
            now()
        ),
        params![
            intent.key.need,
            intent.key.run,
            intent.key.task,
            message_id,
            intent.replacement_job,
        ],
    )
    .map_err(db_err("claim resume message"))?;
    Ok(())
}

fn no_such_intent(replacement_job: &str) -> VivariumError {
    VivariumError::Other(format!("resume intent {replacement_job}: no such intent"))
}

impl PumpStore {
    /// Record one suspension of `s`'s task lineage, appending the next
    /// suspension ordinal. Every earlier suspension of the lineage stays
    /// readable, so a repark keeps its history.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn record_suspension(&self, s: &Suspension) -> Result<SuspensionRow, VivariumError> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin suspension"))?;
        let suspension = next_suspension(&tx, &s.need, s.run, &s.task)?;
        let row = SuspensionRow {
            need: s.need.clone(),
            run: s.run,
            task: s.task.clone(),
            suspension,
            phase: s.phase.clone(),
            execution: s.execution.clone(),
            generation: s.generation,
            reason: s.reason,
            outcome: s.outcome.clone(),
            reason_detail: s.reason_detail.clone(),
            park_task: s.park_task.clone(),
            checkpoint: s.checkpoint.clone(),
        };
        insert_suspension(&tx, &row)?;
        tx.commit().map_err(db_err("commit suspension"))?;
        Ok(row)
    }

    /// Every suspension of the task lineage, oldest ordinal first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when a row cannot be read or decoded.
    pub fn suspensions(
        &self,
        need: &str,
        run: u32,
        task: &str,
    ) -> Result<Vec<SuspensionRow>, VivariumError> {
        query_suspensions(
            &self.conn,
            &format!(
                "SELECT {SUSPENSION_COLUMNS} FROM step_suspensions \
                 WHERE need = ?1 AND run = ?2 AND task = ?3 ORDER BY suspension"
            ),
            params![need, run, task],
        )
    }

    /// The lineage's newest suspension, or `None` before the first one.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when a row cannot be read or decoded.
    pub fn latest_suspension(
        &self,
        need: &str,
        run: u32,
        task: &str,
    ) -> Result<Option<SuspensionRow>, VivariumError> {
        Ok(self.suspensions(need, run, task)?.into_iter().next_back())
    }

    /// Store one arrived source message in the ledger. Idempotent on message
    /// id: a redelivery is a no-op and the first arrival wins unchanged.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn store_message(
        &self,
        need: &str,
        run: u32,
        task: &str,
        message: &ResumeMessage,
    ) -> Result<(), VivariumError> {
        self.conn
            .execute(
                &format!(
                    "INSERT INTO step_resume_messages \
                       (need, run, task, message_id, sender, sent_at, content, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, {}) \
                     ON CONFLICT(need, run, task, message_id) DO NOTHING",
                    now()
                ),
                params![
                    need,
                    run,
                    task,
                    message.message_id,
                    message.sender,
                    i64::try_from(message.sent_at).unwrap_or(i64::MAX),
                    message.content,
                ],
            )
            .map_err(db_err("store resume message"))?;
        Ok(())
    }

    /// Every message of the lineage not yet claimed by an intent, oldest
    /// arrival first; same-second messages keep their arrival order.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when a row cannot be read.
    pub fn unconsumed_messages(
        &self,
        need: &str,
        run: u32,
        task: &str,
    ) -> Result<Vec<ResumeMessageRow>, VivariumError> {
        query_messages(
            &self.conn,
            &format!(
                "SELECT {MESSAGE_COLUMNS} FROM step_resume_messages \
                 WHERE need = ?1 AND run = ?2 AND task = ?3 AND consumed_by IS NULL \
                 ORDER BY sent_at, message_seq"
            ),
            params![need, run, task],
        )
    }

    /// Every stored message of the lineage, consumed or not, oldest first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when a row cannot be read.
    pub fn resume_messages(
        &self,
        need: &str,
        run: u32,
        task: &str,
    ) -> Result<Vec<ResumeMessageRow>, VivariumError> {
        query_messages(
            &self.conn,
            &format!(
                "SELECT {MESSAGE_COLUMNS} FROM step_resume_messages \
                 WHERE need = ?1 AND run = ?2 AND task = ?3 ORDER BY sent_at, message_seq"
            ),
            params![need, run, task],
        )
    }

    /// Record the reply-resume intent for one suspension generation.
    ///
    /// Idempotent on `(key, suspension)`: the first call claims the named
    /// messages and inserts the intent transactionally; a replay — whatever
    /// replacement identity, inputs, transition or messages it now presents —
    /// returns the stored intent unchanged and writes nothing. A named
    /// message must already be stored and unconsumed: an unknown id or a
    /// message another intent claimed is refused before anything is written,
    /// so no execution is ever allocated by a replayed or refused creation.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when a message cannot be claimed or a row
    /// cannot be written; `replacement_job` is UNIQUE, so a cross-lineage
    /// collision is a hard error.
    pub fn create_resume_intent(
        &self,
        intent: &ResumeIntent,
    ) -> Result<ResumeCreation, VivariumError> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin resume intent"))?;
        let existing = query_intents(
            &tx,
            &format!(
                "SELECT {INTENT_COLUMNS} FROM step_resume_intents \
                 WHERE need = ?1 AND run = ?2 AND task = ?3 AND phase = ?4 AND suspension = ?5"
            ),
            params![
                intent.key.need,
                intent.key.run,
                intent.key.task,
                intent.key.phase,
                intent.suspension
            ],
        )?;
        if let Some(row) = existing.into_iter().next() {
            return Ok(ResumeCreation::Existing(row));
        }
        for message_id in &intent.messages {
            claim_message(&tx, intent, message_id)?;
        }
        insert_intent(&tx, intent)?;
        tx.commit().map_err(db_err("commit resume intent"))?;
        let row = self
            .resume_intent(&intent.replacement_job)?
            .ok_or_else(|| {
                VivariumError::Other(format!(
                    "resume intent {}: vanished after creation",
                    intent.replacement_job
                ))
            })?;
        Ok(ResumeCreation::Fresh(row))
    }

    /// The intent a replacement identity names, current or settled.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read or decoded.
    pub fn resume_intent(
        &self,
        replacement_job: &str,
    ) -> Result<Option<ResumeIntentRow>, VivariumError> {
        Ok(query_intents(
            &self.conn,
            &format!("SELECT {INTENT_COLUMNS} FROM step_resume_intents WHERE replacement_job = ?1"),
            params![replacement_job],
        )?
        .into_iter()
        .next())
    }

    /// The intent that answered one suspension generation, if any.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read or decoded.
    pub fn resume_intent_for_suspension(
        &self,
        key: &ExecutionKey,
        suspension: u32,
    ) -> Result<Option<ResumeIntentRow>, VivariumError> {
        Ok(query_intents(
            &self.conn,
            &format!(
                "SELECT {INTENT_COLUMNS} FROM step_resume_intents \
                 WHERE need = ?1 AND run = ?2 AND task = ?3 AND phase = ?4 AND suspension = ?5"
            ),
            params![key.need, key.run, key.task, key.phase, suspension],
        )?
        .into_iter()
        .next())
    }

    /// Every intent of the task lineage, oldest suspension first: the
    /// answered-resume history a repark flow replays from.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when a row cannot be read or decoded.
    pub fn resume_intents(
        &self,
        need: &str,
        run: u32,
        task: &str,
    ) -> Result<Vec<ResumeIntentRow>, VivariumError> {
        query_intents(
            &self.conn,
            &format!(
                "SELECT {INTENT_COLUMNS} FROM step_resume_intents \
                 WHERE need = ?1 AND run = ?2 AND task = ?3 ORDER BY suspension"
            ),
            params![need, run, task],
        )
    }

    /// Every intent that still owes a redispatch or a result, creation order.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when a row cannot be read or decoded.
    pub fn incomplete_resume_intents(&self) -> Result<Vec<ResumeIntentRow>, VivariumError> {
        query_intents(
            &self.conn,
            &format!(
                "SELECT {INTENT_COLUMNS} FROM step_resume_intents \
                 WHERE completed_at IS NULL AND cancelled_at IS NULL \
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
    pub fn mark_resume_materialized(
        &self,
        replacement_job: &str,
        at: u64,
    ) -> Result<(), VivariumError> {
        self.stamp_resume_intent(
            replacement_job,
            "materialized_at",
            at,
            "materialize resume intent",
        )
    }

    /// Record that the resume's result was applied: the intent is complete.
    /// Idempotent.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the intent is unknown or the row
    /// cannot be written.
    pub fn complete_resume_intent(
        &self,
        replacement_job: &str,
        at: u64,
    ) -> Result<(), VivariumError> {
        self.stamp_resume_intent(
            replacement_job,
            "completed_at",
            at,
            "complete resume intent",
        )
    }

    /// Record that the intent was cancelled without its redispatch.
    /// Idempotent.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the intent is unknown or the row
    /// cannot be written.
    pub fn cancel_resume_intent(
        &self,
        replacement_job: &str,
        at: u64,
    ) -> Result<(), VivariumError> {
        self.stamp_resume_intent(replacement_job, "cancelled_at", at, "cancel resume intent")
    }

    /// Set one lifecycle stamp, keeping the first value ever written.
    fn stamp_resume_intent(
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
                    "UPDATE step_resume_intents SET {column} = COALESCE({column}, ?2) \
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
#[path = "step_resume_test.rs"]
mod tests;
