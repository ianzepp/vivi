//! A need's place in the pump's flow, read from `pump.db`.
//!
//! The pump keys everything by the need's handle, the same handle the
//! backlog node and `slot_runs.need` carry. Every statement here is a named
//! query in `queries.rs`; this module binds and decodes.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, ToSql};
use vivi_error::VivariumError;

use super::json_list::json_strings;
use super::pump_db::{PumpRead, pump_db_path};
use super::store::{db_err, has_table};
use crate::queries::{
    FANOUT_OPEN, FANOUT_START, FLOW_RUN_LATEST, FLOW_RUNS, FLOW_STEP_CURRENT, FLOW_STEPS,
    NEED_BRANCH, PLAN_REVISIONS, Query, RESTART_ENTRIES, RUN_DISPATCHED, RUNG_RUNS_OF_TASK,
    SLICE_ROWS, STEP_JOBS_OF_TASK,
};

/// Slice tasks read for one run.
pub const SLICE_LIMIT: i64 = 300;
/// Steps read for one run.
pub const STEP_LIMIT: i64 = 500;
const RUN_LIMIT: i64 = 50;
const REVISION_LIMIT: i64 = 50;
const JOBS_PER_TASK: i64 = 20;
const SLICE_PREFIX: &str = "slice:";
const SENTINEL: &str = "sentinel";
const HELD_PATH_PREFIX: &str = "path:";

/// Whether a pump store can be read, and if not, why not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PumpState<T> {
    /// The project configures no pump.
    NoPump,
    /// A pump is configured but has written no store yet.
    NotStarted,
    /// The store (or the config) could not be read.
    Unreadable(String),
    Ready(T),
}

impl<T> PumpState<T> {
    /// The same state with its value dropped.
    #[must_use]
    pub fn status(&self) -> PumpState<()> {
        match self {
            Self::NoPump => PumpState::NoPump,
            Self::NotStarted => PumpState::NotStarted,
            Self::Unreadable(why) => PumpState::Unreadable(why.clone()),
            Self::Ready(_) => PumpState::Ready(()),
        }
    }
}

/// Open the project's pump store for reading.
#[must_use]
pub fn open_pump(root: &Path) -> PumpState<PumpRead> {
    let path = match pump_db_path(root) {
        Ok(Some(path)) => path,
        Ok(None) => return PumpState::NoPump,
        Err(error) => return PumpState::Unreadable(error.to_string()),
    };
    match PumpRead::open_path(&path) {
        Ok(Some(read)) => PumpState::Ready(read),
        Ok(None) => PumpState::NotStarted,
        Err(error) => PumpState::Unreadable(error.to_string()),
    }
}

/// One flow run of a need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunInfo {
    pub run: i64,
    pub flow: String,
    pub version: i64,
    pub kind: String,
    pub entry: String,
    pub state: String,
    pub state_at: i64,
}

/// One step row of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepInfo {
    pub node: String,
    pub task: String,
    pub state: String,
    pub verdict: Option<String>,
}

/// Where a need stands in its newest run: the list pages show this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowPos {
    pub run: RunInfo,
    pub step: Option<StepInfo>,
    /// When the current step's newest job was dispatched, if it had one.
    pub step_since: Option<i64>,
}

fn run_from_row(row: &Row<'_>) -> rusqlite::Result<RunInfo> {
    Ok(RunInfo {
        run: row.get(0)?,
        flow: row.get(1)?,
        version: row.get(2)?,
        kind: row.get(3)?,
        entry: row.get(4)?,
        state: row.get(5)?,
        state_at: row.get(6)?,
    })
}

fn step_from_row(row: &Row<'_>) -> rusqlite::Result<StepInfo> {
    Ok(StepInfo {
        node: row.get(0)?,
        task: row.get(1)?,
        state: row.get(2)?,
        verdict: row.get(3)?,
    })
}

fn rows<T>(
    conn: &Connection,
    query: &Query,
    params: &[&dyn ToSql],
    map: fn(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, VivariumError> {
    let context = "read the pump store";
    let mut stmt = conn.prepare_cached(query.sql).map_err(db_err(context))?;
    let found = stmt.query_map(params, map).map_err(db_err(context))?;
    found
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err(context))
}

fn one<T>(
    conn: &Connection,
    query: &Query,
    params: &[&dyn ToSql],
    map: fn(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Option<T>, VivariumError> {
    let context = "read the pump store";
    let mut stmt = conn.prepare_cached(query.sql).map_err(db_err(context))?;
    stmt.query_row(params, map)
        .optional()
        .map_err(db_err(context))
}

/// Whether the pump store has the flow tables (a store from before the flow
/// index has none).
///
/// # Errors
/// Returns an error when the schema cannot be read.
pub fn has_flow_tables(conn: &Connection) -> Result<bool, VivariumError> {
    has_table(conn, "flow_runs")
}

/// The newest run of `need` and its current step; `None` when the need has
/// never run through the flow. `mail` is the mailspace connection, used to
/// date the step's job when the slot index exists.
///
/// # Errors
/// Returns an error when a query fails.
pub fn position(
    pump: &Connection,
    mail: Option<&Connection>,
    need: &str,
) -> Result<Option<FlowPos>, VivariumError> {
    let Some(run) = one(pump, &FLOW_RUN_LATEST, &[&need], run_from_row)? else {
        return Ok(None);
    };
    let step = one(pump, &FLOW_STEP_CURRENT, &[&need, &run.run], step_from_row)?;
    let step_since = match (&step, mail) {
        (Some(step), Some(mail)) => step_started(pump, mail, &step.task)?,
        _ => None,
    };
    Ok(Some(FlowPos {
        run,
        step,
        step_since,
    }))
}

/// When the newest job of a step task was dispatched.
fn step_started(
    pump: &Connection,
    mail: &Connection,
    task: &str,
) -> Result<Option<i64>, VivariumError> {
    let jobs = rows(pump, &STEP_JOBS_OF_TASK, &[&task, &5_i64], |row| {
        row.get::<_, String>(0)
    })?;
    let mut newest = None;
    for job in jobs {
        let at = mail
            .prepare_cached(RUN_DISPATCHED.sql)
            .and_then(|mut stmt| {
                stmt.query_row([&job], |row| row.get::<_, i64>(0))
                    .optional()
            })
            .map_err(db_err("read the slot index"))?;
        newest = newest.max(at);
    }
    Ok(newest)
}

/// How a rung ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RungInfo {
    pub rung: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub rc: Option<i64>,
}

/// One slice of a build step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceInfo {
    pub task: String,
    /// The slice id: the node without its `slice:` prefix.
    pub id: String,
    pub state: String,
    pub verdict: Option<String>,
    pub scope: Vec<String>,
    pub scope_requests: i64,
    pub scope_state: String,
    /// Slice tasks this one waits on for paths it asked for.
    pub holders: Vec<String>,
    /// Paths it asked for that another slice holds.
    pub held_paths: Vec<String>,
    pub deps: Vec<String>,
    pub fanout: Option<String>,
    pub tries: i64,
    pub last_job: Option<String>,
    pub rung: Option<RungInfo>,
}

/// The slices of one fanout (build) step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanoutGroup {
    /// The fanout step's node, e.g. `build1`; `?` for slices with no link.
    pub node: String,
    /// The commit the fanout started from.
    pub tip: Option<String>,
    pub open: bool,
    pub slices: Vec<SliceInfo>,
}

/// A step of the path, with the rung that ran under it if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathStep {
    pub step: StepInfo,
    pub rung: Option<RungInfo>,
}

/// One run in detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunDetail {
    pub run: RunInfo,
    /// The steps in order, without the sentinel and the slices.
    pub path: Vec<PathStep>,
    /// Index into `path` of the current step.
    pub current: Option<usize>,
    pub groups: Vec<FanoutGroup>,
}

/// One plan revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRev {
    pub revision: i64,
    pub verdict: String,
    pub reason: String,
    pub created_at: i64,
}

/// The need's branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchInfo {
    pub branch: String,
    pub base: Option<String>,
    pub status: String,
}

/// Everything the flow tab shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowView {
    pub runs: Vec<RunInfo>,
    /// The entry node each restart started a run at: (run, entry).
    pub restarts: Vec<(i64, String)>,
    pub selected: Option<RunDetail>,
    pub plans: Vec<PlanRev>,
    pub branch: Option<BranchInfo>,
}

/// Load the flow tab for `need`; `run` picks a run, else the newest.
///
/// # Errors
/// Returns an error when a query fails.
pub fn load_flow(
    pump: &Connection,
    need: &str,
    run: Option<i64>,
) -> Result<FlowView, VivariumError> {
    let runs = rows(pump, &FLOW_RUNS, &[&need, &RUN_LIMIT], run_from_row)?;
    let chosen = match run {
        Some(wanted) => runs.iter().find(|candidate| candidate.run == wanted),
        None => runs.last(),
    };
    let selected = chosen.map(|run| run_detail(pump, need, run)).transpose()?;
    let restarts = rows(pump, &RESTART_ENTRIES, &[&need, &RUN_LIMIT], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?;
    let plans = rows(pump, &PLAN_REVISIONS, &[&need, &REVISION_LIMIT], |row| {
        Ok(PlanRev {
            revision: row.get(0)?,
            verdict: row.get(1)?,
            reason: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;
    let branch = one(pump, &NEED_BRANCH, &[&need], |row| {
        Ok(BranchInfo {
            branch: row.get(0)?,
            base: row.get(1)?,
            status: row.get(2)?,
        })
    })?;
    Ok(FlowView {
        runs,
        restarts,
        selected,
        plans,
        branch,
    })
}

fn run_detail(pump: &Connection, need: &str, run: &RunInfo) -> Result<RunDetail, VivariumError> {
    let steps = rows(
        pump,
        &FLOW_STEPS,
        &[&need, &run.run, &STEP_LIMIT],
        step_from_row,
    )?;
    let mut path = Vec::new();
    for step in steps {
        if step.node == SENTINEL || step.node.starts_with(SLICE_PREFIX) {
            continue;
        }
        let rung = newest_rung(pump, &step.task)?;
        path.push(PathStep { step, rung });
    }
    let current = path.iter().rposition(|entry| is_live(&entry.step.state));
    let current = current.or_else(|| path.len().checked_sub(1));
    let groups = fanout_groups(pump, need, run.run, &path)?;
    Ok(RunDetail {
        run: run.clone(),
        path,
        current,
        groups,
    })
}

fn is_live(state: &str) -> bool {
    matches!(state, "pending" | "running" | "waiting")
}

fn newest_rung(pump: &Connection, task: &str) -> Result<Option<RungInfo>, VivariumError> {
    one(pump, &RUNG_RUNS_OF_TASK, &[&task, &1_i64], rung_from_row)
}

fn rung_from_row(row: &Row<'_>) -> rusqlite::Result<RungInfo> {
    Ok(RungInfo {
        rung: row.get(0)?,
        started_at: row.get(1)?,
        finished_at: row.get(2)?,
        rc: row.get(3)?,
    })
}

fn slice_from_row(row: &Row<'_>) -> rusqlite::Result<SliceInfo> {
    let node: String = row.get(1)?;
    let list = |index: usize| -> rusqlite::Result<Vec<String>> {
        Ok(row
            .get::<_, Option<String>>(index)?
            .map(|text| json_strings(&text))
            .unwrap_or_default())
    };
    let holders = list(7)?;
    let (held, waits): (Vec<String>, Vec<String>) = holders
        .into_iter()
        .partition(|entry| entry.starts_with(HELD_PATH_PREFIX));
    Ok(SliceInfo {
        task: row.get(0)?,
        id: node.strip_prefix(SLICE_PREFIX).unwrap_or(&node).to_string(),
        state: row.get(2)?,
        verdict: row.get(3)?,
        scope: list(4)?,
        scope_requests: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
        scope_state: row
            .get::<_, Option<String>>(6)?
            .unwrap_or_else(|| String::from("none")),
        holders: waits,
        held_paths: held
            .into_iter()
            .map(|path| path[HELD_PATH_PREFIX.len()..].to_string())
            .collect(),
        deps: list(9)?,
        fanout: row.get(8)?,
        tries: 0,
        last_job: None,
        rung: None,
    })
}

fn fanout_groups(
    pump: &Connection,
    need: &str,
    run: i64,
    path: &[PathStep],
) -> Result<Vec<FanoutGroup>, VivariumError> {
    let mut slices = rows(
        pump,
        &SLICE_ROWS,
        &[&need, &run, &SLICE_LIMIT],
        slice_from_row,
    )?;
    for slice in &mut slices {
        let jobs = rows(
            pump,
            &STEP_JOBS_OF_TASK,
            &[&slice.task, &JOBS_PER_TASK],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )?;
        slice.tries = jobs.iter().map(|(_, attempt)| *attempt).max().unwrap_or(0);
        slice.last_job = jobs.first().map(|(job, _)| job.clone());
        slice.rung = newest_rung(pump, &slice.task)?;
    }
    let open = one(pump, &FANOUT_OPEN, &[&need, &run], |row| {
        row.get::<_, String>(0)
    })?;
    group_by_fanout(pump, path, slices, open.as_deref())
}

fn group_by_fanout(
    pump: &Connection,
    path: &[PathStep],
    slices: Vec<SliceInfo>,
    open: Option<&str>,
) -> Result<Vec<FanoutGroup>, VivariumError> {
    let mut groups: Vec<(Option<String>, Vec<SliceInfo>)> = Vec::new();
    for slice in slices {
        match groups.iter_mut().find(|(task, _)| *task == slice.fanout) {
            Some((_, members)) => members.push(slice),
            None => groups.push((slice.fanout.clone(), vec![slice])),
        }
    }
    let mut out = Vec::new();
    for (task, slices) in groups {
        let node = task
            .as_deref()
            .and_then(|task| path.iter().find(|entry| entry.step.task == task))
            .map_or_else(|| String::from("?"), |entry| entry.step.node.clone());
        let tip = match task.as_deref() {
            Some(task) => one(pump, &FANOUT_START, &[&task], |row| {
                row.get::<_, Option<String>>(0)
            })?
            .flatten(),
            None => None,
        };
        out.push(FanoutGroup {
            node,
            tip,
            open: task.as_deref().is_some() && task.as_deref() == open,
            slices,
        });
    }
    Ok(out)
}

#[cfg(test)]
#[path = "need_flow_test.rs"]
mod tests;
