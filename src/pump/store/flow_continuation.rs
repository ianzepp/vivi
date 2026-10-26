//! The continuation tables of `pump.db`: the park-task-to-source-task link,
//! the run's thread-input journal, and pending same-run resume decisions.
//!
//! A park step may record which executable task's verdict parked the run.
//! The origin resolver in `flow::continuation` prefers the recorded link and
//! walks the topology for park tasks written before the link existed. On top
//! of the link, the tables journal a run's thread inputs as durable
//! message-identity snapshots: the run's input baseline, the identities each
//! source execution actually included, and the identities resumption has
//! already consumed — the last scoped per source task, so one reply can
//! reach several independently held steps. A resume decision is durable
//! intent, committed by [`PumpStore::observe_resume`] before the caller
//! reopens the mailspace or inserts a replacement job; re-observing the same
//! source and input batch returns the same decision and generation. Message
//! identity is the message id alone: a timestamp is evidence, never a
//! uniqueness key. The tables are created with `IF NOT EXISTS` on every open
//! and are not part of the `State` save, so `save()` and `clear()` never
//! touch them. The schema version is not bumped here.

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::error::VivariumError;

use super::{PumpStore, db_err, from_u32, from_u64};

const CONTINUATION_DDL: &str = "
CREATE TABLE IF NOT EXISTS flow_continuations (
    task TEXT PRIMARY KEY,
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    node TEXT NOT NULL,
    source TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS flow_continuations_run ON flow_continuations (need, run);
CREATE TABLE IF NOT EXISTS flow_run_inputs (
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    ordinal INTEGER NOT NULL,
    message_id TEXT NOT NULL,
    author TEXT NOT NULL,
    body TEXT NOT NULL,
    sent_at INTEGER NOT NULL,
    PRIMARY KEY (need, run, ordinal)
);
CREATE UNIQUE INDEX IF NOT EXISTS flow_run_inputs_identity
    ON flow_run_inputs (need, run, message_id);
CREATE TABLE IF NOT EXISTS flow_source_inputs (
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    node TEXT NOT NULL,
    task TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    message_id TEXT NOT NULL,
    author TEXT NOT NULL,
    body TEXT NOT NULL,
    sent_at INTEGER NOT NULL,
    PRIMARY KEY (need, run, node, task, ordinal)
);
CREATE UNIQUE INDEX IF NOT EXISTS flow_source_inputs_identity
    ON flow_source_inputs (need, run, node, task, message_id);
CREATE TABLE IF NOT EXISTS flow_consumed_inputs (
    task TEXT NOT NULL,
    message_id TEXT NOT NULL,
    PRIMARY KEY (task, message_id)
);
CREATE TABLE IF NOT EXISTS flow_resume_decisions (
    decision TEXT PRIMARY KEY,
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    node TEXT NOT NULL,
    task TEXT NOT NULL,
    park_task TEXT,
    generation INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'applied', 'cancelled')),
    progress INTEGER NOT NULL,
    replacement_id TEXT,
    replacement_order INTEGER
);
CREATE INDEX IF NOT EXISTS flow_resume_decisions_source
    ON flow_resume_decisions (need, run, node, task);
CREATE TABLE IF NOT EXISTS flow_resume_inputs (
    decision TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    message_id TEXT NOT NULL,
    author TEXT NOT NULL,
    body TEXT NOT NULL,
    sent_at INTEGER NOT NULL,
    PRIMARY KEY (decision, ordinal)
);
";

/// Create the continuation tables when they are missing.
pub(super) fn ensure_tables(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(CONTINUATION_DDL)
        .map_err(db_err("create flow continuation table"))
}

/// One park-task-to-source-task link of one need and run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationLink {
    /// The park step task the link belongs to.
    pub task: String,
    /// The need of the run both steps belong to.
    pub need: String,
    /// The run both steps belong to.
    pub run: u32,
    /// The park step's node id.
    pub node: String,
    /// The executable source task the park continued from.
    pub source: String,
}

/// One thread input of a run: a durable message-identity snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadInput {
    /// The stable identity of the message: the only identity there is.
    pub message_id: String,
    /// Who authored the message.
    pub author: String,
    /// The message body at observation time.
    pub body: String,
    /// When the message was sent (unix seconds). Evidence only: two distinct
    /// messages may share one, and it is never part of any key.
    pub sent_at: u64,
}

/// One observed same-run resume: a held source step and the ordered batch of
/// thread inputs it should resume with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeRequest {
    /// The need of the run both steps belong to.
    pub need: String,
    /// The run both steps belong to.
    pub run: u32,
    /// The source step's node id.
    pub node: String,
    /// The executable source task that will resume.
    pub task: String,
    /// The park task the resume continues, when it is not the source task.
    pub park_task: Option<String>,
    /// The ordered batch of new inputs, in thread order.
    pub inputs: Vec<ThreadInput>,
}

/// The lifecycle state of a resume decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeState {
    /// Recorded intent, not yet applied: recoverable work.
    Pending,
    /// Applied in full: the source task has consumed every input.
    Applied,
    /// Withdrawn without ever being applied.
    Cancelled,
}

impl ResumeState {
    fn as_str(self) -> &'static str {
        match self {
            ResumeState::Pending => "pending",
            ResumeState::Applied => "applied",
            ResumeState::Cancelled => "cancelled",
        }
    }

    fn of(text: &str) -> Option<ResumeState> {
        match text {
            "pending" => Some(ResumeState::Pending),
            "applied" => Some(ResumeState::Applied),
            "cancelled" => Some(ResumeState::Cancelled),
            _ => None,
        }
    }
}

/// One recorded resume decision: durable intent to resume a held source step
/// with an ordered batch of thread inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeDecision {
    /// The stable decision identity, derived from the source and the batch.
    pub decision: String,
    /// The need of the run both steps belong to.
    pub need: String,
    /// The run both steps belong to.
    pub run: u32,
    /// The source step's node id.
    pub node: String,
    /// The executable source task that will resume.
    pub task: String,
    /// The park task the resume continues, when it is not the source task.
    pub park_task: Option<String>,
    /// The new input generation this decision stands for: the k-th decision
    /// of one source task is generation `k`.
    pub generation: u32,
    /// The lifecycle state of the decision.
    pub state: ResumeState,
    /// How many of the decision's inputs the replacement already applied.
    pub progress: u32,
    /// The replacement job identity, when it is already known.
    pub replacement_id: Option<String>,
    /// The replacement's order among the replacements of the source;
    /// present exactly when `replacement_id` is.
    pub replacement_order: Option<u32>,
    /// The ordered batch of inputs the source resumes with.
    pub inputs: Vec<ThreadInput>,
}

const LINK_COLUMNS: &str = "task, need, run, node, source";
const DECISION_COLUMNS: &str = "decision, need, run, node, task, park_task, generation, state, progress, \
     replacement_id, replacement_order";

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContinuationLink> {
    Ok(ContinuationLink {
        task: row.get(0)?,
        need: row.get(1)?,
        run: row.get(2)?,
        node: row.get(3)?,
        source: row.get(4)?,
    })
}

type DecisionRow = (
    String,
    String,
    i64,
    String,
    String,
    Option<String>,
    i64,
    String,
    i64,
    Option<String>,
    Option<i64>,
);

fn read_decision_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ResumeDecision> {
    let raw: DecisionRow = (
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
    );
    decode_decision(raw)
}

fn decode_decision(raw: DecisionRow) -> rusqlite::Result<ResumeDecision> {
    let (
        decision,
        need,
        run,
        node,
        task,
        park_task,
        generation,
        state_text,
        progress,
        replacement_id,
        replacement_order,
    ) = raw;
    let state = ResumeState::of(&state_text).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            7,
            rusqlite::types::Type::Text,
            format!("unknown resume state {state_text}").into(),
        )
    })?;
    Ok(ResumeDecision {
        decision,
        need,
        run: from_u32(run),
        node,
        task,
        park_task,
        generation: from_u32(generation),
        state,
        progress: from_u32(progress),
        replacement_id,
        replacement_order: replacement_order.map(from_u32),
        inputs: Vec::new(),
    })
}

/// The ordered inputs of one decision.
fn inputs_of(conn: &Connection, decision: &str) -> Result<Vec<ThreadInput>, VivariumError> {
    read_inputs(
        conn,
        "SELECT ordinal, message_id, author, body, sent_at
         FROM flow_resume_inputs WHERE decision = ?1 ORDER BY ordinal",
        [decision],
        "read resume inputs",
    )
}

/// Read thread inputs, in the order the statement produces them.
fn read_inputs<P: rusqlite::Params>(
    conn: &Connection,
    sql: &str,
    params: P,
    what: &'static str,
) -> Result<Vec<ThreadInput>, VivariumError> {
    let mut stmt = conn.prepare(sql).map_err(db_err(what))?;
    let rows = stmt
        .query_map(params, |row| {
            Ok(ThreadInput {
                message_id: row.get(1)?,
                author: row.get(2)?,
                body: row.get(3)?,
                sent_at: from_u64(row.get(4)?),
            })
        })
        .map_err(db_err(what))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err(what))?;
    Ok(rows)
}

/// Append unseen identities in order; the first snapshot of an identity wins.
fn append_run_inputs(
    conn: &Connection,
    need: &str,
    run: u32,
    inputs: &[ThreadInput],
) -> Result<(), VivariumError> {
    let mut base: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(ordinal), 0) FROM flow_run_inputs WHERE need = ?1 AND run = ?2",
            params![need, i64::from(run)],
            |row| row.get(0),
        )
        .map_err(db_err("read run input ordinals"))?;
    for input in inputs {
        base += 1;
        conn.execute(
            "INSERT OR IGNORE INTO flow_run_inputs
                 (need, run, ordinal, message_id, author, body, sent_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                need,
                i64::from(run),
                base,
                input.message_id,
                input.author,
                input.body,
                i64::try_from(input.sent_at).unwrap_or(0)
            ],
        )
        .map_err(db_err("write run input"))?;
    }
    Ok(())
}

/// Append unseen identities of one source execution in order; the first
/// snapshot of an identity wins.
fn append_source_inputs(
    conn: &Connection,
    need: &str,
    run: u32,
    node: &str,
    task: &str,
    inputs: &[ThreadInput],
) -> Result<(), VivariumError> {
    let mut base: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(ordinal), 0) FROM flow_source_inputs
             WHERE need = ?1 AND run = ?2 AND node = ?3 AND task = ?4",
            params![need, i64::from(run), node, task],
            |row| row.get(0),
        )
        .map_err(db_err("read source input ordinals"))?;
    for input in inputs {
        base += 1;
        conn.execute(
            "INSERT OR IGNORE INTO flow_source_inputs
                 (need, run, node, task, ordinal, message_id, author, body, sent_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                need,
                i64::from(run),
                node,
                task,
                base,
                input.message_id,
                input.author,
                input.body,
                i64::try_from(input.sent_at).unwrap_or(0)
            ],
        )
        .map_err(db_err("write source input"))?;
    }
    Ok(())
}

/// The stable decision identity of one source and input batch: a digest of
/// the source coordinates and the ordered message ids. Never a timestamp.
fn decision_key(req: &ResumeRequest) -> String {
    let mut hasher = Sha256::new();
    hasher.update(req.need.as_bytes());
    hasher.update([0]);
    hasher.update(req.run.to_le_bytes());
    hasher.update([0]);
    hasher.update(req.node.as_bytes());
    hasher.update([0]);
    hasher.update(req.task.as_bytes());
    for input in &req.inputs {
        hasher.update([0]);
        hasher.update(input.message_id.as_bytes());
    }
    format!("resume-{}", hex::encode(hasher.finalize()))
}

/// The generation the next decision of one source task gets: one past every
/// decision the source already has, whatever their state.
fn next_generation(conn: &Connection, req: &ResumeRequest) -> Result<u32, VivariumError> {
    conn.query_row(
        "SELECT COUNT(*) FROM flow_resume_decisions
         WHERE need = ?1 AND run = ?2 AND node = ?3 AND task = ?4",
        params![req.need, i64::from(req.run), req.node, req.task],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| from_u32(count) + 1)
    .map_err(db_err("count resume decisions"))
}

/// One decision row with its ordered inputs, when the decision exists.
fn decision_with_inputs(
    conn: &Connection,
    decision: &str,
) -> Result<Option<ResumeDecision>, VivariumError> {
    let raw = conn
        .query_row(
            &format!("SELECT {DECISION_COLUMNS} FROM flow_resume_decisions WHERE decision = ?1"),
            [decision],
            read_decision_row,
        )
        .optional()
        .map_err(db_err("read resume decision"))?;
    let Some(mut found) = raw else {
        return Ok(None);
    };
    found.inputs = inputs_of(conn, decision)?;
    Ok(Some(found))
}

/// Write the pending decision and its ordered input batch.
fn insert_decision(conn: &Connection, pending: &ResumeDecision) -> Result<(), VivariumError> {
    conn.execute(
        &format!(
            "INSERT INTO flow_resume_decisions ({DECISION_COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
        ),
        params![
            pending.decision,
            pending.need,
            i64::from(pending.run),
            pending.node,
            pending.task,
            pending.park_task,
            i64::from(pending.generation),
            pending.state.as_str(),
            i64::from(pending.progress),
            pending.replacement_id,
            pending.replacement_order.map(i64::from)
        ],
    )
    .map_err(db_err("write resume decision"))?;
    insert_decision_inputs(conn, pending)
}

/// Write the ordered input batch of one decision.
fn insert_decision_inputs(
    conn: &Connection,
    pending: &ResumeDecision,
) -> Result<(), VivariumError> {
    for (index, input) in pending.inputs.iter().enumerate() {
        conn.execute(
            "INSERT INTO flow_resume_inputs
                 (decision, ordinal, message_id, author, body, sent_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                pending.decision,
                i64::try_from(index + 1).unwrap_or(0),
                input.message_id,
                input.author,
                input.body,
                i64::try_from(input.sent_at).unwrap_or(0)
            ],
        )
        .map_err(db_err("write resume input"))?;
    }
    Ok(())
}

/// Record the identities a source task consumed through a completed resume.
fn mark_consumed(
    conn: &Connection,
    task: &str,
    inputs: &[ThreadInput],
) -> Result<(), VivariumError> {
    for input in inputs {
        conn.execute(
            "INSERT OR IGNORE INTO flow_consumed_inputs (task, message_id) VALUES (?1, ?2)",
            params![task, input.message_id],
        )
        .map_err(db_err("write consumed input"))?;
    }
    Ok(())
}

/// A repeated write of the identical link is a no-op; any other second link
/// for the same park task is refused.
fn finish_repeat(current: &ContinuationLink, link: &ContinuationLink) -> Result<(), VivariumError> {
    if current == link {
        return Ok(());
    }
    Err(VivariumError::Other(format!(
        "park task {}: already linked to source {}; refusing a link to {}",
        link.task, current.source, link.source
    )))
}

impl PumpStore {
    /// Record the continuation link of one park step task. Writing the same
    /// link again succeeds unchanged; a second, different source for a park
    /// task that already has one is refused. A link is only valid when both
    /// of its steps sit in `flow_steps` under the link's need and run, and
    /// the link's node is the park step's own node.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when validation fails or the row cannot be
    /// written.
    pub fn link_continuation(&self, link: &ContinuationLink) -> Result<(), VivariumError> {
        self.validate_link(link)?;
        if let Some(current) = self.continuation_of(&link.task)? {
            return finish_repeat(&current, link);
        }
        self.conn
            .execute(
                &format!(
                    "INSERT INTO flow_continuations ({LINK_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5)"
                ),
                params![link.task, link.need, link.run, link.node, link.source],
            )
            .map_err(db_err("write flow continuation"))?;
        Ok(())
    }

    /// The continuation link recorded for one park step task, when one is.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn continuation_of(&self, task: &str) -> Result<Option<ContinuationLink>, VivariumError> {
        self.conn
            .query_row(
                &format!("SELECT {LINK_COLUMNS} FROM flow_continuations WHERE task = ?1"),
                [task],
                read_row,
            )
            .optional()
            .map_err(db_err("read flow continuation"))
    }

    /// Journal the run's input baseline: appends every identity the run has
    /// not journalled yet, in the given order. Re-observing an identity is a
    /// no-op that keeps the first snapshot, so the journal stays the durable
    /// record of what the run has been shown.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the transaction or any write fails.
    pub fn journal_run_inputs(
        &self,
        need: &str,
        run: u32,
        inputs: &[ThreadInput],
    ) -> Result<(), VivariumError> {
        if inputs.is_empty() {
            return Ok(());
        }
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin run input journal"))?;
        append_run_inputs(&tx, need, run, inputs)?;
        tx.commit().map_err(db_err("commit run input journal"))
    }

    /// The run's journalled inputs, in journal order.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn run_inputs(&self, need: &str, run: u32) -> Result<Vec<ThreadInput>, VivariumError> {
        read_inputs(
            &self.conn,
            "SELECT ordinal, message_id, author, body, sent_at
             FROM flow_run_inputs WHERE need = ?1 AND run = ?2 ORDER BY ordinal",
            params![need, i64::from(run)],
            "read run inputs",
        )
    }

    /// Record the identities one source execution actually included, in
    /// thread order. Re-recording an identity is a no-op that keeps the
    /// first snapshot.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the transaction or any write fails.
    pub fn record_source_inputs(
        &self,
        need: &str,
        run: u32,
        node: &str,
        task: &str,
        inputs: &[ThreadInput],
    ) -> Result<(), VivariumError> {
        if inputs.is_empty() {
            return Ok(());
        }
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin source input record"))?;
        append_source_inputs(&tx, need, run, node, task, inputs)?;
        tx.commit().map_err(db_err("commit source input record"))
    }

    /// The identities one source execution actually included, in order.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn source_inputs(
        &self,
        need: &str,
        run: u32,
        node: &str,
        task: &str,
    ) -> Result<Vec<ThreadInput>, VivariumError> {
        read_inputs(
            &self.conn,
            "SELECT ordinal, message_id, author, body, sent_at
             FROM flow_source_inputs
             WHERE need = ?1 AND run = ?2 AND node = ?3 AND task = ?4
             ORDER BY ordinal",
            params![need, i64::from(run), node, task],
            "read source inputs",
        )
    }

    /// Whether the source task has already consumed a message identity
    /// through a completed resume.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn input_consumed(&self, task: &str, message_id: &str) -> Result<bool, VivariumError> {
        self.conn
            .query_row(
                "SELECT 1 FROM flow_consumed_inputs WHERE task = ?1 AND message_id = ?2",
                params![task, message_id],
                |_| Ok(()),
            )
            .optional()
            .map(|found| found.is_some())
            .map_err(db_err("read consumed input"))
    }

    /// The message identities the source task has consumed through completed
    /// resumes.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn consumed_inputs(&self, task: &str) -> Result<Vec<String>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare("SELECT message_id FROM flow_consumed_inputs WHERE task = ?1 ORDER BY rowid")
            .map_err(db_err("read consumed inputs"))?;
        let rows = stmt
            .query_map([task], |row| row.get(0))
            .map_err(db_err("read consumed inputs"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err("read consumed inputs"))?;
        Ok(rows)
    }

    /// Observe one same-run resume: write the decision durably — before the
    /// caller reopens the mailspace or inserts a replacement job — and
    /// return it. Observing the same source and input batch again returns
    /// the stored decision unchanged, whatever its state, so the decision
    /// and its generation are stable. The k-th decision of one source task
    /// is generation `k`. An empty batch resumes nothing and is refused.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the batch is empty or the transaction
    /// or any write fails.
    pub fn observe_resume(&self, req: &ResumeRequest) -> Result<ResumeDecision, VivariumError> {
        if req.inputs.is_empty() {
            return Err(VivariumError::Other(format!(
                "resume of task {} ({} r{}): an empty input batch resumes nothing",
                req.task, req.need, req.run
            )));
        }
        let key = decision_key(req);
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin resume observation"))?;
        if let Some(found) = decision_with_inputs(&tx, &key)? {
            return Ok(found);
        }
        let pending = ResumeDecision {
            generation: next_generation(&tx, req)?,
            decision: key,
            need: req.need.clone(),
            run: req.run,
            node: req.node.clone(),
            task: req.task.clone(),
            park_task: req.park_task.clone(),
            state: ResumeState::Pending,
            progress: 0,
            replacement_id: None,
            replacement_order: None,
            inputs: req.inputs.clone(),
        };
        insert_decision(&tx, &pending)?;
        tx.commit().map_err(db_err("commit resume observation"))?;
        Ok(pending)
    }

    /// Every pending decision, oldest source first: the recoverable work a
    /// caller may still have to finish after a crash between observation and
    /// application.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn pending_resumes(&self) -> Result<Vec<ResumeDecision>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {DECISION_COLUMNS} FROM flow_resume_decisions
                 WHERE state = 'pending'
                 ORDER BY need, run, node, task, generation"
            ))
            .map_err(db_err("read pending resumes"))?;
        let rows = stmt
            .query_map([], read_decision_row)
            .map_err(db_err("read pending resumes"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err("read pending resumes"))?;
        let mut found = Vec::new();
        for mut decision in rows {
            decision.inputs = inputs_of(&self.conn, &decision.decision)?;
            found.push(decision);
        }
        Ok(found)
    }

    /// One recorded decision with its inputs, when it exists: also the
    /// recovery read for a caller that crashed mid-application.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn resume_decision(&self, decision: &str) -> Result<Option<ResumeDecision>, VivariumError> {
        decision_with_inputs(&self.conn, decision)
    }

    /// Attach the stable replacement identity and order to a pending
    /// decision, finishing a partially applied one. Returns the updated
    /// decision, or `None` when there is no such decision.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the decision is not pending or the
    /// transaction or any write fails.
    pub fn set_resume_replacement(
        &self,
        decision: &str,
        replacement_id: &str,
        replacement_order: u32,
    ) -> Result<Option<ResumeDecision>, VivariumError> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin resume replacement"))?;
        let Some(mut found) = decision_with_inputs(&tx, decision)? else {
            return Ok(None);
        };
        if found.state != ResumeState::Pending {
            return Err(VivariumError::Other(format!(
                "resume decision {decision}: {}, refusing a replacement",
                found.state.as_str()
            )));
        }
        tx.execute(
            "UPDATE flow_resume_decisions
             SET replacement_id = ?2, replacement_order = ?3 WHERE decision = ?1",
            params![decision, replacement_id, i64::from(replacement_order)],
        )
        .map_err(db_err("write resume replacement"))?;
        found.replacement_id = Some(replacement_id.to_owned());
        found.replacement_order = Some(replacement_order);
        tx.commit().map_err(db_err("commit resume replacement"))?;
        Ok(Some(found))
    }

    /// Record the recoverable application progress of a pending decision:
    /// how many of its inputs the replacement has already applied. Returns
    /// the updated decision, or `None` when there is no such decision.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the decision is not pending, the
    /// progress passes the batch, or the write fails.
    pub fn set_resume_progress(
        &self,
        decision: &str,
        applied: u32,
    ) -> Result<Option<ResumeDecision>, VivariumError> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin resume progress"))?;
        let Some(mut found) = decision_with_inputs(&tx, decision)? else {
            return Ok(None);
        };
        if found.state != ResumeState::Pending {
            return Err(VivariumError::Other(format!(
                "resume decision {decision}: {}, refusing progress",
                found.state.as_str()
            )));
        }
        if usize::try_from(applied).is_ok_and(|count| count > found.inputs.len()) {
            return Err(VivariumError::Other(format!(
                "resume decision {decision}: progress {applied} passes its {} inputs",
                found.inputs.len()
            )));
        }
        tx.execute(
            "UPDATE flow_resume_decisions SET progress = ?2 WHERE decision = ?1",
            params![decision, i64::from(applied)],
        )
        .map_err(db_err("write resume progress"))?;
        found.progress = applied;
        tx.commit().map_err(db_err("commit resume progress"))?;
        Ok(Some(found))
    }

    /// Complete a pending decision: it becomes applied and its inputs are
    /// consumed by the source task, in the same transaction. Completing an
    /// applied decision again is a no-op; a cancelled decision is refused.
    /// Returns the updated decision, or `None` when there is no such
    /// decision.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the decision is cancelled or the
    /// transaction or any write fails.
    pub fn complete_resume(&self, decision: &str) -> Result<Option<ResumeDecision>, VivariumError> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin resume completion"))?;
        let Some(mut found) = decision_with_inputs(&tx, decision)? else {
            return Ok(None);
        };
        match found.state {
            ResumeState::Applied => return Ok(Some(found)),
            ResumeState::Cancelled => {
                return Err(VivariumError::Other(format!(
                    "resume decision {decision}: cancelled, refusing to complete"
                )));
            }
            ResumeState::Pending => {}
        }
        tx.execute(
            "UPDATE flow_resume_decisions SET state = 'applied' WHERE decision = ?1",
            [decision],
        )
        .map_err(db_err("write resume completion"))?;
        mark_consumed(&tx, &found.task, &found.inputs)?;
        found.state = ResumeState::Applied;
        tx.commit().map_err(db_err("commit resume completion"))?;
        Ok(Some(found))
    }

    /// Cancel a pending decision without ever applying it. Cancelling a
    /// cancelled decision again is a no-op; an applied decision is refused.
    /// Returns the updated decision, or `None` when there is no such
    /// decision.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the decision is applied or the
    /// transaction or any write fails.
    pub fn cancel_resume(&self, decision: &str) -> Result<Option<ResumeDecision>, VivariumError> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin resume cancellation"))?;
        let Some(mut found) = decision_with_inputs(&tx, decision)? else {
            return Ok(None);
        };
        match found.state {
            ResumeState::Cancelled => return Ok(Some(found)),
            ResumeState::Applied => {
                return Err(VivariumError::Other(format!(
                    "resume decision {decision}: applied, refusing to cancel"
                )));
            }
            ResumeState::Pending => {}
        }
        tx.execute(
            "UPDATE flow_resume_decisions SET state = 'cancelled' WHERE decision = ?1",
            [decision],
        )
        .map_err(db_err("write resume cancellation"))?;
        found.state = ResumeState::Cancelled;
        tx.commit().map_err(db_err("commit resume cancellation"))?;
        Ok(Some(found))
    }

    /// Both steps of a link must sit in `flow_steps` under the link's need
    /// and run, and the link's node must be the park step's own node.
    fn validate_link(&self, link: &ContinuationLink) -> Result<(), VivariumError> {
        match self.step_place(&link.task)? {
            Some((need, run, node)) if need == link.need && run == link.run => {
                if node != link.node {
                    return Err(VivariumError::Other(format!(
                        "continuation link: park task {} sits at node {node}, not {}",
                        link.task, link.node
                    )));
                }
            }
            _ => {
                return Err(VivariumError::Other(format!(
                    "continuation link: park task {} is not a step of need {} r{}",
                    link.task, link.need, link.run
                )));
            }
        }
        self.check_source(link)
    }

    /// The source step must also sit in the same need and run.
    fn check_source(&self, link: &ContinuationLink) -> Result<(), VivariumError> {
        let ok = self
            .step_place(&link.source)?
            .is_some_and(|(need, run, _)| need == link.need && run == link.run);
        if ok {
            return Ok(());
        }
        Err(VivariumError::Other(format!(
            "continuation link: source {} is not a step of need {} r{}",
            link.source, link.need, link.run
        )))
    }

    /// Where a step sits: `(need, run, node)`, or `None` for no such step.
    fn step_place(&self, task: &str) -> Result<Option<(String, u32, String)>, VivariumError> {
        self.conn
            .query_row(
                "SELECT need, run, node FROM flow_steps WHERE task = ?1",
                [task],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(db_err("read flow step place"))
    }
}

#[cfg(test)]
#[path = "flow_continuation_test.rs"]
mod tests;
