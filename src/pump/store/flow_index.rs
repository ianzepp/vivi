//! The flow run index: tables beside the v8 schema, and run/step/step-job CRUD.
//!
//! The tables are created with `IF NOT EXISTS` on every open and are not part
//! of the `State` save, so `save()` and `clear()` never touch them. The schema
//! version is not bumped here.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{PumpStore, db_err};
use crate::error::VivariumError;
use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState, run_state};
use crate::pump::flow::scope::ScopeState;

/// The error text for a run state word outside the one vocabulary.
fn unknown_run_state(word: &str) -> String {
    format!(
        "run state: expected one of {}; actual: {word}",
        run_state::ALL.join(", ")
    )
}

fn check_run_state(word: &str) -> Result<(), VivariumError> {
    if run_state::is_known(word) {
        Ok(())
    } else {
        Err(VivariumError::Other(unknown_run_state(word)))
    }
}

const FLOW_DDL: &str = "
CREATE TABLE IF NOT EXISTS flow_runs (
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    flow TEXT NOT NULL,
    flow_version INTEGER NOT NULL,
    kind TEXT NOT NULL,
    entry TEXT NOT NULL,
    state TEXT NOT NULL,
    state_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (need, run)
);
CREATE TABLE IF NOT EXISTS flow_steps (
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    node TEXT NOT NULL,
    task TEXT NOT NULL,
    state TEXT NOT NULL,
    verdict TEXT,
    PRIMARY KEY (task)
);
CREATE INDEX IF NOT EXISTS flow_steps_run ON flow_steps (need, run);
CREATE TABLE IF NOT EXISTS step_jobs (
    job TEXT NOT NULL PRIMARY KEY,
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    node TEXT NOT NULL,
    task TEXT NOT NULL,
    attempt INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS step_jobs_task ON step_jobs (task);
CREATE TABLE IF NOT EXISTS slice_scope (
    task TEXT NOT NULL PRIMARY KEY,
    run INTEGER NOT NULL,
    write_scope_json TEXT NOT NULL,
    scope_requests INTEGER NOT NULL DEFAULT 0,
    scope_state TEXT NOT NULL DEFAULT 'none',
    holders TEXT NOT NULL DEFAULT '[]'
);
CREATE TABLE IF NOT EXISTS need_branches (
    need TEXT NOT NULL PRIMARY KEY,
    branch TEXT NOT NULL,
    base TEXT,
    status TEXT NOT NULL,
    record_json TEXT
);
CREATE TABLE IF NOT EXISTS rung_runs (
    task TEXT NOT NULL,
    rung TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    rc INTEGER,
    PRIMARY KEY (task, rung, started_at)
);
CREATE TABLE IF NOT EXISTS applied_actions (
    letter_id TEXT NOT NULL PRIMARY KEY,
    at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS main_state (
    key TEXT NOT NULL PRIMARY KEY,
    value TEXT NOT NULL
);
";

/// Create the flow tables when they are missing.
pub(super) fn ensure_tables(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(FLOW_DDL)
        .map_err(db_err("create flow tables"))
}

fn state_word(state: StepState) -> &'static str {
    match state {
        StepState::Pending => "pending",
        StepState::Running => "running",
        StepState::Waiting => "waiting",
        StepState::Done => "done",
        StepState::Failed => "failed",
    }
}

fn parse_state(word: &str) -> Result<StepState, VivariumError> {
    Ok(match word {
        "pending" => StepState::Pending,
        "running" => StepState::Running,
        "waiting" => StepState::Waiting,
        "done" => StepState::Done,
        "failed" => StepState::Failed,
        other => {
            return Err(VivariumError::Other(format!(
                "pump.db flow_steps: unknown step state {other:?}"
            )));
        }
    })
}

fn run_from_row(row: &Row<'_>) -> rusqlite::Result<RunRow> {
    let state: String = row.get(6)?;
    if !run_state::is_known(&state) {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            unknown_run_state(&state).into(),
        ));
    }
    Ok(RunRow {
        need: row.get(0)?,
        run: row.get(1)?,
        flow: row.get(2)?,
        flow_version: row.get(3)?,
        kind: row.get(4)?,
        entry: row.get(5)?,
        state,
    })
}

fn step_from_row(row: &Row<'_>) -> rusqlite::Result<(StepRow, String)> {
    let word: String = row.get(3)?;
    Ok((
        StepRow {
            run: row.get(0)?,
            node: row.get(1)?,
            task: row.get(2)?,
            state: StepState::Pending,
            verdict: row.get(4)?,
        },
        word,
    ))
}

fn finish_step((mut step, word): (StepRow, String)) -> Result<StepRow, VivariumError> {
    step.state = parse_state(&word)?;
    Ok(step)
}

const RUN_COLUMNS: &str = "need, run, flow, flow_version, kind, entry, state";
const STEP_COLUMNS: &str = "run, node, task, state, verdict";

impl PumpStore {
    /// Insert a run row.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written or already exists.
    pub fn insert_run(&self, row: &RunRow) -> Result<(), VivariumError> {
        check_run_state(&row.state)?;
        self.conn
            .execute(
                "INSERT INTO flow_runs (need, run, flow, flow_version, kind, entry, state, state_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, CAST(strftime('%s', 'now') AS INTEGER))",
                params![
                    row.need,
                    row.run,
                    row.flow,
                    row.flow_version,
                    row.kind,
                    row.entry,
                    row.state
                ],
            )
            .map_err(db_err("insert flow run"))?;
        Ok(())
    }

    /// Every run whose state is [`run_state::LIVE`], ordered by need then run.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn live_runs(&self) -> Result<Vec<RunRow>, VivariumError> {
        self.runs_in_state(run_state::LIVE)
    }

    /// Every run left `finishing`, ordered by need then run.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn finishing_runs(&self) -> Result<Vec<RunRow>, VivariumError> {
        self.runs_in_state(run_state::FINISHING)
    }

    /// Every run in `state`, ordered by need then run.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn runs_in_state(&self, state: &str) -> Result<Vec<RunRow>, VivariumError> {
        self.query_runs(
            &format!("SELECT {RUN_COLUMNS} FROM flow_runs WHERE state = ?1 ORDER BY need, run"),
            params![state],
        )
    }

    /// When the run last changed state (unix seconds), or `None` for no such run.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn run_state_at(&self, need: &str, run: u32) -> Result<Option<i64>, VivariumError> {
        self.conn
            .query_row(
                "SELECT state_at FROM flow_runs WHERE need = ?1 AND run = ?2",
                params![need, run],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err("read flow run state time"))
    }

    /// Every run of one need, oldest first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn runs_of(&self, need: &str) -> Result<Vec<RunRow>, VivariumError> {
        self.query_runs(
            &format!("SELECT {RUN_COLUMNS} FROM flow_runs WHERE need = ?1 ORDER BY run"),
            params![need],
        )
    }

    fn query_runs(
        &self,
        sql: &str,
        args: impl rusqlite::Params,
    ) -> Result<Vec<RunRow>, VivariumError> {
        let mut stmt = self.conn.prepare(sql).map_err(db_err("read flow runs"))?;
        let rows = stmt
            .query_map(args, run_from_row)
            .map_err(db_err("read flow runs"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err("read flow runs"))?;
        Ok(rows)
    }

    /// Set the state of one run.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the update fails.
    pub fn set_run_state(&self, need: &str, run: u32, state: &str) -> Result<(), VivariumError> {
        check_run_state(state)?;
        self.conn
            .execute(
                "UPDATE flow_runs SET state = ?3, state_at = CAST(strftime('%s', 'now') AS INTEGER) \
                 WHERE need = ?1 AND run = ?2",
                params![need, run, state],
            )
            .map_err(db_err("set flow run state"))?;
        Ok(())
    }

    /// One run of `need`.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the run does not exist or cannot be read.
    pub fn run_row(&self, need: &str, run: u32) -> Result<RunRow, VivariumError> {
        self.query_runs(
            &format!("SELECT {RUN_COLUMNS} FROM flow_runs WHERE need = ?1 AND run = ?2"),
            params![need, run],
        )?
        .into_iter()
        .next()
        .ok_or_else(|| {
            VivariumError::Other(format!(
                "need {need}: expected run r{run}; actual: no such run"
            ))
        })
    }

    /// Insert a step row through the run it belongs to.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row's run differs from `run`, or
    /// when [`Self::insert_step`] fails.
    pub fn insert_run_step(&self, run: &RunRow, row: &StepRow) -> Result<(), VivariumError> {
        if row.run != run.run {
            return Err(VivariumError::Other(format!(
                "step {}: expected run r{}; actual: row run r{}",
                row.task, run.run, row.run
            )));
        }
        self.insert_step(&run.need, row)
    }

    /// Insert a step row of `need`'s run.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written or the task
    /// already has a step.
    pub fn insert_step(&self, need: &str, row: &StepRow) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "INSERT INTO flow_steps (need, run, node, task, state, verdict) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    need,
                    row.run,
                    row.node,
                    row.task,
                    state_word(row.state),
                    row.verdict
                ],
            )
            .map_err(db_err("insert flow step"))?;
        Ok(())
    }

    /// The steps of one run, in insertion order.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn steps_of(&self, need: &str, run: u32) -> Result<Vec<StepRow>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {STEP_COLUMNS} FROM flow_steps WHERE need = ?1 AND run = ?2 \
                 ORDER BY rowid"
            ))
            .map_err(db_err("read flow steps"))?;
        let rows = stmt
            .query_map(params![need, run], step_from_row)
            .map_err(db_err("read flow steps"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err("read flow steps"))?;
        rows.into_iter().map(finish_step).collect()
    }

    /// The step whose task handle is `task`.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn step_by_task(&self, task: &str) -> Result<Option<StepRow>, VivariumError> {
        self.conn
            .query_row(
                &format!("SELECT {STEP_COLUMNS} FROM flow_steps WHERE task = ?1"),
                params![task],
                step_from_row,
            )
            .optional()
            .map_err(db_err("read flow step"))?
            .map(finish_step)
            .transpose()
    }

    /// Set the state and verdict of the step with this task.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the update fails.
    pub fn set_step_state(
        &self,
        task: &str,
        state: StepState,
        verdict: Option<&str>,
    ) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "UPDATE flow_steps SET state = ?2, verdict = ?3 WHERE task = ?1",
                params![task, state_word(state), verdict],
            )
            .map_err(db_err("set flow step state"))?;
        Ok(())
    }

    /// Insert a job attempt row of `need`'s run.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written or the job
    /// already has a row.
    pub fn insert_step_job(&self, need: &str, row: &StepJobRow) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "INSERT INTO step_jobs (job, need, run, node, task, attempt) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![row.job, need, row.run, row.node, row.task, row.attempt],
            )
            .map_err(db_err("insert step job"))?;
        Ok(())
    }

    /// The step-job row for a pool job id.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn step_job(&self, job: &str) -> Result<Option<StepJobRow>, VivariumError> {
        self.conn
            .query_row(
                "SELECT job, run, node, task, attempt FROM step_jobs WHERE job = ?1",
                params![job],
                |row| {
                    Ok(StepJobRow {
                        job: row.get(0)?,
                        run: row.get(1)?,
                        node: row.get(2)?,
                        task: row.get(3)?,
                        attempt: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(db_err("read step job"))
    }

    /// The highest attempt number the step task has had (0 when none).
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the attempt cannot be read.
    pub fn last_attempt(&self, task: &str) -> Result<u32, VivariumError> {
        self.conn
            .query_row(
                "SELECT COALESCE(MAX(attempt), 0) FROM step_jobs WHERE task = ?1",
                params![task],
                |row| row.get(0),
            )
            .map_err(db_err("read last step attempt"))
    }

    /// The attempt number a new job of the step task takes.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the attempt cannot be read.
    pub fn next_attempt(&self, task: &str) -> Result<u32, VivariumError> {
        Ok(self.last_attempt(task)? + 1)
    }

    /// How many job rows the step task has (counts corrections and requeues).
    ///
    /// Kept only until the tests outside this slice's scope move to
    /// [`Self::last_attempt`]; use that instead.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the count cannot be read.
    pub fn attempts_of(&self, task: &str) -> Result<u32, VivariumError> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM step_jobs WHERE task = ?1",
                params![task],
                |row| row.get(0),
            )
            .map_err(db_err("count step attempts"))
    }
}

impl PumpStore {
    /// Store the write scope of a slice task of `run`.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written or the task
    /// already has a scope.
    pub fn insert_slice_scope(
        &self,
        task: &str,
        run: u32,
        write_scope: &[String],
    ) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "INSERT INTO slice_scope (task, run, write_scope_json) VALUES (?1, ?2, ?3)",
                params![task, run, json_list(write_scope)?],
            )
            .map_err(db_err("insert slice scope"))?;
        Ok(())
    }

    /// The current (possibly widened) write scope of a slice task.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn slice_scope(&self, task: &str) -> Result<Option<Vec<String>>, VivariumError> {
        self.scope_text(task, "write_scope_json")?
            .map(|text| parse_list(&text))
            .transpose()
    }

    /// Add `paths` to a slice's write scope; paths already present are kept once.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the task has no scope row or the update fails.
    pub fn widen_slice_scope(&self, task: &str, paths: &[String]) -> Result<(), VivariumError> {
        let mut scope = self.slice_scope(task)?.ok_or_else(|| no_scope(task))?;
        for path in paths {
            if !scope.contains(path) {
                scope.push(path.clone());
            }
        }
        self.update_scope_column(task, "write_scope_json", &json_list(&scope)?)
    }

    /// Count one more blocked-scope request on the task; returns the new count.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the task has no scope row or the update fails.
    pub fn bump_scope_requests(&self, task: &str) -> Result<u32, VivariumError> {
        let changed = self
            .conn
            .execute(
                "UPDATE slice_scope SET scope_requests = scope_requests + 1 WHERE task = ?1",
                params![task],
            )
            .map_err(db_err("bump scope requests"))?;
        if changed == 0 {
            return Err(no_scope(task));
        }
        self.conn
            .query_row(
                "SELECT scope_requests FROM slice_scope WHERE task = ?1",
                params![task],
                |row| row.get(0),
            )
            .map_err(db_err("read scope requests"))
    }

    /// Set the blocked-scope state of a slice task.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the task has no scope row or the update fails.
    pub fn set_scope_state(&self, task: &str, state: ScopeState) -> Result<(), VivariumError> {
        self.update_scope_column(task, "scope_state", state.word())
    }

    /// The blocked-scope state of a slice task.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn scope_state(&self, task: &str) -> Result<Option<ScopeState>, VivariumError> {
        self.scope_text(task, "scope_state")?
            .map(|word| ScopeState::parse(&word))
            .transpose()
    }

    /// Record the slice tasks a blocked slice waits on.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the task has no scope row or the update fails.
    pub fn set_scope_holders(&self, task: &str, holders: &[String]) -> Result<(), VivariumError> {
        self.update_scope_column(task, "holders", &json_list(holders)?)
    }

    /// The slice tasks a blocked slice waits on.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn scope_holders(&self, task: &str) -> Result<Vec<String>, VivariumError> {
        self.scope_text(task, "holders")?
            .map_or_else(|| Ok(Vec::new()), |text| parse_list(&text))
    }

    /// The write scopes of the other slices of `task`'s run whose step is not
    /// closed (any state but done or failed), as `(task, write scope)` pairs.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn open_scopes_of_run(
        &self,
        task: &str,
        run: u32,
    ) -> Result<Vec<(String, Vec<String>)>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT s.task, s.write_scope_json FROM slice_scope s \
                 JOIN flow_steps f ON f.task = s.task \
                 WHERE s.task != ?1 AND s.run = ?2 AND f.run = ?2 \
                   AND f.need = (SELECT need FROM flow_steps WHERE task = ?1) \
                   AND f.state NOT IN ('done', 'failed') \
                 ORDER BY f.rowid",
            )
            .map_err(db_err("read open slice scopes"))?;
        let rows = stmt
            .query_map(params![task, run], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(db_err("read open slice scopes"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err("read open slice scopes"))?;
        rows.into_iter()
            .map(|(task, text)| Ok((task, parse_list(&text)?)))
            .collect()
    }

    fn scope_text(&self, task: &str, column: &str) -> Result<Option<String>, VivariumError> {
        self.conn
            .query_row(
                &format!("SELECT {column} FROM slice_scope WHERE task = ?1"),
                params![task],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err("read slice scope"))
    }

    fn update_scope_column(
        &self,
        task: &str,
        column: &str,
        value: &str,
    ) -> Result<(), VivariumError> {
        let changed = self
            .conn
            .execute(
                &format!("UPDATE slice_scope SET {column} = ?2 WHERE task = ?1"),
                params![task, value],
            )
            .map_err(db_err("update slice scope"))?;
        if changed == 0 {
            return Err(no_scope(task));
        }
        Ok(())
    }
}

impl PumpStore {
    /// The ids of the operator action letters the flow engine has applied.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the ids cannot be read.
    pub fn applied_action_ids(&self) -> Result<BTreeSet<String>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare("SELECT letter_id FROM applied_actions")
            .map_err(db_err("read applied actions"))?;
        stmt.query_map([], |row| row.get::<_, String>(0))
            .map_err(db_err("read applied actions"))?
            .collect::<Result<BTreeSet<_>, _>>()
            .map_err(db_err("read applied actions"))
    }

    /// Record an action letter as applied at `at` (unix seconds). Returns
    /// false when it was already recorded.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn mark_action_applied(&self, letter_id: &str, at: u64) -> Result<bool, VivariumError> {
        let at = i64::try_from(at).unwrap_or(i64::MAX);
        let changed = self
            .conn
            .execute(
                "INSERT OR IGNORE INTO applied_actions (letter_id, at) VALUES (?1, ?2)",
                params![letter_id, at],
            )
            .map_err(db_err("mark action applied"))?;
        Ok(changed > 0)
    }
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
#[path = "flow_index_test.rs"]
mod tests;

#[cfg(test)]
#[path = "flow_round_trip_test.rs"]
mod round_trip_tests;
