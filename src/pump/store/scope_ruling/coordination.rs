//! Atomic slice-coordination commits over the scope workflow journal.
//!
//! Settling a blocked-scope request moves four `slice_scope` fields (the
//! request counter, the widened write scope, the holders record, the scope
//! state) plus the journal's coordination record. Done as separate calls, an
//! interrupted replay counts one request twice or leaves partial state. This
//! module makes the settlement one transaction keyed by the request's source
//! job:
//!
//! 1. [`PumpStore::scope_coordination_snapshot`] reads the counted requests
//!    and the run's open scopes without writing, so the resolver computes its
//!    first candidate without incrementing anything.
//! 2. [`PumpStore::commit_scope_coordination`] adopts an already-recorded
//!    result for the same source (changing nothing) or commits the candidate
//!    — counter bump, widened free paths, holder/path record, scope state,
//!    journal record — atomically. A failed transaction leaves every field
//!    as it was.
//!
//! The recorded [`ScopeCoordinationResult`] persists the full resolution —
//! the widened free paths, the waited-on holders, the held requested paths,
//! or the rework reason — so a replaying caller can redo the external
//! dependency writes (the wait-on mail) from the adopted result. No mail
//! belongs inside the transaction; the journal row must already exist (see
//! [`super::ScopeRequest`]'s prepare), and the recorded word stays the plain
//! [`ScopeCoordination`] encoding that `waiting_scope_request` filters on.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::super::{PumpStore, db_err, from_u32, json};
use super::{
    ScopeCoordination, ScopeJournalRow, ScopeRequest, coordination_word, identity_matches, no_row,
    read_row, source_key,
};
use crate::error::VivariumError;
use crate::pump::flow::scope::{HELD_PATH_PREFIX, Resolution, ScopeState};

/// The typed slice-coordination result stored beside a journal row: the
/// resolution plus the held-path half of the wait record. `held` is empty
/// unless the resolution waits on holders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeCoordinationResult {
    /// How the request was settled: the widened free paths, the holder tasks
    /// waited on, or the rework reason.
    pub resolution: Resolution,
    /// The requested paths some open slice holds; recorded as
    /// `{HELD_PATH_PREFIX}{path}` entries of the holders record.
    #[serde(default)]
    pub held: Vec<String>,
}

impl ScopeCoordinationResult {
    /// The journal outcome word this result records.
    fn outcome(&self) -> ScopeCoordination {
        match &self.resolution {
            Resolution::Widen(_) => ScopeCoordination::Widen,
            Resolution::WaitOn(_) => ScopeCoordination::WaitOn,
            Resolution::Rework(_) => ScopeCoordination::Rework,
        }
    }

    /// The free paths the result widens the slice's scope with.
    fn free(&self) -> &[String] {
        match &self.resolution {
            Resolution::Widen(free) => free,
            _ => &[],
        }
    }

    /// The slice tasks the result waits on.
    fn holders(&self) -> &[String] {
        match &self.resolution {
            Resolution::WaitOn(holders) => holders,
            _ => &[],
        }
    }
}

/// What [`PumpStore::commit_scope_coordination`] did with the candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeCoordinationOutcome {
    /// The candidate was committed atomically; the caller now performs the
    /// external dependency writes (the wait-on mail).
    Committed(ScopeCoordinationResult),
    /// The source already settled: nothing changed and the recorded result
    /// is returned for the replay.
    Adopted(ScopeCoordinationResult),
}

impl ScopeCoordinationOutcome {
    /// The settled result, recorded or adopted.
    #[must_use]
    pub fn resolution(&self) -> &ScopeCoordinationResult {
        match self {
            Self::Committed(result) | Self::Adopted(result) => result,
        }
    }
}

/// The read-only state a first coordination candidate is computed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeCoordinationSnapshot {
    /// The blocked-scope requests already counted on the slice task.
    pub requests: u32,
    /// The write scopes of the run's other open slices, as `(task, scope)`.
    pub others: Vec<(String, Vec<String>)>,
}

impl PumpStore {
    /// The read-only coordination state of `task` in `run`: the counted
    /// blocked-scope requests and the run's other open slices' scopes.
    /// Writes nothing, so the first candidate is computed outside any
    /// transaction.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the task has no scope row or a read
    /// fails.
    pub fn scope_coordination_snapshot(
        &self,
        task: &str,
        run: u32,
    ) -> Result<ScopeCoordinationSnapshot, VivariumError> {
        Ok(ScopeCoordinationSnapshot {
            requests: read_requests(&self.conn, task)?,
            others: self.open_scopes_of_run(task, run)?,
        })
    }

    /// Settle `candidate` for `request`'s source in one transaction: adopt
    /// the recorded result of an already-settled source (count and scope
    /// unchanged), or commit the candidate — request-counter bump, widened
    /// free paths, holder/path record, scope state, journal record — as a
    /// unit. The journal row must already exist; a mismatching request
    /// identity is refused.
    ///
    /// The per-outcome scope state mirrors the resolver: `WaitOn` sets
    /// `scope-wait`, `Rework` sets `scope-rework`, `Widen` leaves the state
    /// to the re-dispatch. No mail happens here.
    ///
    /// # Errors
    /// Returns [`VivariumError`] for a blank source job, an unprepared
    /// source, a mismatching request identity, a recorded result without a
    /// typed payload, or a failed transaction — which leaves every field
    /// unchanged.
    pub fn commit_scope_coordination(
        &self,
        request: &ScopeRequest,
        candidate: &ScopeCoordinationResult,
    ) -> Result<ScopeCoordinationOutcome, VivariumError> {
        let key = source_key(&request.source)?;
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin scope coordination"))?;
        let row = read_row(&tx, &key)?.ok_or_else(|| no_row(&key))?;
        if !identity_matches(&row, request) {
            return Err(identity_mismatch(&key, &row));
        }
        if let Some(recorded) = recorded_result(&tx, &key, &row)? {
            return Ok(ScopeCoordinationOutcome::Adopted(recorded));
        }
        apply_candidate(&tx, &row.task, candidate)?;
        record_result(&tx, &key, candidate)?;
        tx.commit().map_err(db_err("commit scope coordination"))?;
        Ok(ScopeCoordinationOutcome::Committed(candidate.clone()))
    }
}

fn identity_mismatch(key: &str, row: &ScopeJournalRow) -> VivariumError {
    VivariumError::Other(format!(
        "pump.db scope_journal: source {key} cannot coordinate for different work \
         (need {}, target {}, run {}, node {}, task {}, attempt {})",
        row.need, row.target, row.run, row.node, row.task, row.attempt
    ))
}

/// The result recorded for `key`, or `None` while the source is unsettled.
/// A word without its typed payload (written by the word-only recorder)
/// cannot be replayed and is refused.
fn recorded_result(
    conn: &Connection,
    key: &str,
    row: &ScopeJournalRow,
) -> Result<Option<ScopeCoordinationResult>, VivariumError> {
    if row.coordination.is_none() {
        return Ok(None);
    }
    let payload: Option<String> = conn
        .query_row(
            "SELECT coordination_json FROM scope_journal WHERE source = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_err("read scope coordination payload"))?;
    let text = payload.ok_or_else(|| {
        VivariumError::Other(format!(
            "pump.db scope_journal: source {key} records coordination without \
             a typed result; it cannot be adopted"
        ))
    })?;
    let result: ScopeCoordinationResult = serde_json::from_str(&text).map_err(|err| {
        VivariumError::Other(format!(
            "pump.db scope_journal: decode coordination payload: {err}"
        ))
    })?;
    if Some(coordination_word(result.outcome())) != row.coordination.map(coordination_word) {
        return Err(VivariumError::Other(format!(
            "pump.db scope_journal: source {key} records a coordination word \
             that disagrees with its typed result"
        )));
    }
    Ok(Some(result))
}

/// Apply every `slice_scope` write of `candidate` to `task`, in the
/// resolver's order: counter, widened free paths, then the per-outcome
/// wait/rework record.
fn apply_candidate(
    conn: &Connection,
    task: &str,
    candidate: &ScopeCoordinationResult,
) -> Result<(), VivariumError> {
    bump_requests(conn, task)?;
    let free = candidate.free();
    if !free.is_empty() {
        widen_scope(conn, task, free)?;
    }
    match candidate.outcome() {
        ScopeCoordination::WaitOn => {
            let mut record: Vec<String> = candidate.holders().to_vec();
            record.extend(
                candidate
                    .held
                    .iter()
                    .map(|path| format!("{HELD_PATH_PREFIX}{path}")),
            );
            write_scope_column(conn, task, "holders", &json_list(&record)?)?;
            set_state(conn, task, ScopeState::Wait)?;
        }
        ScopeCoordination::Rework => set_state(conn, task, ScopeState::Rework)?,
        ScopeCoordination::Widen => {}
    }
    Ok(())
}

/// Record `candidate`'s word and typed payload on the journal row. Runs
/// last: every scope write before it can only fail the whole transaction.
fn record_result(
    conn: &Connection,
    key: &str,
    candidate: &ScopeCoordinationResult,
) -> Result<(), VivariumError> {
    let changed = conn
        .execute(
            "UPDATE scope_journal SET coordination = ?2, coordination_json = ?3 \
             WHERE source = ?1",
            params![
                key,
                coordination_word(candidate.outcome()),
                json(candidate)?
            ],
        )
        .map_err(db_err("record scope coordination"))?;
    if changed == 0 {
        return Err(no_row(key));
    }
    Ok(())
}

fn bump_requests(conn: &Connection, task: &str) -> Result<(), VivariumError> {
    changed_scope_row(
        conn.execute(
            "UPDATE slice_scope SET scope_requests = scope_requests + 1 WHERE task = ?1",
            params![task],
        )
        .map_err(db_err("bump scope requests"))?,
        task,
    )
}

fn read_requests(conn: &Connection, task: &str) -> Result<u32, VivariumError> {
    let requests: Option<i64> = conn
        .query_row(
            "SELECT scope_requests FROM slice_scope WHERE task = ?1",
            params![task],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_err("read scope requests"))?;
    requests.map(from_u32).ok_or_else(|| no_scope(task))
}

fn widen_scope(conn: &Connection, task: &str, free: &[String]) -> Result<(), VivariumError> {
    let scope = read_write_scope(conn, task)?;
    let mut widened = scope.clone();
    for path in free {
        if !widened.contains(path) {
            widened.push(path.clone());
        }
    }
    write_scope_column(conn, task, "write_scope_json", &json_list(&widened)?)
}

fn read_write_scope(conn: &Connection, task: &str) -> Result<Vec<String>, VivariumError> {
    let text: Option<String> = conn
        .query_row(
            "SELECT write_scope_json FROM slice_scope WHERE task = ?1",
            params![task],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_err("read slice scope"))?;
    let text = text.ok_or_else(|| no_scope(task))?;
    parse_list(&text)
}

fn set_state(conn: &Connection, task: &str, state: ScopeState) -> Result<(), VivariumError> {
    write_scope_column(conn, task, "scope_state", state.word())
}

fn write_scope_column(
    conn: &Connection,
    task: &str,
    column: &str,
    value: &str,
) -> Result<(), VivariumError> {
    let changed = conn
        .execute(
            &format!("UPDATE slice_scope SET {column} = ?2 WHERE task = ?1"),
            params![task, value],
        )
        .map_err(db_err("update slice scope"))?;
    changed_scope_row(changed, task)
}

fn changed_scope_row(changed: usize, task: &str) -> Result<(), VivariumError> {
    if changed == 0 {
        return Err(no_scope(task));
    }
    Ok(())
}

fn no_scope(task: &str) -> VivariumError {
    VivariumError::Other(format!("pump.db slice_scope: no scope for task {task}"))
}

fn json_list(items: &[String]) -> Result<String, VivariumError> {
    serde_json::to_string(items)
        .map_err(|e| VivariumError::Other(format!("pump.db slice_scope: encode list: {e}")))
}

fn parse_list(text: &str) -> Result<Vec<String>, VivariumError> {
    serde_json::from_str(text)
        .map_err(|e| VivariumError::Other(format!("pump.db slice_scope: decode list: {e}")))
}

#[cfg(test)]
#[path = "coordination_test.rs"]
mod tests;
