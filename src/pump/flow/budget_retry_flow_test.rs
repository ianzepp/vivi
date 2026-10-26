//! End to end: budget-retry allowance and raised budgets survive the real
//! pump store.
//!
//! The contract is "a budget kill gets one retry at a raised budget before
//! the step counts as failed". These tests drive the shipped research flow
//! over a real mailspace with jobs held in the durable [`State`] (the
//! `JobStore` the live pump loads), persist through the actual
//! [`PumpStore::save`] / [`PumpStore::load`] path, and drop and reopen the
//! database between the raise and the raised execution.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{PAYLOAD_KEY, spent};
use crate::error::VivariumError;
use crate::letter::fixture::record_with_need;
use crate::pump::config::PumpConfig;
use crate::pump::envelope::correct::base_id;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::e2e_rig::agent_receipt;
use crate::pump::flow::mail::{MailWriter, MailspacePort};
use crate::pump::flow::tick::tick;
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobOutcome, JobStatus, PoolJob};
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

/// The pump's mailspace identity: the default `[pump] reply_from`.
const PUMP: &str = "mind";
/// Ticks one wait may take before it gives up.
const MAX_TICKS: usize = 12;
/// The fixture pool the routed researcher runs on.
const POOL: &str = "test-planner-pool";
/// The dispatch budget the research job starts with (the default).
const FIRST_BUDGET: u64 = 600;
/// `FIRST_BUDGET` doubled: the raised retry budget.
const RAISED_BUDGET: u64 = 1_200;
/// The current configuration the raise must ignore: a changed shared budget.
const DISTRACTOR_BUDGET: u64 = 444;

/// A research-flow run over a real mailspace, jobs held in `State`.
struct FlowRig {
    /// Keeps the project directory alive for the whole test.
    project: Project,
    ctx: Ctx,
    need: String,
}

impl FlowRig {
    /// The pump database's path.
    fn db(&self) -> PathBuf {
        self.ctx.home.join(DB_FILENAME)
    }
}

/// A research rig: a need with an open pump record and a run started at the
/// research entry, a routed researcher role, and the run's first job not yet
/// started. Ticks run through the real [`tick`], which opens and closes the
/// store itself.
///
/// # Errors
/// Returns a [`VivariumError`] when the record or the run cannot be opened.
fn rig(cfg: PumpConfig) -> Result<FlowRig, VivariumError> {
    let project = project();
    let need = project.need("a need for the budget-retry flow");
    let mut ctx = project.pump_ctx();
    ctx.cfg = Arc::new(cfg);
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("researcher"),
        kind: Some(String::from("researcher")),
        status: String::from("active"),
        pool: Some(POOL.into()),
        read_only: None,
        model: None,
        models: Vec::new(),
    });
    ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let mut port = MailspacePort {
        mailspace: project.mailspace.clone(),
        pump_identity: PUMP.into(),
    };
    MailWriter::open_record(
        &mut port,
        &need,
        &record_with_need("+++\nkind = \"research\"\n+++\n", &need),
    )?;
    start(&ctx, &store, &mut port, &need, Some("research"))?;
    Ok(FlowRig { project, ctx, need })
}

/// Tick until `pred` holds on the job store, at most [`MAX_TICKS`] ticks.
///
/// # Errors
/// Returns a tick error, or an error naming `label` when `pred` never held.
fn tick_until(ctx: &Ctx, state: &mut State, label: &str, pred: impl Fn(&State) -> bool) -> R {
    for _ in 0..MAX_TICKS {
        if pred(state) {
            return Ok(());
        }
        tick(ctx, state)?;
    }
    if pred(state) {
        return Ok(());
    }
    Err(VivariumError::Message(format!(
        "never reached `{label}` in {MAX_TICKS} ticks"
    )))
}

/// The newest pending or dispatched job whose payload `node` is `node`.
fn open_job(state: &State, node: &str) -> Option<PoolJob> {
    state
        .jobs
        .values()
        .filter(|job| matches!(job.status, JobStatus::Pending | JobStatus::Dispatched))
        .filter(|job| job.payload.get("node").and_then(|v| v.as_str()) == Some(node))
        .max_by_key(|job| job.queued_at)
        .cloned()
}

/// The job with `id`, or an error naming what is there.
fn job(state: &State, id: &str) -> Result<PoolJob, VivariumError> {
    state.jobs.get(id).cloned().ok_or_else(|| {
        let known: Vec<&String> = state.jobs.keys().collect();
        VivariumError::Message(format!("no job {id} among {known:?}"))
    })
}

/// End job `id` as the pool would: `status` and an outcome carrying
/// `receipt`.
fn finish(state: &mut State, id: &str, status: JobStatus, receipt: &str) -> R {
    let job = state
        .jobs
        .get_mut(id)
        .ok_or_else(|| VivariumError::Message(format!("no job {id} to finish")))?;
    job.status = status;
    job.outcome = Some(JobOutcome {
        state: match status {
            JobStatus::Timeout => "timeout".into(),
            JobStatus::Lost => "lost".into(),
            _ => "finished".into(),
        },
        detail: String::new(),
        receipt: receipt.into(),
        at: 0,
    });
    Ok(())
}

/// Save the whole state through the real store, drop the database and load
/// it back: the round trip a pump restart gives every durable record.
fn save_reload(path: &Path, state: State) -> Result<State, VivariumError> {
    PumpStore::open(path)?.save(&state)?;
    drop(PumpStore::open(path)?);
    PumpStore::open(path)?.load()
}

/// The stamped run/task identity of a job payload: need, run, node, task.
fn identity(job: &PoolJob) -> (String, u64, String, String) {
    let text = |key: &str| {
        job.payload
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    let number = |key: &str| job.payload.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    (text("need"), number("run"), text("node"), text("task"))
}

/// The states of the need's runs, oldest first.
fn run_states(path: &Path, need: &str) -> Result<Vec<String>, VivariumError> {
    let store = PumpStore::open_read_only(path)?;
    Ok(store
        .ok_or_else(|| VivariumError::Message("no pump store".into()))?
        .runs_of(need)?
        .into_iter()
        .map(|run| run.state)
        .collect())
}

/// Wait until the need's run is `done`.
fn tick_until_done(rig: &FlowRig, state: &mut State) -> R {
    let db = rig.db();
    let need = rig.need.clone();
    tick_until(
        &rig.ctx,
        state,
        "run done",
        |_| matches!(run_states(&db, &need), Ok(states) if states.iter().any(|s| s == "done")),
    )
}

#[test]
fn raised_retry_survives_store_round_trip() -> R {
    let mut rig = rig(PumpConfig::default())?;
    let db = rig.db();
    let mut state = PumpStore::open(&db)?.load()?;

    // The research step opens its first attempt at the configured budget.
    tick_until(&rig.ctx, &mut state, "research job", |state| {
        open_job(state, "research").is_some()
    })?;
    let first_id = open_job(&state, "research")
        .ok_or_else(|| VivariumError::Message("no research job".into()))?
        .id;
    let first = job(&state, &first_id)?;
    assert_eq!(first.budget, FIRST_BUDGET);
    // An older record: the payload carries no marker key at all, so the
    // retry is unspent.
    assert!(first.payload.get(PAYLOAD_KEY).is_none());
    assert!(!spent(&first));
    let raised_id = format!("{}-a2", base_id(&first_id));

    // First budget kill: machine evidence, the pool's timeout status.
    finish(&mut state, &first_id, JobStatus::Timeout, "timeout")?;

    // Before collection the configuration changes; the raise must still be
    // calculated from the failed job's stored budget, not the current one.
    let mut cfg = (*rig.ctx.cfg).clone();
    cfg.dispatch.budget = DISTRACTOR_BUDGET;
    rig.ctx.cfg = Arc::new(cfg);
    tick(&rig.ctx, &mut state)?;
    let raised = job(&state, &raised_id)?;
    assert_eq!(raised.status, JobStatus::Pending);
    assert_eq!(raised.budget, RAISED_BUDGET);
    assert!(spent(&raised));
    assert_eq!(identity(&raised), identity(&first));
    assert_eq!(
        raised.payload.get("attempt").and_then(|v| v.as_u64()),
        Some(2)
    );

    // The round trip: persist the jobs plus the indexed step attempts, drop
    // and reopen the database, and load the state before the raised
    // execution ends. The pending raised budget and the marker survive.
    let mut state = save_reload(&db, state)?;
    let raised = job(&state, &raised_id)?;
    assert_eq!(raised.status, JobStatus::Pending);
    assert_eq!(raised.budget, RAISED_BUDGET);
    assert!(spent(&raised));
    // The killed attempt's applied marker survived too, and both indexed
    // step attempts still point at the same step of the same run.
    assert!(job(&state, &first_id)?.applied_at.is_some());
    {
        let store = PumpStore::open(&db)?;
        let row1 = store
            .step_job(&first_id)?
            .ok_or_else(|| VivariumError::Message("no indexed attempt 1".into()))?;
        let row2 = store
            .step_job(&raised_id)?
            .ok_or_else(|| VivariumError::Message("no indexed attempt 2".into()))?;
        assert_eq!((row1.attempt, row2.attempt), (1, 2));
        assert_eq!((row1.task, row1.node), (row2.task.clone(), row2.node));
        assert_eq!(store.last_attempt(&row2.task)?, 2);
    }

    // A repeated tick enqueues nothing extra.
    tick(&rig.ctx, &mut state)?;
    assert_eq!(state.jobs.len(), 2);
    assert_eq!(job(&state, &raised_id)?.budget, RAISED_BUDGET);

    // Replay of the first collection: a killed attempt seen unapplied again
    // finds the raised job already present under the deterministic id and
    // queues no duplicate.
    if let Some(killed) = state.jobs.get_mut(&first_id) {
        killed.applied_at = None;
    }
    tick(&rig.ctx, &mut state)?;
    assert_eq!(state.jobs.len(), 2);
    assert_eq!(job(&state, &raised_id)?.budget, RAISED_BUDGET);
    assert!(job(&state, &first_id)?.applied_at.is_some());

    // Kill the raised job after the reload: the one retry is spent, so the
    // step fails — no third allowance and no correction job.
    finish(&mut state, &raised_id, JobStatus::Timeout, "timeout")?;
    tick(&rig.ctx, &mut state)?;
    assert_eq!(state.jobs.len(), 2);
    assert!(spent(&job(&state, &raised_id)?));
    assert!(state.jobs.values().all(|job| job.kind != "envelope-fix"));
    tick_until_done(&rig, &mut state)?;
    Ok(())
}

#[test]
fn lost_then_budget_kill_still_gets_one_raised_retry() -> R {
    let mut rig = rig(PumpConfig::default())?;
    let db = rig.db();
    let mut state = PumpStore::open(&db)?.load()?;

    tick_until(&rig.ctx, &mut state, "research job", |state| {
        open_job(state, "research").is_some()
    })?;
    let first_id = open_job(&state, "research")
        .ok_or_else(|| VivariumError::Message("no research job".into()))?
        .id;
    assert_eq!(job(&state, &first_id)?.budget, FIRST_BUDGET);
    let base = base_id(&first_id).to_owned();
    let retry_id = format!("{base}-a2");
    let raised_id = format!("{retry_id}-a3");

    // First lose the job the ordinary way: Lost is no budget kill, so the
    // attempt reruns at the unchanged budget with no retry marker.
    finish(
        &mut state,
        &first_id,
        JobStatus::Lost,
        "worker lost the job",
    )?;
    tick(&rig.ctx, &mut state)?;
    let retry = job(&state, &retry_id)?;
    assert_eq!(retry.status, JobStatus::Pending);
    assert_eq!(retry.budget, FIRST_BUDGET);
    assert!(retry.payload.get(PAYLOAD_KEY).is_none());
    assert!(!spent(&retry));

    // Persist and reload in the chain, before the kill of the retry.
    let mut state = save_reload(&db, state)?;
    assert_eq!(job(&state, &retry_id)?.budget, FIRST_BUDGET);

    // The retry's first genuine budget kill still raises once: attempt 3 at
    // the doubled budget, killed through the finished exit-124 summary.
    finish(
        &mut state,
        &retry_id,
        JobStatus::Finished,
        &agent_receipt(124, "cut off mid-analysis"),
    )?;
    tick(&rig.ctx, &mut state)?;
    let raised = job(&state, &raised_id)?;
    assert_eq!(raised.status, JobStatus::Pending);
    assert_eq!(raised.budget, RAISED_BUDGET);
    assert!(spent(&raised));

    // Reload: the spent marker survives the store.
    let mut state = save_reload(&db, state)?;
    assert!(spent(&job(&state, &raised_id)?));

    // The next budget kill cannot escalate again: the step fails, no fourth
    // attempt and no correction appears, and the run still settles.
    finish(&mut state, &raised_id, JobStatus::Timeout, "timeout")?;
    tick(&rig.ctx, &mut state)?;
    assert_eq!(state.jobs.len(), 3);
    assert!(spent(&job(&state, &raised_id)?));
    tick_until_done(&rig, &mut state)?;
    Ok(())
}
