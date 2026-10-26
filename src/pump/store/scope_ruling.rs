//! The source-keyed scope workflow journal of `pump.db`.
//!
//! When a hand answers `blocked-scope`, the pump settles the request and
//! publishes the ruling as a scope-ruling pump note on the need thread. This
//! table is the pump's local memory of that workflow. It is keyed by the
//! original source job with only the envelope-correction suffix stripped
//! ([`correct::base_id`]), so a corrected reply lands on the same record and
//! every scope re-dispatch stays its own source.
//!
//! A row records the full request identity, the typed decision, the
//! publication acknowledgement, one stable continuation identity, an
//! optional scope round, and an optional typed slice-coordination result
//! (the plain word beside its full payload). Preparing the same identity
//! twice adopts the record; reusing a source key
//! for different work is refused. A decision may be refreshed while the
//! record is genuinely unpublished; a published decision and a reserved
//! continuation never drift. Round allocation is transactional and counts
//! distinct sources, not repeated calls, and skips rounds whose step job
//! already exists.
//!
//! The journal is local workflow memory and is never permission evidence on
//! its own: only a published scope-ruling pump note grants paths (see
//! [`crate::pump::flow::need_scope`]).

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{PumpStore, db_err, from_u32, json};
use crate::error::VivariumError;
use crate::pump::envelope::correct;
use crate::pump::flow::need_scope::{RulingDecision, ScopeIssue};

const DDL: &str = "
CREATE TABLE IF NOT EXISTS scope_journal (
    source TEXT PRIMARY KEY,
    need TEXT NOT NULL,
    target TEXT NOT NULL,
    run INTEGER NOT NULL,
    node TEXT NOT NULL,
    task TEXT NOT NULL,
    attempt INTEGER NOT NULL,
    paths_json TEXT NOT NULL,
    reason TEXT NOT NULL,
    design INTEGER NOT NULL CHECK (design IN (0, 1)),
    decision TEXT NOT NULL,
    issue TEXT,
    published_at INTEGER,
    continuation TEXT,
    round INTEGER,
    coordination TEXT,
    coordination_json TEXT
);
CREATE INDEX IF NOT EXISTS scope_journal_task_idx ON scope_journal (task);
";

const COLUMNS: &str = "source, need, target, run, node, task, attempt, paths_json, reason, \
     design, decision, issue, published_at, continuation, round, coordination";

/// Create the scope-journal table when it is missing, and add the typed
/// coordination payload column to a journal written by an earlier binary.
pub(super) fn ensure_table(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(DDL)
        .map_err(db_err("create scope journal table"))?;
    ensure_coordination_json(conn)
}

fn ensure_coordination_json(conn: &Connection) -> Result<(), VivariumError> {
    let present: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('scope_journal') \
             WHERE name = 'coordination_json'",
            [],
            |row| row.get(0),
        )
        .map_err(db_err("read scope journal columns"))?;
    if present == 0 {
        conn.execute(
            "ALTER TABLE scope_journal ADD COLUMN coordination_json TEXT",
            [],
        )
        .map_err(db_err("add scope journal coordination_json"))?;
    }
    Ok(())
}

/// One journal row: a prepared request and everything recorded about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeJournalRow {
    /// The original source job (correction suffix stripped).
    pub source: String,
    /// The need the request belongs to.
    pub need: String,
    /// The pump target the request names.
    pub target: String,
    /// The run that asked.
    pub run: u32,
    /// The flow node that asked.
    pub node: String,
    /// The task handle of the requesting slice.
    pub task: String,
    /// The dispatch attempt of the source job.
    pub attempt: u32,
    /// The requested paths, as asked.
    pub paths: Vec<String>,
    /// The reason the hand gave, as asked.
    pub reason: String,
    /// Whether the hand flagged a design question.
    pub design: bool,
    /// The typed decision recorded for the request.
    pub decision: RulingDecision,
    /// Why an operator was needed; a grant carries none.
    pub issue: Option<ScopeIssue>,
    /// When the ruling was acknowledged as published (unix seconds).
    pub published_at: Option<i64>,
    /// The reserved continuation identity, once set.
    pub continuation: Option<String>,
    /// The reserved hand scope round, once allocated.
    pub round: Option<u32>,
    /// The recorded slice-coordination result, once settled.
    pub coordination: Option<ScopeCoordination>,
}

/// The request identity a [`ScopeRequest`] carries: everything prepare writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRequest {
    /// Any job id of the asking chain; the journal keys by its correction
    /// suffix-stripped base.
    pub source: String,
    /// The need the request belongs to.
    pub need: String,
    /// The pump target the request names.
    pub target: String,
    /// The run that asked.
    pub run: u32,
    /// The flow node that asked.
    pub node: String,
    /// The task handle of the requesting slice.
    pub task: String,
    /// The dispatch attempt of the source job.
    pub attempt: u32,
    /// The requested paths, as asked.
    pub paths: Vec<String>,
    /// The reason the hand gave, as asked.
    pub reason: String,
    /// Whether the hand flagged a design question.
    pub design: bool,
    /// The typed decision recorded for the request.
    pub decision: RulingDecision,
    /// Why an operator was needed; a grant carries none.
    pub issue: Option<ScopeIssue>,
}

/// What [`PumpStore::prepare_scope_request`] did with the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeRequestOutcome {
    /// A fresh journal row was recorded.
    Recorded,
    /// An identical record was already there and was left alone.
    Adopted,
    /// The record matched the identity but not the content and was
    /// refreshed while still unpublished.
    Refreshed,
}

/// How slice coordination settled a prepared request. The payload lives in
/// its own tables (`slice_scope` holders, the widened write scope); the
/// journal records only the typed outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeCoordination {
    /// The requested paths were granted to the slice.
    Widen,
    /// The slice waits on the slices holding the requested paths.
    WaitOn,
    /// The slicer must rework the scopes.
    Rework,
}

/// The stored word of a [`ScopeCoordination`].
fn coordination_word(coordination: ScopeCoordination) -> &'static str {
    match coordination {
        ScopeCoordination::Widen => "widen",
        ScopeCoordination::WaitOn => "wait-on",
        ScopeCoordination::Rework => "rework",
    }
}

/// Read a stored coordination word.
fn parse_coordination(word: &str) -> Result<ScopeCoordination, VivariumError> {
    Ok(match word {
        "widen" => ScopeCoordination::Widen,
        "wait-on" => ScopeCoordination::WaitOn,
        "rework" => ScopeCoordination::Rework,
        other => return Err(unknown_word("coordination", other)),
    })
}

/// The stored word of a [`RulingDecision`], matching the published record.
fn decision_word(decision: RulingDecision) -> &'static str {
    match decision {
        RulingDecision::Grant => "grant",
        RulingDecision::OperatorRequired => "operator-required",
    }
}

/// Read a stored decision word.
fn parse_decision(word: &str) -> Result<RulingDecision, VivariumError> {
    Ok(match word {
        "grant" => RulingDecision::Grant,
        "operator-required" => RulingDecision::OperatorRequired,
        other => return Err(unknown_word("decision", other)),
    })
}

/// The stored word of a [`ScopeIssue`], matching the published record.
fn issue_word(issue: ScopeIssue) -> &'static str {
    match issue {
        ScopeIssue::UnknownTarget => "unknown-target",
        ScopeIssue::TargetMismatch => "target-mismatch",
        ScopeIssue::InvalidPath => "invalid-path",
        ScopeIssue::Denied => "denied",
        ScopeIssue::OutsideAllow => "outside-allow",
        ScopeIssue::UnverifiablePath => "unverifiable-path",
    }
}

/// Read a stored issue word.
fn parse_issue(word: &str) -> Result<ScopeIssue, VivariumError> {
    Ok(match word {
        "unknown-target" => ScopeIssue::UnknownTarget,
        "target-mismatch" => ScopeIssue::TargetMismatch,
        "invalid-path" => ScopeIssue::InvalidPath,
        "denied" => ScopeIssue::Denied,
        "outside-allow" => ScopeIssue::OutsideAllow,
        "unverifiable-path" => ScopeIssue::UnverifiablePath,
        other => return Err(unknown_word("issue", other)),
    })
}

fn unknown_word(kind: &str, word: &str) -> VivariumError {
    VivariumError::Other(format!(
        "pump.db scope_journal: unknown {kind} word {word:?}"
    ))
}

/// The journal key of `job`: the original source job with only the
/// envelope-correction suffix stripped.
fn source_key(job: &str) -> Result<String, VivariumError> {
    let key = correct::base_id(job.trim());
    if key.is_empty() {
        return Err(VivariumError::Other(
            "pump.db scope_journal: blank source job".to_string(),
        ));
    }
    Ok(key.to_owned())
}

fn no_row(key: &str) -> VivariumError {
    VivariumError::Other(format!(
        "pump.db scope_journal: no prepared request for source {key}"
    ))
}

fn encode_paths(paths: &[String]) -> Result<String, VivariumError> {
    json(&paths)
}

fn parse_paths(text: &str) -> Result<Vec<String>, VivariumError> {
    serde_json::from_str(text)
        .map_err(|err| VivariumError::Other(format!("pump.db scope_journal: decode paths: {err}")))
}

fn text_error(index: usize, err: VivariumError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(err))
}

fn row_from_row(row: &Row<'_>) -> rusqlite::Result<ScopeJournalRow> {
    let paths: String = row.get(7)?;
    let decision: String = row.get(10)?;
    let issue: Option<String> = row.get(11)?;
    let coordination: Option<String> = row.get(15)?;
    Ok(ScopeJournalRow {
        source: row.get(0)?,
        need: row.get(1)?,
        target: row.get(2)?,
        run: from_u32(row.get(3)?),
        node: row.get(4)?,
        task: row.get(5)?,
        attempt: from_u32(row.get(6)?),
        paths: parse_paths(&paths).map_err(|err| text_error(7, err))?,
        reason: row.get(8)?,
        design: from_u32(row.get(9)?) != 0,
        decision: parse_decision(&decision).map_err(|err| text_error(10, err))?,
        issue: issue
            .map(|word| parse_issue(&word).map_err(|err| text_error(11, err)))
            .transpose()?,
        published_at: row.get(12)?,
        continuation: row.get(13)?,
        round: row.get::<_, Option<i64>>(14)?.map(from_u32),
        coordination: coordination
            .map(|word| parse_coordination(&word).map_err(|err| text_error(15, err)))
            .transpose()?,
    })
}

fn read_row(conn: &Connection, key: &str) -> Result<Option<ScopeJournalRow>, VivariumError> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {COLUMNS} FROM scope_journal WHERE source = ?1"
        ))
        .map_err(db_err("read scope journal"))?;
    stmt.query_row(params![key], row_from_row)
        .optional()
        .map_err(db_err("read scope journal"))
}

fn insert_request(
    conn: &Connection,
    key: &str,
    request: &ScopeRequest,
) -> Result<(), VivariumError> {
    conn.execute(
        "INSERT INTO scope_journal (source, need, target, run, node, task, attempt, \
           paths_json, reason, design, decision, issue) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            key,
            request.need,
            request.target,
            i64::from(request.run),
            request.node,
            request.task,
            i64::from(request.attempt),
            encode_paths(&request.paths)?,
            request.reason,
            i64::from(request.design),
            decision_word(request.decision),
            request.issue.map(issue_word),
        ],
    )
    .map_err(db_err("write scope request"))?;
    Ok(())
}

/// Whether the stored row records the same work as `request`: the identity
/// fields that make a source key name one job of one slice.
fn identity_matches(row: &ScopeJournalRow, request: &ScopeRequest) -> bool {
    row.need == request.need
        && row.target == request.target
        && row.run == request.run
        && row.node == request.node
        && row.task == request.task
        && row.attempt == request.attempt
}

/// Whether the stored row records exactly what `request` asks: identity
/// matched, so only the asked content remains.
fn content_matches(row: &ScopeJournalRow, request: &ScopeRequest) -> bool {
    row.paths == request.paths
        && row.reason == request.reason
        && row.design == request.design
        && row.decision == request.decision
        && row.issue == request.issue
}

fn refresh_request(
    conn: &Connection,
    key: &str,
    request: &ScopeRequest,
) -> Result<(), VivariumError> {
    conn.execute(
        "UPDATE scope_journal SET paths_json = ?2, reason = ?3, design = ?4, \
           decision = ?5, issue = ?6 WHERE source = ?1",
        params![
            key,
            encode_paths(&request.paths)?,
            request.reason,
            i64::from(request.design),
            decision_word(request.decision),
            request.issue.map(issue_word),
        ],
    )
    .map_err(db_err("refresh scope request"))?;
    Ok(())
}

/// Adopt, refresh or refuse a request whose source is already recorded.
fn prepare_existing(
    conn: &Connection,
    key: &str,
    request: &ScopeRequest,
    row: &ScopeJournalRow,
) -> Result<ScopeRequestOutcome, VivariumError> {
    if !identity_matches(row, request) {
        return Err(VivariumError::Other(format!(
            "pump.db scope_journal: source {key} already records different work \
             (need {}, target {}, run {}, node {}, task {}, attempt {})",
            row.need, row.target, row.run, row.node, row.task, row.attempt
        )));
    }
    if content_matches(row, request) {
        return Ok(ScopeRequestOutcome::Adopted);
    }
    if row.published_at.is_some() {
        return Err(VivariumError::Other(format!(
            "pump.db scope_journal: source {key} is published; \
             its decision must not drift"
        )));
    }
    refresh_request(conn, key, request)?;
    Ok(ScopeRequestOutcome::Refreshed)
}

/// Whether `round` is already held by another source of `task` or by an
/// existing step job of the slice.
fn round_taken(
    conn: &Connection,
    task: &str,
    round: u32,
    scope_job_id: &impl Fn(u32) -> String,
) -> Result<bool, VivariumError> {
    let held: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM scope_journal WHERE task = ?1 AND round = ?2",
            params![task, i64::from(round)],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_err("read scope round"))?;
    if held.is_some() {
        return Ok(true);
    }
    let used: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM step_jobs WHERE job = ?1",
            params![scope_job_id(round)],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_err("read step job"))?;
    Ok(used.is_some())
}

/// The next free round of `row`'s task: one past the distinct journaled
/// sources, advanced past held rounds and existing step jobs.
fn allocate_round(
    conn: &Connection,
    row: &ScopeJournalRow,
    scope_job_id: &impl Fn(u32) -> String,
) -> Result<u32, VivariumError> {
    let journaled: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT source) FROM scope_journal \
             WHERE task = ?1 AND round IS NOT NULL",
            params![row.task],
            |row| row.get(0),
        )
        .map_err(db_err("count scope rounds"))?;
    let mut round = from_u32(journaled) + 1;
    while round_taken(conn, &row.task, round, scope_job_id)? {
        round += 1;
    }
    Ok(round)
}

impl PumpStore {
    /// The journal record of `job`'s original source, or `None` when no
    /// request was prepared for it. A correction job id reads its base.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read or decoded.
    pub fn scope_request(&self, job: &str) -> Result<Option<ScopeJournalRow>, VivariumError> {
        read_row(&self.conn, &source_key(job)?)
    }

    /// Journal a blocked-scope request under its original source job.
    ///
    /// The same identity prepared twice adopts the record; the same source
    /// key for different work is refused. Content that differs refreshes the
    /// record while it is genuinely unpublished and is refused once the
    /// decision was published.
    ///
    /// # Errors
    /// Returns [`VivariumError`] for a blank source job, a source key reused
    /// for different work, a published decision that would drift, or a
    /// failed write.
    pub fn prepare_scope_request(
        &self,
        request: &ScopeRequest,
    ) -> Result<ScopeRequestOutcome, VivariumError> {
        let key = source_key(&request.source)?;
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin scope request"))?;
        let outcome = match read_row(&tx, &key)? {
            None => {
                insert_request(&tx, &key, request)?;
                ScopeRequestOutcome::Recorded
            }
            Some(row) => prepare_existing(&tx, &key, request, &row)?,
        };
        tx.commit().map_err(db_err("commit scope request"))?;
        Ok(outcome)
    }

    /// Acknowledge that the ruling of `job`'s request was published as a
    /// pump note at `at` (unix seconds). Acknowledging an acknowledged
    /// publication adopts the first stamp and returns `false`; a fresh
    /// acknowledgement returns `true`.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when no request was prepared or the write
    /// fails.
    pub fn acknowledge_scope_publication(&self, job: &str, at: u64) -> Result<bool, VivariumError> {
        let key = source_key(job)?;
        let changed = self
            .conn
            .execute(
                "UPDATE scope_journal SET published_at = ?2 \
                 WHERE source = ?1 AND published_at IS NULL",
                params![key, i64::try_from(at).unwrap_or(i64::MAX)],
            )
            .map_err(db_err("acknowledge scope publication"))?;
        if changed == 0 {
            let row = read_row(&self.conn, &key)?;
            if row.is_none() {
                return Err(no_row(&key));
            }
            return Ok(false);
        }
        Ok(true)
    }

    /// Reserve `continuation` as the one stable continuation identity of
    /// `job`'s request. Reserving the same identity again adopts it and
    /// returns `false`; a fresh reservation returns `true`; a different
    /// identity for an already-reserved source is refused.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when no request was prepared, a reserved
    /// continuation would drift, or the write fails.
    pub fn reserve_scope_continuation(
        &self,
        job: &str,
        continuation: &str,
    ) -> Result<bool, VivariumError> {
        let key = source_key(job)?;
        let row = self.read_row(&key)?.ok_or_else(|| no_row(&key))?;
        match &row.continuation {
            Some(reserved) if reserved == continuation => Ok(false),
            Some(reserved) => Err(VivariumError::Other(format!(
                "pump.db scope_journal: source {key} already reserves \
                 continuation {reserved:?}; a reserved continuation must not drift"
            ))),
            None => {
                self.conn
                    .execute(
                        "UPDATE scope_journal SET continuation = ?2 WHERE source = ?1",
                        params![key, continuation],
                    )
                    .map_err(db_err("reserve scope continuation"))?;
                Ok(true)
            }
        }
    }

    /// Reserve the next hand scope round of `job`'s request: one past the
    /// count of distinct journaled sources of the task, advanced past rounds
    /// another source already holds and past rounds whose step job
    /// `scope_job_id(round)` already exists. Reserving twice for one source
    /// adopts the stored round. The allocation runs in one transaction.
    ///
    /// `scope_job_id` builds the would-be re-dispatch step job id of a
    /// candidate round; the journal never parses dispatch ids itself.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when no request was prepared or the
    /// transaction fails.
    pub fn reserve_scope_round(
        &self,
        job: &str,
        scope_job_id: impl Fn(u32) -> String,
    ) -> Result<u32, VivariumError> {
        let key = source_key(job)?;
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin scope round"))?;
        let row = read_row(&tx, &key)?.ok_or_else(|| no_row(&key))?;
        if let Some(round) = row.round {
            return Ok(round);
        }
        let round = allocate_round(&tx, &row, &scope_job_id)?;
        tx.execute(
            "UPDATE scope_journal SET round = ?2 WHERE source = ?1",
            params![key, i64::from(round)],
        )
        .map_err(db_err("reserve scope round"))?;
        tx.commit().map_err(db_err("commit scope round"))?;
        Ok(round)
    }

    /// Record how slice coordination settled `job`'s request. The latest
    /// outcome supersedes an earlier one; the payloads live in their own
    /// tables.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when no request was prepared or the write
    /// fails.
    pub fn record_scope_coordination(
        &self,
        job: &str,
        result: ScopeCoordination,
    ) -> Result<(), VivariumError> {
        let key = source_key(job)?;
        let changed = self
            .conn
            .execute(
                "UPDATE scope_journal SET coordination = ?2 WHERE source = ?1",
                params![key, coordination_word(result)],
            )
            .map_err(db_err("record scope coordination"))?;
        if changed == 0 {
            return Err(no_row(&key));
        }
        Ok(())
    }

    /// The latest recorded request of `task` that still waits on the slices
    /// holding its requested paths, newest first, or `None`.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read or decoded.
    pub fn waiting_scope_request(
        &self,
        task: &str,
    ) -> Result<Option<ScopeJournalRow>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {COLUMNS} FROM scope_journal \
                 WHERE task = ?1 AND coordination = ?2 ORDER BY rowid DESC LIMIT 1"
            ))
            .map_err(db_err("read waiting scope request"))?;
        let row = stmt
            .query_row(
                params![task, coordination_word(ScopeCoordination::WaitOn)],
                row_from_row,
            )
            .optional()
            .map_err(db_err("read waiting scope request"))?;
        Ok(row)
    }

    /// The latest step-job identity recorded for `task` in `step_jobs`, with
    /// only the envelope-correction suffix stripped; `None` when the slice
    /// has no recorded step job. Read-only: a caller adopting a legacy wait
    /// validates the job's kind through its [`crate::pump::jobs::JobStore`],
    /// not here.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn latest_hand_step_job(&self, task: &str) -> Result<Option<String>, VivariumError> {
        let job: Option<String> = self
            .conn
            .query_row(
                "SELECT job FROM step_jobs WHERE task = ?1 ORDER BY rowid DESC LIMIT 1",
                params![task],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err("read latest hand step job"))?;
        Ok(job.map(|job| correct::base_id(&job).to_owned()))
    }

    fn read_row(&self, key: &str) -> Result<Option<ScopeJournalRow>, VivariumError> {
        read_row(&self.conn, key)
    }
}

mod coordination;

#[cfg(test)]
#[path = "scope_ruling_test.rs"]
mod tests;
