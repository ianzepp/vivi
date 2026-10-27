//! Parked flow origins and their continuation journals of `pump.db`.
//!
//! A flow run can park an origin task at a gate while it waits for input.
//! The pause freezes the exact boundary: which gate the origin stopped at,
//! which need/run/node it belongs to, which pause and input generation it is,
//! which role request started it, the input message the wait starts from,
//! and which replies it has already consumed. A continuation journal freezes
//! how the park ends: the selected replies, the parked answer, and the
//! deterministic replacement request and attempt that carry them.
//!
//! Replay-safe by the same discipline as the recovery intents: recording a
//! pause that is already recorded adopts the stored boundary unchanged,
//! reserving a continuation that is already reserved returns the stored
//! journal, and the replay stages of a journal stamp once. One reply may
//! serve several independently paused origins, but never the same origin
//! twice.
//!
//! The tables sit beside the v8 schema like the recovery tables: created
//! with `IF NOT EXISTS` on every open and never touched by the `State` save
//! or clear, so no schema version moves and no migration is needed.

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::VivariumError;
use crate::pump::jobs::JobSpec;

use super::{PumpStore, db_err, from_u32, from_u64, recovery_state, stamp};

/// The lifecycle of a pause boundary.
pub mod pause_state {
    /// Parked; the origin waits at its gate.
    pub const OPEN: &str = "open";
    /// The park ended; the origin moved on.
    pub const RETIRED: &str = "retired";

    /// Every state word, for error text.
    pub const ALL: &[&str] = &[OPEN, RETIRED];

    /// Whether `word` is in the vocabulary.
    #[must_use]
    pub fn is_known(word: &str) -> bool {
        ALL.contains(&word)
    }
}

/// The replay stages of a continuation journal. Each stage stamps once, so
/// replaying reopen, gate retirement and enqueue can never repeat work.
pub mod journal_stage {
    /// The parked origin was reopened from the journal.
    pub const REOPENED: &str = "reopened";
    /// The park gate was retired.
    pub const GATE_RETIRED: &str = "gate-retired";
    /// The replacement request was enqueued.
    pub const ENQUEUED: &str = "enqueued";

    /// Every stage word, for error text.
    pub const ALL: &[&str] = &[REOPENED, GATE_RETIRED, ENQUEUED];

    /// Whether `word` is in the vocabulary.
    #[must_use]
    pub fn is_known(word: &str) -> bool {
        ALL.contains(&word)
    }
}

/// One frozen pause boundary: an origin parked at its gate.
#[derive(Debug, Clone, PartialEq)]
pub struct PauseBoundaryRow {
    /// The need whose flow run parked.
    pub need: String,
    /// The flow run that parked.
    pub run: u32,
    /// The flow node that parked.
    pub node: String,
    /// The origin task that owns the park.
    pub origin_task: String,
    /// The park gate the origin stopped at.
    pub gate: String,
    /// Which pause of this origin this is: 1 for the first.
    pub pause_generation: u32,
    /// The input generation the pause parked on.
    pub input_generation: u32,
    /// The identity of the role request that started the origin.
    pub role_request: String,
    /// The input message boundary, stamped by [`PumpStore::snapshot_input`].
    pub boundary_message: Option<String>,
    /// Absolute unix-second stamp of the pause.
    pub paused_at: u64,
    /// Lifecycle state; see [`pause_state`].
    pub state: String,
}

/// One reply a continuation journal selected.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PauseJournalReply {
    /// The reply's message id.
    pub id: String,
    /// Who wrote the reply.
    pub author: String,
    /// The reply body.
    pub body: String,
}

/// A continuation to freeze: the deterministic replacement of a parked wait.
#[derive(Debug, Clone, PartialEq)]
pub struct PauseContinuation {
    /// The deterministic identity of the replacement role request.
    pub replacement_request: String,
    /// The fresh transient input identity this continuation supplies.
    pub input_message: String,
    /// The parked answer the continuation carries.
    pub answers: serde_json::Value,
    /// The replies selected for the continuation.
    pub replies: Vec<PauseJournalReply>,
    /// The complete deterministic replacement request.
    pub request: JobSpec,
    /// Which attempt of the replacement this is: 1 for the first.
    pub attempt: u32,
}

/// One frozen continuation journal.
#[derive(Debug, Clone, PartialEq)]
pub struct PauseJournalRow {
    /// The deterministic identity of the replacement role request.
    pub replacement_request: String,
    /// The need whose flow run parked.
    pub need: String,
    /// The flow run that parked.
    pub run: u32,
    /// The flow node that parked.
    pub node: String,
    /// The origin task that owns the park.
    pub origin_task: String,
    /// The park gate the origin stopped at; copied from the boundary.
    pub gate: String,
    /// The pause generation the journal continues.
    pub pause_generation: u32,
    /// The input generation the pause parked on; copied from the boundary.
    pub input_generation: u32,
    /// The identity of the role request that started the origin.
    pub role_request: String,
    /// The input message boundary the continuation starts from.
    pub boundary_message: String,
    /// The fresh transient input identity the continuation supplies.
    pub input_message: String,
    /// The parked answer the continuation carries.
    pub answers: serde_json::Value,
    /// The replies selected for the continuation.
    pub replies: Vec<PauseJournalReply>,
    /// The complete deterministic replacement request.
    pub request: JobSpec,
    /// Which attempt of the replacement this is: 1 for the first.
    pub attempt: u32,
    /// Replay/application state; see [`recovery_state`].
    pub state: String,
    /// The pause's stamp; copied from the boundary.
    pub paused_at: u64,
    /// When the origin was reopened from this journal, if it was.
    pub reopened_at: Option<u64>,
    /// When the park gate was retired, if it was.
    pub gate_retired_at: Option<u64>,
    /// When the replacement request was enqueued, if it was.
    pub enqueued_at: Option<u64>,
    /// When the journal was applied, if it was.
    pub applied_at: Option<u64>,
}

/// What [`PumpStore::invalidate_run_recovery`] removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunRecoveryInvalidation {
    /// Pending transient recovery intents of the run.
    pub intents: usize,
    /// Open pause boundaries of the run.
    pub boundaries: usize,
    /// Consumed reply identities of the run.
    pub replies: usize,
    /// Pending continuation journals of the run.
    pub journals: usize,
}

const DDL: &str = "
CREATE TABLE IF NOT EXISTS pause_boundaries (
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    node TEXT NOT NULL,
    origin_task TEXT NOT NULL,
    pause_generation INTEGER NOT NULL,
    gate TEXT NOT NULL,
    input_generation INTEGER NOT NULL,
    role_request TEXT NOT NULL,
    boundary_message TEXT,
    paused_at INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'open',
    PRIMARY KEY (need, run, node, origin_task, pause_generation)
);
CREATE INDEX IF NOT EXISTS pause_boundaries_run ON pause_boundaries (need, run);

CREATE TABLE IF NOT EXISTS pause_replies (
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    node TEXT NOT NULL,
    origin_task TEXT NOT NULL,
    reply_id TEXT NOT NULL,
    PRIMARY KEY (need, run, node, origin_task, reply_id)
);

CREATE TABLE IF NOT EXISTS pause_journals (
    replacement_request TEXT PRIMARY KEY,
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    node TEXT NOT NULL,
    origin_task TEXT NOT NULL,
    pause_generation INTEGER NOT NULL,
    gate TEXT NOT NULL,
    input_generation INTEGER NOT NULL,
    role_request TEXT NOT NULL,
    boundary_message TEXT NOT NULL,
    input_message TEXT NOT NULL,
    paused_at INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',
    attempt INTEGER NOT NULL,
    answers_json TEXT NOT NULL,
    replies_json TEXT NOT NULL,
    request_json TEXT NOT NULL,
    reopened_at INTEGER,
    gate_retired_at INTEGER,
    enqueued_at INTEGER,
    applied_at INTEGER
);
CREATE INDEX IF NOT EXISTS pause_journals_run ON pause_journals (need, run, state);
";

const BOUNDARY_COLUMNS: &str = "need, run, node, origin_task, pause_generation, gate, \
     input_generation, role_request, boundary_message, paused_at, state";

const JOURNAL_COLUMNS: &str = "replacement_request, need, run, node, origin_task, \
     pause_generation, gate, input_generation, role_request, boundary_message, \
     input_message, paused_at, state, attempt, answers_json, replies_json, request_json, \
     reopened_at, gate_retired_at, enqueued_at, applied_at";

/// Create the pause tables when they are missing; reached by the recovery
/// ensure-tables hook on every open.
pub(super) fn ensure_tables(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(DDL)
        .map_err(db_err("create pause tables"))
}

/// Read one boundary result row.
fn boundary_row(row: &Row<'_>) -> rusqlite::Result<PauseBoundaryRow> {
    Ok(PauseBoundaryRow {
        need: row.get(0)?,
        run: row.get::<_, i64>(1).map(from_u32)?,
        node: row.get(2)?,
        origin_task: row.get(3)?,
        pause_generation: row.get::<_, i64>(4).map(from_u32)?,
        gate: row.get(5)?,
        input_generation: row.get::<_, i64>(6).map(from_u32)?,
        role_request: row.get(7)?,
        boundary_message: row.get(8)?,
        paused_at: row.get::<_, i64>(9).map(from_u64)?,
        state: row.get(10)?,
    })
}

/// A journal's undecoded JSON columns and stamps.
struct JournalRaw {
    answers: String,
    replies: String,
    request: String,
    reopened_at: Option<i64>,
    gate_retired_at: Option<i64>,
    enqueued_at: Option<i64>,
    applied_at: Option<i64>,
}

/// Read one journal result row into its core and its raw columns;
/// [`decode_journal`] finishes it.
fn journal_raw_row(row: &Row<'_>) -> rusqlite::Result<(PauseJournalRow, JournalRaw)> {
    Ok((
        PauseJournalRow {
            replacement_request: row.get(0)?,
            need: row.get(1)?,
            run: row.get::<_, i64>(2).map(from_u32)?,
            node: row.get(3)?,
            origin_task: row.get(4)?,
            pause_generation: row.get::<_, i64>(5).map(from_u32)?,
            gate: row.get(6)?,
            input_generation: row.get::<_, i64>(7).map(from_u32)?,
            role_request: row.get(8)?,
            boundary_message: row.get(9)?,
            input_message: row.get(10)?,
            paused_at: row.get::<_, i64>(11).map(from_u64)?,
            state: row.get(12)?,
            attempt: row.get::<_, i64>(13).map(from_u32)?,
            answers: serde_json::Value::Null,
            replies: Vec::new(),
            request: JobSpec::new("", "", "", "", ""),
            reopened_at: None,
            gate_retired_at: None,
            enqueued_at: None,
            applied_at: None,
        },
        JournalRaw {
            answers: row.get(14)?,
            replies: row.get(15)?,
            request: row.get(16)?,
            reopened_at: row.get(17)?,
            gate_retired_at: row.get(18)?,
            enqueued_at: row.get(19)?,
            applied_at: row.get(20)?,
        },
    ))
}

/// Decode a journal's JSON columns and stamps, and check its state word.
fn decode_journal(
    (mut row, raw): (PauseJournalRow, JournalRaw),
) -> Result<PauseJournalRow, VivariumError> {
    row.answers = serde_json::from_str(&raw.answers).map_err(db_err("decode pause answers"))?;
    row.replies = serde_json::from_str(&raw.replies).map_err(db_err("decode pause replies"))?;
    row.request = serde_json::from_str(&raw.request).map_err(db_err("decode pause request"))?;
    row.reopened_at = raw.reopened_at.map(from_u64);
    row.gate_retired_at = raw.gate_retired_at.map(from_u64);
    row.enqueued_at = raw.enqueued_at.map(from_u64);
    row.applied_at = raw.applied_at.map(from_u64);
    if !recovery_state::is_known(&row.state) {
        return Err(VivariumError::Other(format!(
            "pump.db pause_journals: unknown journal state {:?}",
            row.state
        )));
    }
    Ok(row)
}

fn check_pause_state(word: &str) -> Result<(), VivariumError> {
    if pause_state::is_known(word) {
        Ok(())
    } else {
        Err(VivariumError::Other(format!(
            "pause boundary state: expected one of {}; actual: {word}",
            pause_state::ALL.join(", ")
        )))
    }
}

impl PumpStore {
    /// Record one pause boundary, or adopt the stored one on replay.
    ///
    /// A replay names an origin and pause generation that is already
    /// recorded: the stored boundary is returned unchanged, so a replay can
    /// never move the gate, the generations or the role request — even when
    /// it presents different values. Recording a generation older than the
    /// newest recorded for the origin is refused: pauses move forward, never
    /// back.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the state word is unknown, the
    /// generation is older than the origin's newest, or the write fails.
    pub fn record_pause(
        &self,
        boundary: &PauseBoundaryRow,
    ) -> Result<PauseBoundaryRow, VivariumError> {
        if let Some(stored) = self.pause_boundary(
            &boundary.need,
            boundary.run,
            &boundary.node,
            &boundary.origin_task,
            boundary.pause_generation,
        )? {
            return Ok(stored);
        }
        check_pause_state(&boundary.state)?;
        let newest = self.newest_pause_generation(
            &boundary.need,
            boundary.run,
            &boundary.node,
            &boundary.origin_task,
        )?;
        if boundary.pause_generation <= newest {
            return Err(VivariumError::Other(format!(
                "pump.db: pause generation {} of origin {:?} is older than the newest \
                 recorded ({newest})",
                boundary.pause_generation, boundary.origin_task
            )));
        }
        self.conn
            .execute(
                "INSERT INTO pause_boundaries (need, run, node, origin_task, \
                 pause_generation, gate, input_generation, role_request, boundary_message, \
                 paused_at, state)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    boundary.need,
                    i64::from(boundary.run),
                    boundary.node,
                    boundary.origin_task,
                    i64::from(boundary.pause_generation),
                    boundary.gate,
                    i64::from(boundary.input_generation),
                    boundary.role_request,
                    boundary.boundary_message,
                    stamp(boundary.paused_at),
                    boundary.state,
                ],
            )
            .map_err(db_err("write pause boundary"))?;
        Ok(boundary.clone())
    }

    /// The boundary recorded for `(origin, pause generation)`, if any.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn pause_boundary(
        &self,
        need: &str,
        run: u32,
        node: &str,
        origin_task: &str,
        pause_generation: u32,
    ) -> Result<Option<PauseBoundaryRow>, VivariumError> {
        self.one_boundary(
            "need = ?1 AND run = ?2 AND node = ?3 AND origin_task = ?4 \
             AND pause_generation = ?5",
            params![
                need,
                i64::from(run),
                node,
                origin_task,
                i64::from(pause_generation)
            ],
        )
    }

    /// The newest boundary recorded for `origin_task`, if any.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn latest_pause_boundary(
        &self,
        need: &str,
        run: u32,
        node: &str,
        origin_task: &str,
    ) -> Result<Option<PauseBoundaryRow>, VivariumError> {
        self.one_boundary(
            "need = ?1 AND run = ?2 AND node = ?3 AND origin_task = ?4 \
             ORDER BY pause_generation DESC LIMIT 1",
            params![need, i64::from(run), node, origin_task],
        )
    }

    /// Stamp the input message boundary of a recorded pause. The first
    /// snapshot wins: repeating it with the same boundary is a no-op, and a
    /// different boundary is refused — an adopted pause never moves its
    /// boundary.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when no pause carries the identity, or the
    /// boundary would move.
    pub fn snapshot_input(
        &self,
        need: &str,
        run: u32,
        node: &str,
        origin_task: &str,
        pause_generation: u32,
        boundary_message: &str,
    ) -> Result<String, VivariumError> {
        let pause = self
            .pause_boundary(need, run, node, origin_task, pause_generation)?
            .ok_or_else(|| missing_pause(origin_task, pause_generation))?;
        match pause.boundary_message.as_deref() {
            Some(stored) if stored == boundary_message => Ok(stored.to_owned()),
            Some(stored) => Err(VivariumError::Other(format!(
                "pump.db: pause of origin {origin_task:?} is already stamped \
                 {stored:?}; refusing to move it to {boundary_message:?}"
            ))),
            None => {
                self.conn
                    .execute(
                        "UPDATE pause_boundaries SET boundary_message = ?6 WHERE need = ?1 \
                         AND run = ?2 AND node = ?3 AND origin_task = ?4 \
                         AND pause_generation = ?5",
                        params![
                            need,
                            i64::from(run),
                            node,
                            origin_task,
                            i64::from(pause_generation),
                            boundary_message
                        ],
                    )
                    .map_err(db_err("stamp pause boundary message"))?;
                Ok(boundary_message.to_owned())
            }
        }
    }

    /// Retire a parked origin: its park ended. Idempotent.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when no pause carries the identity or the
    /// update fails.
    pub fn retire_pause(
        &self,
        need: &str,
        run: u32,
        node: &str,
        origin_task: &str,
        pause_generation: u32,
    ) -> Result<(), VivariumError> {
        let pause = self
            .pause_boundary(need, run, node, origin_task, pause_generation)?
            .ok_or_else(|| missing_pause(origin_task, pause_generation))?;
        if pause.state == pause_state::RETIRED {
            return Ok(());
        }
        self.conn
            .execute(
                "UPDATE pause_boundaries SET state = ?6 WHERE need = ?1 AND run = ?2 \
                 AND node = ?3 AND origin_task = ?4 AND pause_generation = ?5",
                params![
                    need,
                    i64::from(run),
                    node,
                    origin_task,
                    i64::from(pause_generation),
                    pause_state::RETIRED
                ],
            )
            .map_err(db_err("retire pause boundary"))?;
        Ok(())
    }

    /// Atomically reserve a continuation and the reply identities it
    /// consumes.
    ///
    /// The journal is anchored to the recorded pause: it copies the origin's
    /// gate, generations, role request, input boundary and pause stamp from
    /// the stored boundary, so a reservation names a real, snapshotted park.
    /// A replay naming a replacement request that is already reserved returns
    /// the stored journal unchanged and consumes nothing further. A fresh
    /// reservation consumes each of its reply identities for the origin: a
    /// reply this origin already consumed is a hard insert error, and the
    /// whole reservation rolls back.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when no recorded snapshot carries the
    /// origin, a reply is already consumed by the origin, or a write fails.
    pub fn reserve_continuation(
        &self,
        need: &str,
        run: u32,
        node: &str,
        origin_task: &str,
        pause_generation: u32,
        continuation: &PauseContinuation,
    ) -> Result<PauseJournalRow, VivariumError> {
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(db_err("begin pause reservation"))?;
        let reserved = self.reserve_continuation_tx(
            need,
            run,
            node,
            origin_task,
            pause_generation,
            continuation,
        );
        if reserved.is_err() {
            drop(self.conn.execute_batch("ROLLBACK"));
            return reserved;
        }
        self.conn
            .execute_batch("COMMIT")
            .map_err(db_err("commit pause reservation"))?;
        reserved
    }

    fn reserve_continuation_tx(
        &self,
        need: &str,
        run: u32,
        node: &str,
        origin_task: &str,
        pause_generation: u32,
        continuation: &PauseContinuation,
    ) -> Result<PauseJournalRow, VivariumError> {
        let pause = self.reserve_pause(need, run, node, origin_task, pause_generation)?;
        if let Some(stored) = self.pause_journal(&continuation.replacement_request)? {
            return Ok(stored);
        }
        self.reserve_replies(need, run, node, origin_task, &continuation.replies)?;
        self.insert_pause_journal(&pause, continuation)
    }

    /// The recorded, snapshotted boundary a reservation anchors to.
    fn reserve_pause(
        &self,
        need: &str,
        run: u32,
        node: &str,
        origin_task: &str,
        pause_generation: u32,
    ) -> Result<PauseBoundaryRow, VivariumError> {
        let pause = self
            .pause_boundary(need, run, node, origin_task, pause_generation)?
            .ok_or_else(|| missing_pause(origin_task, pause_generation))?;
        if pause.boundary_message.is_none() {
            return Err(VivariumError::Other(format!(
                "pump.db: pause of origin {origin_task:?} generation \
                 {pause_generation} has no input snapshot to continue from"
            )));
        }
        Ok(pause)
    }

    fn reserve_replies(
        &self,
        need: &str,
        run: u32,
        node: &str,
        origin_task: &str,
        replies: &[PauseJournalReply],
    ) -> Result<(), VivariumError> {
        for reply in replies {
            self.conn
                .execute(
                    "INSERT INTO pause_replies (need, run, node, origin_task, reply_id) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![need, i64::from(run), node, origin_task, reply.id],
                )
                .map_err(db_err("reserve pause reply"))?;
        }
        Ok(())
    }

    /// Write one fresh pending journal anchored to `pause`, and return it.
    fn insert_pause_journal(
        &self,
        pause: &PauseBoundaryRow,
        continuation: &PauseContinuation,
    ) -> Result<PauseJournalRow, VivariumError> {
        let answers =
            serde_json::to_string(&continuation.answers).map_err(db_err("encode pause answers"))?;
        let replies =
            serde_json::to_string(&continuation.replies).map_err(db_err("encode pause replies"))?;
        let request =
            serde_json::to_string(&continuation.request).map_err(db_err("encode pause request"))?;
        let row = fresh_journal(pause, continuation);
        self.conn
            .execute(
                "INSERT INTO pause_journals (replacement_request, need, run, node, \
                 origin_task, pause_generation, gate, input_generation, role_request, \
                 boundary_message, input_message, paused_at, state, attempt, answers_json, \
                 replies_json, request_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
                 ?16, ?17)",
                params![
                    row.replacement_request,
                    row.need,
                    i64::from(row.run),
                    row.node,
                    row.origin_task,
                    i64::from(row.pause_generation),
                    row.gate,
                    i64::from(row.input_generation),
                    row.role_request,
                    row.boundary_message,
                    row.input_message,
                    stamp(row.paused_at),
                    row.state,
                    i64::from(row.attempt),
                    answers,
                    replies,
                    request,
                ],
            )
            .map_err(db_err("write pause journal"))?;
        Ok(row)
    }

    /// The journal frozen for replacement request `replacement_request`, if
    /// any.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read or decoded.
    pub fn pause_journal(
        &self,
        replacement_request: &str,
    ) -> Result<Option<PauseJournalRow>, VivariumError> {
        self.one_journal("replacement_request = ?1", params![replacement_request])
    }

    /// Every unfinished (pending) journal, oldest pause first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when a row cannot be read or decoded.
    pub fn unfinished_pause_journals(&self) -> Result<Vec<PauseJournalRow>, VivariumError> {
        self.journal_rows(
            &format!(
                "SELECT {JOURNAL_COLUMNS} FROM pause_journals WHERE state = ?1 \
                 ORDER BY paused_at, replacement_request"
            ),
            params![recovery_state::PENDING],
        )
    }

    /// Stamp one replay stage of a continuation journal. Idempotent — a
    /// repeated stamp keeps the first time.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the stage word is unknown, no journal
    /// carries the replacement request, or the update fails.
    pub fn mark_journal_stage(
        &self,
        replacement_request: &str,
        stage: &str,
        at: u64,
    ) -> Result<(), VivariumError> {
        if !journal_stage::is_known(stage) {
            return Err(VivariumError::Other(format!(
                "pause journal stage: expected one of {}; actual: {stage}",
                journal_stage::ALL.join(", ")
            )));
        }
        let journal = self
            .pause_journal(replacement_request)?
            .ok_or_else(|| missing_journal(replacement_request))?;
        let (column, current) = match stage {
            journal_stage::REOPENED => ("reopened_at", journal.reopened_at),
            journal_stage::GATE_RETIRED => ("gate_retired_at", journal.gate_retired_at),
            _ => ("enqueued_at", journal.enqueued_at),
        };
        if current.is_some() {
            return Ok(());
        }
        let sql = format!("UPDATE pause_journals SET {column} = ?2 WHERE replacement_request = ?1");
        self.conn
            .execute(&sql, params![replacement_request, stamp(at)])
            .map_err(db_err("stamp pause journal stage"))?;
        Ok(())
    }

    /// Mark the journal of `replacement_request` applied: its replacement
    /// request was minted. Idempotent — a repeated mark keeps the first
    /// stamp.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when no journal carries the replacement
    /// request or the update fails.
    pub fn mark_pause_journal_applied(
        &self,
        replacement_request: &str,
        at: u64,
    ) -> Result<(), VivariumError> {
        let existing = self
            .pause_journal(replacement_request)?
            .ok_or_else(|| missing_journal(replacement_request))?;
        if existing.state == recovery_state::APPLIED {
            return Ok(());
        }
        self.conn
            .execute(
                "UPDATE pause_journals SET state = ?2, applied_at = ?3 \
                 WHERE replacement_request = ?1",
                params![replacement_request, recovery_state::APPLIED, stamp(at)],
            )
            .map_err(db_err("mark pause journal applied"))?;
        Ok(())
    }

    /// The reply identities `origin_task` has already consumed, in
    /// reservation order.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn consumed_pause_replies(
        &self,
        need: &str,
        run: u32,
        node: &str,
        origin_task: &str,
    ) -> Result<Vec<String>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT reply_id FROM pause_replies WHERE need = ?1 AND run = ?2 \
                 AND node = ?3 AND origin_task = ?4 ORDER BY rowid",
            )
            .map_err(db_err("prepare pause reply read"))?;
        stmt.query_map(params![need, i64::from(run), node, origin_task], |row| {
            row.get(0)
        })
        .map_err(db_err("read pause replies"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read pause replies"))
    }

    /// Invalidate recovery for one flow run: cancel everything that could
    /// still fire, transient and continuation alike. Pending recovery
    /// intents, open pause boundaries, pending journals and the run's
    /// consumed reply identities are removed; applied intents, retired
    /// boundaries and applied journals stay as receipts.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when a removal fails.
    pub fn invalidate_run_recovery(
        &self,
        need: &str,
        run: u32,
    ) -> Result<RunRecoveryInvalidation, VivariumError> {
        let mut removed = RunRecoveryInvalidation::default();
        removed.intents = self.remove(
            "DELETE FROM recovery_intents WHERE need = ?1 AND run = ?2 AND state = ?3",
            params![need, i64::from(run), recovery_state::PENDING],
            "remove run recovery intents",
        )?;
        removed.boundaries = self.remove(
            "DELETE FROM pause_boundaries WHERE need = ?1 AND run = ?2 AND state = ?3",
            params![need, i64::from(run), pause_state::OPEN],
            "remove run pause boundaries",
        )?;
        removed.replies = self.remove(
            "DELETE FROM pause_replies WHERE need = ?1 AND run = ?2",
            params![need, i64::from(run)],
            "remove run pause replies",
        )?;
        removed.journals = self.remove(
            "DELETE FROM pause_journals WHERE need = ?1 AND run = ?2 AND state = ?3",
            params![need, i64::from(run), recovery_state::PENDING],
            "remove run pause journals",
        )?;
        Ok(removed)
    }

    fn remove(
        &self,
        sql: &str,
        args: impl rusqlite::Params,
        what: &'static str,
    ) -> Result<usize, VivariumError> {
        self.conn.execute(sql, args).map_err(db_err(what))
    }

    /// The highest pause generation recorded for `origin_task`; absence is 0.
    fn newest_pause_generation(
        &self,
        need: &str,
        run: u32,
        node: &str,
        origin_task: &str,
    ) -> Result<u32, VivariumError> {
        let newest: Option<i64> = self
            .conn
            .query_row(
                "SELECT MAX(pause_generation) FROM pause_boundaries WHERE need = ?1 \
                 AND run = ?2 AND node = ?3 AND origin_task = ?4",
                params![need, i64::from(run), node, origin_task],
                |row| row.get(0),
            )
            .map_err(db_err("read newest pause generation"))?;
        Ok(from_u32(newest.unwrap_or(0)))
    }

    fn one_boundary(
        &self,
        filter: &str,
        args: impl rusqlite::Params,
    ) -> Result<Option<PauseBoundaryRow>, VivariumError> {
        let sql = format!("SELECT {BOUNDARY_COLUMNS} FROM pause_boundaries WHERE {filter}");
        self.conn
            .query_row(&sql, args, boundary_row)
            .optional()
            .map_err(db_err("read pause boundary"))
    }

    fn one_journal(
        &self,
        filter: &str,
        args: impl rusqlite::Params,
    ) -> Result<Option<PauseJournalRow>, VivariumError> {
        let sql = format!("SELECT {JOURNAL_COLUMNS} FROM pause_journals WHERE {filter}");
        self.conn
            .query_row(&sql, args, journal_raw_row)
            .optional()
            .map_err(db_err("read pause journal"))?
            .map(decode_journal)
            .transpose()
    }

    fn journal_rows(
        &self,
        sql: &str,
        args: impl rusqlite::Params,
    ) -> Result<Vec<PauseJournalRow>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(db_err("prepare pause journal read"))?;
        let raw = stmt
            .query_map(args, journal_raw_row)
            .map_err(db_err("read pause journals"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err("read pause journals"))?;
        raw.into_iter().map(decode_journal).collect()
    }
}

/// The fresh pending journal a reservation writes, anchored to `pause`:
/// gate, generations, role request, input boundary and pause stamp are
/// copied from the stored boundary, not asserted by the caller.
fn fresh_journal(pause: &PauseBoundaryRow, continuation: &PauseContinuation) -> PauseJournalRow {
    PauseJournalRow {
        replacement_request: continuation.replacement_request.clone(),
        need: pause.need.clone(),
        run: pause.run,
        node: pause.node.clone(),
        origin_task: pause.origin_task.clone(),
        gate: pause.gate.clone(),
        pause_generation: pause.pause_generation,
        input_generation: pause.input_generation,
        role_request: pause.role_request.clone(),
        boundary_message: pause.boundary_message.clone().unwrap_or_default(),
        input_message: continuation.input_message.clone(),
        answers: continuation.answers.clone(),
        replies: continuation.replies.clone(),
        request: continuation.request.clone(),
        attempt: continuation.attempt,
        state: recovery_state::PENDING.into(),
        paused_at: pause.paused_at,
        reopened_at: None,
        gate_retired_at: None,
        enqueued_at: None,
        applied_at: None,
    }
}

/// The error for a reservation that names no recorded pause.
fn missing_pause(origin_task: &str, pause_generation: u32) -> VivariumError {
    VivariumError::Other(format!(
        "pump.db: no pause boundary names origin {origin_task:?} generation {pause_generation}"
    ))
}

/// The error for a stamp that names no frozen journal.
fn missing_journal(replacement_request: &str) -> VivariumError {
    VivariumError::Other(format!(
        "pump.db: no continuation journal names replacement request {replacement_request:?}"
    ))
}

#[cfg(test)]
#[path = "pause_test.rs"]
mod tests;
