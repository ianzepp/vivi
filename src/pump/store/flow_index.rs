//! The flow run index: tables beside the v8 schema, and run/step/step-job CRUD.
//!
//! The tables are created with `IF NOT EXISTS` on every open and are not part
//! of the `State` save, so `save()` and `clear()` never touch them. The schema
//! version is not bumped here.

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{PumpStore, db_err};
use crate::error::VivariumError;
use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState};

/// The run state of a run that is still being advanced. Any other value is a
/// finished run.
pub const RUN_LIVE: &str = "live";

const FLOW_DDL: &str = "
CREATE TABLE IF NOT EXISTS flow_runs (
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    flow TEXT NOT NULL,
    flow_version INTEGER NOT NULL,
    kind TEXT NOT NULL,
    entry TEXT NOT NULL,
    state TEXT NOT NULL,
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
    write_scope_json TEXT NOT NULL
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
    Ok(RunRow {
        need: row.get(0)?,
        run: row.get(1)?,
        flow: row.get(2)?,
        flow_version: row.get(3)?,
        kind: row.get(4)?,
        entry: row.get(5)?,
        state: row.get(6)?,
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
        self.conn
            .execute(
                "INSERT INTO flow_runs (need, run, flow, flow_version, kind, entry, state) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
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

    /// Every run whose state is [`RUN_LIVE`], ordered by need then run.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn live_runs(&self) -> Result<Vec<RunRow>, VivariumError> {
        self.query_runs(
            &format!("SELECT {RUN_COLUMNS} FROM flow_runs WHERE state = ?1 ORDER BY need, run"),
            params![RUN_LIVE],
        )
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
        self.conn
            .execute(
                "UPDATE flow_runs SET state = ?3 WHERE need = ?1 AND run = ?2",
                params![need, run, state],
            )
            .map_err(db_err("set flow run state"))?;
        Ok(())
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

    /// How many job attempts the step task has had.
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

#[cfg(test)]
#[path = "flow_index_test.rs"]
mod tests;
