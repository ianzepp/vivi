//! Phase-aware active execution generations for step tasks.
//!
//! One execution is one generation of work on one task of one run under one
//! owning phase. Retries consume the next generation; corrections share the
//! attempt but always mint a new execution identity. The store also keeps the
//! task's current owning phase, so an execution of an old phase cannot become
//! current after the task has moved on (for example once its merge starts).
//!
//! Like the other flow tables, these tables are created with `IF NOT EXISTS`
//! on every open and are not part of the [`State`](crate::pump::state::State)
//! save, so `save()` and `clear()` never touch them and the schema version is
//! not bumped. Historical `StepJobRow` rows are only ever read here (adoption),
//! never rewritten.

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{PumpStore, db_err, from_u32};
use crate::error::VivariumError;

/// The typed key of one execution lineage: a task of one run under one phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionKey {
    pub need: String,
    pub run: u32,
    pub task: String,
    pub phase: String,
}

/// One execution: the `generation`-th run of an [`ExecutionKey`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionRow {
    pub need: String,
    pub run: u32,
    pub task: String,
    pub phase: String,
    pub generation: u32,
    /// The unique execution identity; corrections never share it.
    pub execution: String,
    /// The attempt of the active job, once one is activated.
    pub attempt: Option<u32>,
    /// The job activated on this execution; a superseded execution keeps its
    /// last job so a late result can still be traced to its stale execution.
    pub active_job: Option<String>,
    /// The identity of the first generation of this lineage.
    pub original_execution: String,
    /// The lineage's first job facts, stamped by the first activation.
    pub original_job: Option<String>,
    pub original_attempt: Option<u32>,
    /// How many retries this lineage has consumed so far.
    pub retries: u32,
    /// The superseded execution identities this one was minted from, oldest first.
    pub retry_history: Vec<String>,
    /// The input-message checkpoint the execution has consumed so far.
    pub input_checkpoint: Option<String>,
    pub superseded: bool,
}

/// The task's current owning phase, advanced by the path that takes the task
/// over (for example when a merge starts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPhaseRow {
    pub task: String,
    pub need: String,
    pub run: u32,
    pub phase: String,
}

impl ExecutionKey {
    /// A key from its parts.
    pub fn new(need: &str, run: u32, task: &str, phase: &str) -> Self {
        Self {
            need: need.to_string(),
            run,
            task: task.to_string(),
            phase: phase.to_string(),
        }
    }

    /// The deterministic execution identity of `generation` of this key.
    fn identity(&self, generation: u32) -> String {
        format!(
            "{}:r{}:{}:{}:g{}",
            self.need, self.run, self.task, self.phase, generation
        )
    }
}

impl ExecutionRow {
    /// This row's execution key.
    pub fn key(&self) -> ExecutionKey {
        ExecutionKey {
            need: self.need.clone(),
            run: self.run,
            task: self.task.clone(),
            phase: self.phase.clone(),
        }
    }
}

const DDL: &str = "
CREATE TABLE IF NOT EXISTS step_executions (
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    task TEXT NOT NULL,
    phase TEXT NOT NULL,
    generation INTEGER NOT NULL,
    execution TEXT NOT NULL UNIQUE,
    attempt INTEGER,
    active_job TEXT,
    original_execution TEXT NOT NULL,
    original_job TEXT,
    original_attempt INTEGER,
    retries INTEGER NOT NULL DEFAULT 0,
    retry_history_json TEXT NOT NULL DEFAULT '[]',
    input_checkpoint TEXT,
    superseded INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (need, run, task, phase, generation)
);
CREATE INDEX IF NOT EXISTS step_executions_job ON step_executions(active_job);
CREATE INDEX IF NOT EXISTS step_executions_task ON step_executions(task, generation);
CREATE TABLE IF NOT EXISTS task_phase (
    task TEXT NOT NULL PRIMARY KEY,
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    phase TEXT NOT NULL,
    changed_at INTEGER NOT NULL DEFAULT 0
);
";

/// Create the execution tables when they are missing.
pub(super) fn ensure_tables(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(DDL)
        .map_err(db_err("create step execution tables"))
}

const COLUMNS: &str = "need, run, task, phase, generation, execution, attempt, active_job, \
     original_execution, original_job, original_attempt, retries, retry_history_json, \
     input_checkpoint, superseded, created_at";

fn execution_from_row(row: &Row<'_>) -> rusqlite::Result<(ExecutionRow, String)> {
    Ok((
        ExecutionRow {
            need: row.get(0)?,
            run: row.get(1)?,
            task: row.get(2)?,
            phase: row.get(3)?,
            generation: row.get(4)?,
            execution: row.get(5)?,
            attempt: row.get::<_, Option<i64>>(6)?.map(from_u32),
            active_job: row.get(7)?,
            original_execution: row.get(8)?,
            original_job: row.get(9)?,
            original_attempt: row.get::<_, Option<i64>>(10)?.map(from_u32),
            retries: row.get(11)?,
            retry_history: Vec::new(),
            input_checkpoint: row.get(13)?,
            superseded: row.get::<_, i64>(14)? != 0,
        },
        row.get(12)?,
    ))
}

fn finish_execution(
    (mut execution, history): (ExecutionRow, String),
) -> Result<ExecutionRow, VivariumError> {
    execution.retry_history = serde_json::from_str(&history).map_err(|e| {
        VivariumError::Other(format!(
            "pump.db step_executions: decode history of {}: {e}",
            execution.execution
        ))
    })?;
    Ok(execution)
}

fn now() -> &'static str {
    "CAST(strftime('%s', 'now') AS INTEGER)"
}

/// The generation a new execution of `key` takes.
fn next_generation(conn: &Connection, key: &ExecutionKey) -> Result<u32, VivariumError> {
    conn.query_row(
        "SELECT COALESCE(MAX(generation), 0) FROM step_executions \
         WHERE need = ?1 AND run = ?2 AND task = ?3 AND phase = ?4",
        params![key.need, key.run, key.task, key.phase],
        |row| row.get::<_, i64>(0),
    )
    .map(|max| from_u32(max) + 1)
    .map_err(db_err("read execution generation"))
}

/// The highest attempt `task` already has in `step_jobs`, for adoption.
fn adopted_attempt(conn: &Connection, task: &str) -> Result<Option<u32>, VivariumError> {
    conn.query_row(
        "SELECT COALESCE(MAX(attempt), 0) FROM step_jobs WHERE task = ?1",
        params![task],
        |row| row.get::<_, i64>(0),
    )
    .map(|max| (max > 0).then(|| from_u32(max)))
    .map_err(db_err("read adopted attempt"))
}

/// Stamp the task's owning phase, but only when the task has none yet.
fn stamp_initial_phase(conn: &Connection, key: &ExecutionKey) -> Result<(), VivariumError> {
    conn.execute(
        &format!(
            "INSERT INTO task_phase (task, need, run, phase, changed_at) \
             VALUES (?1, ?2, ?3, ?4, {}) ON CONFLICT(task) DO NOTHING",
            now()
        ),
        params![key.task, key.need, key.run, key.phase],
    )
    .map_err(db_err("stamp task phase"))?;
    Ok(())
}

fn no_such_execution(identity: &str) -> VivariumError {
    VivariumError::Other(format!("execution {identity}: no such execution"))
}

/// Insert the next generation of `current`'s lineage, carrying its
/// original-facts linkage and input checkpoint.
fn insert_next_generation(
    conn: &Connection,
    current: &ExecutionRow,
    generation: u32,
    attempt: Option<u32>,
    retries: u32,
    history: &[String],
) -> Result<(), VivariumError> {
    let key = current.key();
    conn.execute(
        &format!(
            "INSERT INTO step_executions \
               (need, run, task, phase, generation, execution, attempt, \
                original_execution, original_job, original_attempt, retries, \
                retry_history_json, input_checkpoint, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, {})",
            now()
        ),
        params![
            key.need,
            key.run,
            key.task,
            key.phase,
            generation,
            key.identity(generation),
            attempt,
            current.original_execution,
            current.original_job,
            current.original_attempt,
            retries,
            json_history(history)?,
            current.input_checkpoint,
        ],
    )
    .map_err(db_err("mint execution generation"))?;
    Ok(())
}

fn superseded_execution(identity: &str, doing: &str) -> VivariumError {
    VivariumError::Other(format!(
        "execution {identity} is superseded and cannot be {doing}"
    ))
}

impl PumpStore {
    /// Register the execution of `key`, or return its active execution.
    ///
    /// Idempotent: the first call mints generation 1 with a deterministic
    /// identity; later calls return the active execution unchanged. When the
    /// task already has historical `step_jobs` rows, the new execution adopts
    /// their highest attempt as its own without rewriting any row. A task with
    /// no owning phase yet is stamped with `key.phase`.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn register_execution(&self, key: &ExecutionKey) -> Result<ExecutionRow, VivariumError> {
        if let Some(active) = self.active_execution(key)? {
            return Ok(active);
        }
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin execution registration"))?;
        let generation = next_generation(&tx, key)?;
        let execution = key.identity(generation);
        let attempt = adopted_attempt(&tx, &key.task)?;
        tx.execute(
            &format!(
                "INSERT INTO step_executions \
                   (need, run, task, phase, generation, execution, attempt, \
                    original_execution, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?6, {})",
                now()
            ),
            params![
                key.need, key.run, key.task, key.phase, generation, execution, attempt
            ],
        )
        .map_err(db_err("register execution"))?;
        stamp_initial_phase(&tx, key)?;
        tx.commit()
            .map_err(db_err("commit execution registration"))?;
        Ok(ExecutionRow {
            need: key.need.clone(),
            run: key.run,
            task: key.task.clone(),
            phase: key.phase.clone(),
            generation,
            execution,
            attempt,
            active_job: None,
            original_execution: key.identity(generation),
            original_job: None,
            original_attempt: None,
            retries: 0,
            retry_history: Vec::new(),
            input_checkpoint: None,
            superseded: false,
        })
    }

    /// The active (newest, non-superseded) execution of `key`.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn active_execution(
        &self,
        key: &ExecutionKey,
    ) -> Result<Option<ExecutionRow>, VivariumError> {
        self.query_execution(
            &format!(
                "SELECT {COLUMNS} FROM step_executions \
                 WHERE need = ?1 AND run = ?2 AND task = ?3 AND phase = ?4 AND superseded = 0 \
                 ORDER BY generation DESC LIMIT 1"
            ),
            params![key.need, key.run, key.task, key.phase],
        )
    }

    /// The execution with this identity.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn execution(&self, identity: &str) -> Result<Option<ExecutionRow>, VivariumError> {
        self.query_execution(
            &format!("SELECT {COLUMNS} FROM step_executions WHERE execution = ?1"),
            params![identity],
        )
    }

    /// The execution `job` was activated on, current or superseded. Compare
    /// with [`Self::execution_is_current`] before trusting a result.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn execution_by_job(&self, job: &str) -> Result<Option<ExecutionRow>, VivariumError> {
        self.query_execution(
            &format!("SELECT {COLUMNS} FROM step_executions WHERE active_job = ?1"),
            params![job],
        )
    }

    fn query_execution(
        &self,
        sql: &str,
        args: impl rusqlite::Params,
    ) -> Result<Option<ExecutionRow>, VivariumError> {
        self.conn
            .query_row(sql, args, execution_from_row)
            .optional()
            .map_err(db_err("read step execution"))?
            .map(finish_execution)
            .transpose()
    }

    /// Make `job` (at `attempt`) the active job of the execution `identity`.
    ///
    /// Guarded for the enqueue paths: refused when the execution is superseded,
    /// when the task's current owning phase has moved past the execution's
    /// phase, or when a newer generation of the same key already holds an
    /// active job at the same or a higher attempt (a stale same-attempt
    /// activation). Activating the already-active job is idempotent. The first
    /// activation stamps the lineage's original job facts.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the execution is unknown or stale, or
    /// the row cannot be written.
    pub fn activate_job(
        &self,
        identity: &str,
        job: &str,
        attempt: u32,
    ) -> Result<ExecutionRow, VivariumError> {
        let current = self
            .execution(identity)?
            .ok_or_else(|| no_such_execution(identity))?;
        if current.superseded {
            return Err(superseded_execution(identity, "activated"));
        }
        if let Some(phase) = self.current_phase(&current.task)? {
            if phase.phase != current.phase {
                return Err(VivariumError::Other(format!(
                    "execution {}: task {} is now owned by phase {}, not {}",
                    identity, current.task, phase.phase, current.phase
                )));
            }
        }
        self.refuse_stale_activation(&current, attempt)?;
        if current.active_job.as_deref() == Some(job) && current.attempt == Some(attempt) {
            return Ok(current);
        }
        self.conn
            .execute(
                "UPDATE step_executions SET active_job = ?2, attempt = ?3, \
                   original_job = COALESCE(original_job, ?2), \
                   original_attempt = COALESCE(original_attempt, ?3) \
                 WHERE execution = ?1",
                params![identity, job, attempt],
            )
            .map_err(db_err("activate execution job"))?;
        let mut activated = current;
        activated.active_job = Some(job.to_string());
        activated.attempt = Some(attempt);
        if activated.original_job.is_none() {
            activated.original_job = Some(job.to_string());
            activated.original_attempt = Some(attempt);
        }
        Ok(activated)
    }

    /// Refuse an activation an already-active newer generation outranks.
    fn refuse_stale_activation(
        &self,
        current: &ExecutionRow,
        attempt: u32,
    ) -> Result<(), VivariumError> {
        let key = current.key();
        let newer = self.query_execution(
            &format!(
                "SELECT {COLUMNS} FROM step_executions \
                 WHERE need = ?1 AND run = ?2 AND task = ?3 AND phase = ?4 \
                   AND generation > ?5 AND superseded = 0 AND active_job IS NOT NULL \
                 ORDER BY generation DESC LIMIT 1"
            ),
            params![key.need, key.run, key.task, key.phase, current.generation],
        )?;
        let Some(newer) = newer else {
            return Ok(());
        };
        let outranked = match newer.attempt {
            Some(active) => active >= attempt,
            None => true,
        };
        if outranked {
            return Err(VivariumError::Other(format!(
                "execution {}: refusing job at attempt {attempt}: newer execution {} \
                 is already active at attempt {}",
                current.execution,
                newer.execution,
                newer.attempt.map(|a| a.to_string()).unwrap_or_default()
            )));
        }
        Ok(())
    }

    /// Consume one retry of the execution `identity`: supersede it and mint
    /// the next generation of its key, carrying the original-facts linkage,
    /// the input checkpoint, and a history entry for the consumed execution.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the execution is unknown or superseded,
    /// or a row cannot be written.
    pub fn consume_retry(&self, identity: &str) -> Result<ExecutionRow, VivariumError> {
        self.mint_next(identity, None, true)
    }

    /// Start a correction of the execution `identity` at the same attempt:
    /// a new generation and a new execution identity, superseding `identity`.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the execution is unknown or superseded,
    /// or a row cannot be written.
    pub fn start_correction(&self, identity: &str) -> Result<ExecutionRow, VivariumError> {
        let current = self
            .execution(identity)?
            .ok_or_else(|| no_such_execution(identity))?;
        self.mint_next(identity, current.attempt, false)
    }

    fn mint_next(
        &self,
        identity: &str,
        attempt: Option<u32>,
        retry: bool,
    ) -> Result<ExecutionRow, VivariumError> {
        let current = self
            .execution(identity)?
            .ok_or_else(|| no_such_execution(identity))?;
        if current.superseded {
            return Err(superseded_execution(identity, "consumed"));
        }
        let key = current.key();
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin execution generation"))?;
        let generation = next_generation(&tx, &key)?;
        let retries = if retry {
            current.retries + 1
        } else {
            current.retries
        };
        let mut history = current.retry_history.clone();
        history.push(current.execution.clone());
        insert_next_generation(&tx, &current, generation, attempt, retries, &history)?;
        tx.execute(
            "UPDATE step_executions SET superseded = 1 WHERE execution = ?1",
            params![identity],
        )
        .map_err(db_err("supersede execution"))?;
        tx.commit().map_err(db_err("commit execution generation"))?;
        let mut next = current;
        next.generation = generation;
        next.execution = key.identity(generation);
        next.attempt = attempt;
        next.active_job = None;
        next.retries = retries;
        next.retry_history = history;
        next.superseded = false;
        Ok(next)
    }

    /// Supersede the execution `identity`: its result can no longer become
    /// current. Idempotent.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn supersede_execution(&self, identity: &str) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "UPDATE step_executions SET superseded = 1 WHERE execution = ?1",
                params![identity],
            )
            .map_err(db_err("supersede execution"))?;
        Ok(())
    }

    /// Record the input-message checkpoint the execution has consumed.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the execution is unknown or the row
    /// cannot be written.
    pub fn set_input_checkpoint(
        &self,
        identity: &str,
        checkpoint: &str,
    ) -> Result<(), VivariumError> {
        let changed = self
            .conn
            .execute(
                "UPDATE step_executions SET input_checkpoint = ?2 WHERE execution = ?1",
                params![identity, checkpoint],
            )
            .map_err(db_err("set execution checkpoint"))?;
        if changed == 0 {
            return Err(no_such_execution(identity));
        }
        Ok(())
    }

    /// Whether `identity` is still the active execution of its key and its
    /// key still owns the task's current phase.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read.
    pub fn execution_is_current(&self, identity: &str) -> Result<bool, VivariumError> {
        let Some(row) = self.execution(identity)? else {
            return Ok(false);
        };
        if row.superseded {
            return Ok(false);
        }
        if let Some(phase) = self.current_phase(&row.task)? {
            if phase.phase != row.phase {
                return Ok(false);
            }
        }
        Ok(self
            .active_execution(&row.key())?
            .is_some_and(|active| active.execution == row.execution))
    }

    /// The task's current owning phase.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read.
    pub fn current_phase(&self, task: &str) -> Result<Option<TaskPhaseRow>, VivariumError> {
        self.conn
            .query_row(
                "SELECT task, need, run, phase FROM task_phase WHERE task = ?1",
                params![task],
                |row| {
                    Ok(TaskPhaseRow {
                        task: row.get(0)?,
                        need: row.get(1)?,
                        run: row.get(2)?,
                        phase: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(db_err("read task phase"))
    }

    /// Set the task's current owning phase. The path that takes the task over
    /// (for example the start of its merge) advances it.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn set_current_phase(
        &self,
        task: &str,
        need: &str,
        run: u32,
        phase: &str,
    ) -> Result<(), VivariumError> {
        self.conn
            .execute(
                &format!(
                    "INSERT INTO task_phase (task, need, run, phase, changed_at) \
                     VALUES (?1, ?2, ?3, ?4, {}) \
                     ON CONFLICT(task) DO UPDATE SET \
                       need = ?2, run = ?3, phase = ?4, changed_at = {}",
                    now(),
                    now()
                ),
                params![task, need, run, phase],
            )
            .map_err(db_err("set task phase"))?;
        Ok(())
    }
}

fn json_history(history: &[String]) -> Result<String, VivariumError> {
    serde_json::to_string(history)
        .map_err(|e| VivariumError::Other(format!("pump.db step_executions: encode history: {e}")))
}

#[cfg(test)]
#[path = "step_execution_test.rs"]
mod tests;
