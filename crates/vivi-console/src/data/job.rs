//! One job end to end: its `slot_runs` row, its task's text, events and
//! replies. Every read goes through a named query and a read-only store.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row};
use vivi_error::VivariumError;

use super::letter::Letter;
use super::mail_read;
use super::store::db_err;
use super::{RunsState, with_runs};
use crate::queries::{JOB_RUN, TASK_EVENTS};

/// Most events read for one task.
pub const EVENT_LIMIT: i64 = 500;

/// A `slot_runs` row, every column the job page shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRun {
    pub job: String,
    pub task_message_id: String,
    pub need: Option<String>,
    pub pool: String,
    pub slot: String,
    pub machine: String,
    pub model: String,
    pub kind: Option<String>,
    pub target: Option<String>,
    pub branch: String,
    pub start_sha: String,
    pub budget_s: Option<i64>,
    pub read_only: bool,
    pub dispatched_at: i64,
    pub collected_at: Option<i64>,
    pub outcome: Option<String>,
    pub push: Option<String>,
    pub sha: Option<String>,
    pub agent_rc: Option<i64>,
    pub agent_s: Option<i64>,
    pub check_rc: Option<i64>,
    pub check_s: Option<i64>,
    pub job_s: Option<i64>,
    pub agent_cpu_s: Option<i64>,
    pub check_cpu_s: Option<i64>,
    pub landing: Option<String>,
    pub landing_at: Option<i64>,
    pub landing_reason: Option<String>,
}

/// One event on the job's task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskEvent {
    /// Epoch seconds; 0 when the stored stamp did not parse.
    pub at: i64,
    pub event_type: String,
    pub command: String,
    pub actor: Option<String>,
    pub note: Option<String>,
}

/// Everything the job page shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobDetail {
    pub run: JobRun,
    /// The task letter, front matter included; `None` when the store has no
    /// such message (or its file is gone).
    pub brief: Option<Letter>,
    /// The task's events, oldest first. Includes other jobs' events when the
    /// task was dispatched more than once.
    pub events: Vec<TaskEvent>,
    /// Replies on the task, oldest first, front matter dropped.
    pub replies: Vec<Letter>,
}

fn run_from_row(row: &Row<'_>) -> rusqlite::Result<JobRun> {
    Ok(JobRun {
        job: row.get(0)?,
        task_message_id: row.get(1)?,
        need: row.get(2)?,
        pool: row.get(3)?,
        slot: row.get(4)?,
        machine: row.get(5)?,
        model: row.get(6)?,
        kind: row.get(7)?,
        target: row.get(8)?,
        branch: row.get(9)?,
        start_sha: row.get(10)?,
        budget_s: row.get(11)?,
        read_only: row.get::<_, i64>(12)? != 0,
        dispatched_at: row.get(13)?,
        collected_at: row.get(14)?,
        outcome: row.get(15)?,
        push: row.get(16)?,
        sha: row.get(17)?,
        agent_rc: row.get(18)?,
        agent_s: row.get(19)?,
        check_rc: row.get(20)?,
        check_s: row.get(21)?,
        job_s: row.get(22)?,
        agent_cpu_s: row.get(23)?,
        check_cpu_s: row.get(24)?,
        landing: row.get(25)?,
        landing_at: row.get(26)?,
        landing_reason: row.get(27)?,
    })
}

/// Read job `job` of the project at `root`. `Ready(None)` when the index has
/// no such job.
///
/// # Errors
/// Returns an error when the store cannot be read.
pub fn load(root: &Path, job: &str) -> Result<RunsState<Option<JobDetail>>, VivariumError> {
    with_runs(root, |conn| detail(conn, root, job))
}

fn detail(conn: &Connection, root: &Path, job: &str) -> Result<Option<JobDetail>, VivariumError> {
    let found = conn
        .prepare_cached(JOB_RUN.sql)
        .and_then(|mut stmt| stmt.query_row([job], run_from_row).optional())
        .map_err(db_err("read a job"))?;
    let Some(run) = found else {
        return Ok(None);
    };
    let events = task_events(conn, &run.task_message_id)?;
    let (brief, replies) = match mail_read::task_letter(conn, root, &run.task_message_id) {
        Ok(Some((content_id, letter))) => (
            Some(letter),
            mail_read::replies(conn, root, &content_id).unwrap_or_else(|error| {
                tracing::warn!(%error, "console: could not read a job's thread");
                Vec::new()
            }),
        ),
        Ok(None) => (None, Vec::new()),
        Err(error) => {
            tracing::warn!(%error, "console: could not read a job's task letter");
            (None, Vec::new())
        }
    };
    Ok(Some(JobDetail {
        run,
        brief,
        events,
        replies,
    }))
}

fn task_events(conn: &Connection, message_id: &str) -> Result<Vec<TaskEvent>, VivariumError> {
    let context = "read a task's events";
    let mut stmt = conn
        .prepare_cached(TASK_EVENTS.sql)
        .map_err(db_err(context))?;
    let rows = stmt
        .query_map(rusqlite::params![message_id, EVENT_LIMIT], |row| {
            let stamp: String = row.get(0)?;
            Ok(TaskEvent {
                at: chrono::DateTime::parse_from_rfc3339(&stamp).map_or(0, |when| when.timestamp()),
                event_type: row.get(1)?,
                command: row.get(2)?,
                actor: row.get(3)?,
                note: row.get(4)?,
            })
        })
        .map_err(db_err(context))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_err(context))
}

#[cfg(test)]
#[path = "job_test.rs"]
mod tests;
