//! Immutable admission artifacts and run-continuation context of `pump.db`.
//!
//! An admission artifact is the durable record of the plan a run admitted:
//! the need and source run, the planner task with the execution identity and
//! input generation that produced the accepted plan, the admitting task with
//! the accepted admission identity, and the exact accepted plan content. The
//! first accepted reply for a run stores the artifact; every later reply of
//! the same run — a revision, a replay, a different admitting task — finds it
//! standing and writes nothing, so the artifact a continuation names can
//! never drift underneath it.
//!
//! A run continuation is the explicit context of one run continuing an
//! earlier one: the continuation kind (the restart entry the run starts at),
//! the continued source run with its artifact identity, and the planning
//! boundary. Slice and rework continuations may inherit only an explicit
//! source — the store records exactly the source the caller names and never
//! derives one; plan and research continuations establish a planning
//! boundary and inherit nothing, so recording one with a source is refused.
//!
//! A restart intent lets the context be recorded before the target run
//! exists: the intent stands pending until the eventual run attaches it,
//! idempotently — a repeated attach keeps the first context, and a pending
//! intent replays unchanged. Like the other flow tables, these tables are
//! created with `IF NOT EXISTS` on every open and are not part of the
//! [`State`](crate::pump::state::State) save, so `save()` and `clear()` never
//! touch them and the schema version is not bumped.

use rusqlite::{Connection, OptionalExtension, params};

use super::{PumpStore, db_err, from_u32, from_u64};
use crate::error::VivariumError;

/// Why a run continues an earlier run: the restart entry the newer run
/// starts at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationKind {
    /// A research restart: the run re-researches from scratch.
    Research,
    /// A plan restart: the run re-plans from scratch.
    Plan,
    /// A slice restart: the run re-slices and may inherit an explicit source.
    Slice,
    /// A rework restart: the run reworks and may inherit an explicit source.
    Rework,
}

impl ContinuationKind {
    /// The stored label of this kind.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Research => "research",
            Self::Plan => "plan",
            Self::Slice => "slice",
            Self::Rework => "rework",
        }
    }

    /// The kind for a stored label.
    ///
    /// # Errors
    /// Returns [`VivariumError`] for a label no continuation kind answers.
    pub fn parse(text: &str) -> Result<Self, VivariumError> {
        match text {
            "research" => Ok(Self::Research),
            "plan" => Ok(Self::Plan),
            "slice" => Ok(Self::Slice),
            "rework" => Ok(Self::Rework),
            other => Err(VivariumError::Message(format!(
                "unknown continuation kind {other}"
            ))),
        }
    }

    /// True when the kind establishes a planning boundary: the run plans from
    /// scratch and inherits nothing from a source run.
    #[must_use]
    pub fn establishes_boundary(self) -> bool {
        matches!(self, Self::Research | Self::Plan)
    }
}

/// One accepted admission to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionArtifact {
    /// The need the plan belongs to.
    pub need: String,
    /// The source run that planned and admitted it.
    pub run: u32,
    /// The planner task whose plan was accepted.
    pub planner_task: String,
    /// The execution identity the accepted plan came from.
    pub execution: String,
    /// The input generation the accepted plan came from.
    pub generation: u32,
    /// The admitting task that accepted the plan.
    pub admit_task: String,
    /// The admission identity the admitting task accepted.
    pub admission: String,
    /// The exact accepted plan content.
    pub plan_body: String,
}

/// One stored admission artifact: the first accepted plan of its run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionArtifactRow {
    pub need: String,
    pub run: u32,
    pub planner_task: String,
    pub execution: String,
    pub generation: u32,
    pub admit_task: String,
    pub admission: String,
    pub plan_body: String,
    pub created_at: u64,
}

impl AdmissionArtifactRow {
    /// The canonical identity a continuation names as its source artifact.
    pub fn identity(&self) -> String {
        format!("{}:r{}", self.need, self.run)
    }
}

/// The outcome of storing one admission artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionOutcome {
    /// The artifact recorded the run's accepted plan.
    Recorded(AdmissionArtifactRow),
    /// The run already had an artifact; the stored one answers unchanged and
    /// the newer reply wrote nothing.
    Existing(AdmissionArtifactRow),
}

/// One restart intent to record before the target run exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartIntent {
    /// The need the eventual run belongs to.
    pub need: String,
    /// The continuation kind the eventual run starts at.
    pub kind: ContinuationKind,
    /// The explicit source run to inherit, when the kind may inherit one.
    pub source_run: Option<u32>,
    /// The explicit source artifact identity, when the kind may inherit one.
    pub source_artifact: Option<String>,
}

/// One stored restart intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartIntentRow {
    pub intent_id: u64,
    pub need: String,
    pub kind: ContinuationKind,
    pub source_run: Option<u32>,
    pub source_artifact: Option<String>,
    /// The planning boundary the kind establishes.
    pub boundary: bool,
    /// The run the intent attached to, once one attached it.
    pub attached_run: Option<u32>,
    pub created_at: u64,
}

impl RestartIntentRow {
    /// True while no run has attached the intent yet.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.attached_run.is_none()
    }
}

/// The outcome of recording one restart intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartIntentOutcome {
    /// A new pending intent was stored.
    Recorded(RestartIntentRow),
    /// A pending intent of the same need and kind already stood; the stored
    /// one answers unchanged and the newer request wrote nothing.
    Existing(RestartIntentRow),
}

/// One explicit run continuation to store without a restart intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunContinuation {
    /// The need the continued run belongs to.
    pub need: String,
    /// The run that continues a source.
    pub run: u32,
    /// The continuation kind the run starts at.
    pub kind: ContinuationKind,
    /// The explicit source run to inherit, when the kind may inherit one.
    pub source_run: Option<u32>,
    /// The explicit source artifact identity, when the kind may inherit one.
    pub source_artifact: Option<String>,
}

/// One stored run continuation: the first context attached to its run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunContinuationRow {
    pub need: String,
    pub run: u32,
    pub kind: ContinuationKind,
    pub source_run: Option<u32>,
    pub source_artifact: Option<String>,
    /// The planning boundary the kind establishes.
    pub boundary: bool,
    /// The restart intent the context came from, when it attached one.
    pub intent_id: Option<u64>,
    pub created_at: u64,
}

impl RunContinuationRow {
    /// The source this continuation inherits, when any: a planning boundary
    /// inherits nothing, and any other kind inherits only an explicit source.
    #[must_use]
    pub fn inherited_source(&self) -> Option<(u32, Option<&str>)> {
        if self.boundary {
            return None;
        }
        self.source_run
            .map(|run| (run, self.source_artifact.as_deref()))
    }
}

/// The outcome of storing one run continuation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinuationOutcome {
    /// The context was recorded for the run.
    Recorded(RunContinuationRow),
    /// The run already had a context; the stored one answers unchanged and
    /// the newer attachment wrote nothing.
    Existing(RunContinuationRow),
}

const DDL: &str = "
CREATE TABLE IF NOT EXISTS admission_artifacts (
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    planner_task TEXT NOT NULL,
    execution TEXT NOT NULL,
    generation INTEGER NOT NULL,
    admit_task TEXT NOT NULL,
    admission TEXT NOT NULL,
    plan_body TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (need, run)
);
CREATE TABLE IF NOT EXISTS run_restart_intents (
    intent_id INTEGER PRIMARY KEY AUTOINCREMENT,
    need TEXT NOT NULL,
    kind TEXT NOT NULL,
    source_run INTEGER,
    source_artifact TEXT,
    boundary INTEGER NOT NULL DEFAULT 0,
    attached_run INTEGER,
    created_at INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS run_restart_intents_pending
    ON run_restart_intents (need, kind, attached_run);
CREATE TABLE IF NOT EXISTS run_continuations (
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    kind TEXT NOT NULL,
    source_run INTEGER,
    source_artifact TEXT,
    boundary INTEGER NOT NULL DEFAULT 0,
    intent_id INTEGER,
    created_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (need, run)
);
";

/// Create the run-context tables when they are missing.
pub(super) fn ensure_tables(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(DDL)
        .map_err(db_err("create run context tables"))
}

fn now() -> &'static str {
    "CAST(strftime('%s', 'now') AS INTEGER)"
}

/// Refuse a boundary kind that tries to inherit a source: plan and research
/// establish boundaries with no inheritance.
fn check_source(
    kind: ContinuationKind,
    source_run: Option<u32>,
    what: &'static str,
) -> Result<(), VivariumError> {
    if kind.establishes_boundary() && source_run.is_some() {
        return Err(VivariumError::Message(format!(
            "a {} continuation establishes a planning boundary and cannot inherit a source ({what})",
            kind.as_str()
        )));
    }
    Ok(())
}

fn boundary_of(kind: ContinuationKind) -> i64 {
    i64::from(kind.establishes_boundary())
}

const ARTIFACT_COLUMNS: &str =
    "need, run, planner_task, execution, generation, admit_task, admission, plan_body, created_at";

fn artifact_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AdmissionArtifactRow> {
    Ok(AdmissionArtifactRow {
        need: row.get(0)?,
        run: from_u32(row.get(1)?),
        planner_task: row.get(2)?,
        execution: row.get(3)?,
        generation: from_u32(row.get(4)?),
        admit_task: row.get(5)?,
        admission: row.get(6)?,
        plan_body: row.get(7)?,
        created_at: from_u64(row.get(8)?),
    })
}

const INTENT_COLUMNS: &str =
    "intent_id, need, kind, source_run, source_artifact, boundary, attached_run, created_at";

fn intent_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(RestartIntentRow, String)> {
    Ok((
        RestartIntentRow {
            intent_id: from_u64(row.get(0)?),
            need: row.get(1)?,
            kind: ContinuationKind::Research,
            source_run: row.get::<_, Option<i64>>(3)?.map(from_u32),
            source_artifact: row.get(4)?,
            boundary: row.get::<_, i64>(5)? != 0,
            attached_run: row.get::<_, Option<i64>>(6)?.map(from_u32),
            created_at: from_u64(row.get(7)?),
        },
        row.get(2)?,
    ))
}

fn finish_intent(
    (mut intent, kind): (RestartIntentRow, String),
) -> Result<RestartIntentRow, VivariumError> {
    intent.kind = ContinuationKind::parse(&kind)?;
    Ok(intent)
}

fn query_intents(
    conn: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<RestartIntentRow>, VivariumError> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(db_err("prepare restart intents"))?;
    let rows = stmt
        .query_map(args, intent_from_row)
        .map_err(db_err("read restart intents"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read restart intent row"))?
        .into_iter()
        .map(finish_intent)
        .collect()
}

const CONTINUATION_COLUMNS: &str =
    "need, run, kind, source_run, source_artifact, boundary, intent_id, created_at";

fn continuation_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(RunContinuationRow, String)> {
    Ok((
        RunContinuationRow {
            need: row.get(0)?,
            run: from_u32(row.get(1)?),
            kind: ContinuationKind::Research,
            source_run: row.get::<_, Option<i64>>(3)?.map(from_u32),
            source_artifact: row.get(4)?,
            boundary: row.get::<_, i64>(5)? != 0,
            intent_id: row.get::<_, Option<i64>>(6)?.map(from_u64),
            created_at: from_u64(row.get(7)?),
        },
        row.get(2)?,
    ))
}

fn finish_continuation(
    (mut row, kind): (RunContinuationRow, String),
) -> Result<RunContinuationRow, VivariumError> {
    row.kind = ContinuationKind::parse(&kind)?;
    Ok(row)
}

fn query_continuations(
    conn: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<RunContinuationRow>, VivariumError> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(db_err("prepare run continuations"))?;
    let rows = stmt
        .query_map(args, continuation_from_row)
        .map_err(db_err("read run continuations"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read run continuation row"))?
        .into_iter()
        .map(finish_continuation)
        .collect()
}

fn no_such_intent(intent_id: u64) -> VivariumError {
    VivariumError::Message(format!("restart intent {intent_id}: no such intent"))
}

fn existing_continuation(
    conn: &Connection,
    need: &str,
    run: u32,
) -> Result<Option<RunContinuationRow>, VivariumError> {
    query_continuations(
        conn,
        &format!(
            "SELECT {CONTINUATION_COLUMNS} FROM run_continuations WHERE need = ?1 AND run = ?2"
        ),
        params![need, run],
    )
    .map(|rows| rows.into_iter().next())
}

impl PumpStore {
    /// Store the accepted admission of one run. The first accepted reply of a
    /// run records the artifact; every later reply of the same run — whatever
    /// plan, tasks or admission identity it now carries — finds the stored
    /// artifact standing and writes nothing.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read or written.
    pub fn store_admission_artifact(
        &self,
        artifact: &AdmissionArtifact,
    ) -> Result<AdmissionOutcome, VivariumError> {
        let inserted = self
            .conn
            .execute(
                &format!(
                    "INSERT OR IGNORE INTO admission_artifacts ({ARTIFACT_COLUMNS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, {})",
                    now()
                ),
                params![
                    artifact.need,
                    artifact.run,
                    artifact.planner_task,
                    artifact.execution,
                    artifact.generation,
                    artifact.admit_task,
                    artifact.admission,
                    artifact.plan_body,
                ],
            )
            .map_err(db_err("store admission artifact"))?;
        let row = self
            .admission_artifact(&artifact.need, artifact.run)?
            .ok_or_else(|| {
                VivariumError::Other(
                    "pump.db admission artifact: insert left no readable row".to_string(),
                )
            })?;
        Ok(if inserted == 1 {
            AdmissionOutcome::Recorded(row)
        } else {
            AdmissionOutcome::Existing(row)
        })
    }

    /// The artifact a run accepted, when one stands.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn admission_artifact(
        &self,
        need: &str,
        run: u32,
    ) -> Result<Option<AdmissionArtifactRow>, VivariumError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {ARTIFACT_COLUMNS} FROM admission_artifacts \
                     WHERE need = ?1 AND run = ?2"
                ),
                params![need, run],
                artifact_from_row,
            )
            .optional()
            .map_err(db_err("read admission artifact"))
    }

    /// Record a restart intent before the target run exists. A pending intent
    /// of the same need and kind answers a replay unchanged — never a rewrite
    /// of its kind, source or boundary — so replaying the record cannot
    /// allocate a second intent; an intent an earlier run already attached
    /// never blocks a fresh restart and records a new intent.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read or written, or
    /// a boundary kind carries a source.
    pub fn record_restart_intent(
        &self,
        intent: &RestartIntent,
    ) -> Result<RestartIntentOutcome, VivariumError> {
        check_source(intent.kind, intent.source_run, "restart intent")?;
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin restart intent"))?;
        let pending = query_intents(
            &tx,
            &format!(
                "SELECT {INTENT_COLUMNS} FROM run_restart_intents \
                 WHERE need = ?1 AND kind = ?2 AND attached_run IS NULL \
                 ORDER BY intent_id"
            ),
            params![intent.need, intent.kind.as_str()],
        )?;
        if let Some(row) = pending.into_iter().next() {
            return Ok(RestartIntentOutcome::Existing(row));
        }
        tx.execute(
            &format!(
                "INSERT INTO run_restart_intents \
                   (need, kind, source_run, source_artifact, boundary, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, {})",
                now()
            ),
            params![
                intent.need,
                intent.kind.as_str(),
                intent.source_run,
                intent.source_artifact,
                boundary_of(intent.kind),
            ],
        )
        .map_err(db_err("insert restart intent"))?;
        let intent_id = tx.last_insert_rowid();
        tx.commit().map_err(db_err("commit restart intent"))?;
        let intent_id = u64::try_from(intent_id).unwrap_or(0);
        let row = self
            .restart_intent(intent_id)?
            .ok_or_else(|| no_such_intent(intent_id))?;
        Ok(RestartIntentOutcome::Recorded(row))
    }

    /// One stored restart intent, current or attached.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn restart_intent(
        &self,
        intent_id: u64,
    ) -> Result<Option<RestartIntentRow>, VivariumError> {
        Ok(query_intents(
            &self.conn,
            &format!("SELECT {INTENT_COLUMNS} FROM run_restart_intents WHERE intent_id = ?1"),
            params![i64::try_from(intent_id).unwrap_or(0)],
        )?
        .into_iter()
        .next())
    }

    /// The need's intents that no run has attached yet, oldest first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn pending_restart_intents(
        &self,
        need: &str,
    ) -> Result<Vec<RestartIntentRow>, VivariumError> {
        query_intents(
            &self.conn,
            &format!(
                "SELECT {INTENT_COLUMNS} FROM run_restart_intents \
                 WHERE need = ?1 AND attached_run IS NULL ORDER BY intent_id"
            ),
            params![need],
        )
    }

    /// Store a run continuation directly, without a restart intent. The first
    /// context a run receives stands; a repeated continuation — whatever
    /// source it now names — keeps the original context and writes nothing.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read or written, or
    /// a boundary kind carries a source.
    pub fn record_run_continuation(
        &self,
        continuation: &RunContinuation,
    ) -> Result<ContinuationOutcome, VivariumError> {
        check_source(continuation.kind, continuation.source_run, "continuation")?;
        let existing = self.write_continuation(&continuation.need, continuation.run, |tx| {
            tx.execute(
                &format!(
                    "INSERT OR IGNORE INTO run_continuations ({CONTINUATION_COLUMNS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, {})",
                    now()
                ),
                params![
                    continuation.need,
                    continuation.run,
                    continuation.kind.as_str(),
                    continuation.source_run,
                    continuation.source_artifact,
                    boundary_of(continuation.kind),
                ],
            )
            .map_err(db_err("store run continuation"))?;
            Ok(())
        })?;
        self.continuation_outcome(existing, &continuation.need, continuation.run)
    }

    /// Attach a restart intent to the eventual run `need`/`run`, copying the
    /// intent's kind, source and boundary into the run's continuation. The
    /// first attach wins: a repeated attach keeps the stored context, and the
    /// intent keeps the first run it attached to, so one intent can never
    /// seed two lineages.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the intent is unknown, already attached
    /// to a different run, or the rows cannot be read or written.
    pub fn attach_restart_intent(
        &self,
        need: &str,
        run: u32,
        intent_id: u64,
    ) -> Result<ContinuationOutcome, VivariumError> {
        let intent = self
            .restart_intent(intent_id)?
            .ok_or_else(|| no_such_intent(intent_id))?;
        if let Some(attached) = intent.attached_run {
            if attached != run {
                return Err(VivariumError::Message(format!(
                    "restart intent {intent_id} is already attached to {} r{attached}",
                    intent.need
                )));
            }
        }
        let existing = self.write_continuation(need, run, |tx| {
            tx.execute(
                &format!(
                    "INSERT OR IGNORE INTO run_continuations ({CONTINUATION_COLUMNS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, {})",
                    now()
                ),
                params![
                    need,
                    run,
                    intent.kind.as_str(),
                    intent.source_run,
                    intent.source_artifact,
                    boundary_of(intent.kind),
                    i64::try_from(intent.intent_id).unwrap_or(0),
                ],
            )
            .map_err(db_err("attach restart intent"))?;
            if intent.is_pending() {
                tx.execute(
                    "UPDATE run_restart_intents SET attached_run = ?2 \
                     WHERE intent_id = ?1 AND attached_run IS NULL",
                    params![i64::try_from(intent.intent_id).unwrap_or(0), run],
                )
                .map_err(db_err("mark restart intent attached"))?;
            }
            Ok(())
        })?;
        self.continuation_outcome(existing, need, run)
    }

    /// The outcome of one continuation write: an existing context answers
    /// unchanged, a recorded one is read back from the row just written.
    fn continuation_outcome(
        &self,
        existing: Option<RunContinuationRow>,
        need: &str,
        run: u32,
    ) -> Result<ContinuationOutcome, VivariumError> {
        match existing {
            Some(row) => Ok(ContinuationOutcome::Existing(row)),
            None => {
                let row = self.run_continuation(need, run)?.ok_or_else(|| {
                    VivariumError::Other(
                        "pump.db run continuation: write left no readable row".to_string(),
                    )
                })?;
                Ok(ContinuationOutcome::Recorded(row))
            }
        }
    }

    /// Run `write` over the continuation tables and answer the continuation
    /// the run holds afterwards: `Some` when the run already had one, `None`
    /// when the write recorded it.
    fn write_continuation(
        &self,
        need: &str,
        run: u32,
        write: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<(), VivariumError>,
    ) -> Result<Option<RunContinuationRow>, VivariumError> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin run continuation"))?;
        if let Some(row) = existing_continuation(&tx, need, run)? {
            return Ok(Some(row));
        }
        write(&tx)?;
        tx.commit().map_err(db_err("commit run continuation"))?;
        Ok(None)
    }

    /// The context a run continues from, when one stands.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn run_continuation(
        &self,
        need: &str,
        run: u32,
    ) -> Result<Option<RunContinuationRow>, VivariumError> {
        existing_continuation(&self.conn, need, run)
    }
}

#[cfg(test)]
#[path = "run_context_test.rs"]
mod tests;
