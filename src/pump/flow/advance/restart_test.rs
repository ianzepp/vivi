//! Tests for the idempotent restart and its tick phase, over the fake mail
//! port with one injected failure per write.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::{TempDir, tempdir};

use super::{ABANDONED_MARKER, RESTART_STUCK, RUN_RESTARTING, prune_dead_jobs, register, restart};
use crate::error::VivariumError;
use crate::mailspace::marked_payload;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::{SENTINEL_NODE, start};
use crate::pump::flow::def::{STEP_MARKER, VERDICT_SUPERSEDED};
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::fanout::SLICE_NODE_PREFIX;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, TaskStatus};
use crate::pump::flow::slice_dispatch::SliceMail;
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobSpec, JobStatus, JobStore, PoolJob, enqueue};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";

#[derive(Default)]
struct Jobs {
    jobs: BTreeMap<String, PoolJob>,
}

impl JobStore for Jobs {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.jobs
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.jobs
    }
}

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
    registry: Registry<Jobs, FakeMail>,
}

/// A need with a live run 1: sentinel, entry step, one extra open step and one
/// open slice.
fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("pump");
    MailWriter::open_record(&mut mail, NEED, "record")?;
    start(&ctx, &store, &mut mail, NEED, None)?;
    for node in ["build1", &format!("{SLICE_NODE_PREFIX}a")] {
        let task = mail.send_step(
            "pump",
            "pump",
            node,
            &format!("{STEP_MARKER} {NEED} r1 {node}"),
            &[],
        )?;
        mail.bind(NEED, std::slice::from_ref(&task))?;
        store.insert_step(
            NEED,
            &StepRow {
                run: 1,
                node: node.into(),
                task,
                state: StepState::Running,
                verdict: None,
            },
        )?;
    }
    let mut registry = Registry::new();
    register(&mut registry);
    Ok(Rig {
        _dir: dir,
        ctx,
        host,
        store,
        mail,
        jobs: Jobs::default(),
        registry,
    })
}

impl Rig {
    fn old(&self) -> Result<RunRow, VivariumError> {
        self.store
            .runs_of(NEED)?
            .into_iter()
            .find(|r| r.run == 1)
            .ok_or_else(|| VivariumError::Other("no run 1".into()))
    }

    fn tick(&mut self) -> Result<(), VivariumError> {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.mail,
            &self.registry,
        )
    }

    fn restart(&mut self) -> Result<RunRow, VivariumError> {
        let old = self.old()?;
        restart(&self.ctx, &self.store, &mut self.mail, &old, "plan")
    }

    fn exceptions(&self) -> String {
        fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }

    fn state_of(&self, run: u32) -> Result<String, VivariumError> {
        self.store
            .runs_of(NEED)?
            .into_iter()
            .find(|r| r.run == run)
            .map(|r| r.state)
            .ok_or_else(|| VivariumError::Other(format!("no run {run}")))
    }

    fn add_job(&mut self, id: &str, task: &str, run: u32) -> R {
        let mut spec = JobSpec::new(id, "planner", "pump-planner", "s", "b");
        spec.task = Some(task.to_string());
        enqueue(&mut self.jobs, spec)?;
        self.store.insert_step_job(
            NEED,
            &StepJobRow {
                job: id.into(),
                run,
                node: "x".into(),
                task: task.into(),
                attempt: 1,
            },
        )
    }
}

fn epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[test]
fn a_restart_closes_the_old_tasks_superseded_with_the_sentinel_last() -> R {
    let mut rig = rig()?;
    rig.mail.calls.clear();
    let next = rig.restart()?;

    assert_eq!((next.run, next.state.as_str()), (2, "live"));
    assert_eq!(rig.state_of(1)?, "superseded");
    let rows = rig.store.steps_of(NEED, 1)?;
    assert_eq!(rows.len(), 4);
    for row in &rows {
        assert_eq!(row.state, StepState::Done, "{}", row.node);
        assert_eq!(row.verdict.as_deref(), Some(VERDICT_SUPERSEDED));
        let task = rig.mail.task_state(&row.task)?;
        assert_eq!(task.status, TaskStatus::Done, "{}", row.node);
        assert_eq!(task.verdict.as_deref(), Some(VERDICT_SUPERSEDED));
    }
    let sentinel = rows.iter().find(|r| r.node == SENTINEL_NODE);
    let sentinel = sentinel.map(|r| r.task.clone()).unwrap_or_default();
    let closed_at = |task: &str| {
        rig.mail
            .position(|c| matches!(c, Call::CloseTask { handle, .. } if handle == task))
    };
    for row in rows.iter().filter(|r| r.task != sentinel) {
        assert!(closed_at(&row.task) < closed_at(&sentinel), "{}", row.node);
    }
    Ok(())
}

#[test]
fn a_failure_at_any_write_settles_to_one_new_run_through_the_phase() -> R {
    let mut tried = 0;
    for write in 0..40 {
        let mut rig = rig()?;
        rig.mail.fail_at = Some(rig.mail.calls.len() + write);
        if rig.restart().is_ok() {
            assert!(rig.mail.fail_at.is_some(), "write {write} was due but ok");
            break;
        }
        tried += 1;
        assert!(
            matches!(rig.state_of(1)?.as_str(), RUN_RESTARTING),
            "write {write}"
        );
        for _ in 0..3 {
            rig.tick()?;
        }
        assert_eq!(rig.state_of(1)?, "superseded", "write {write}");
        let runs = rig.store.runs_of(NEED)?;
        assert_eq!(runs.len(), 2, "write {write}");
        assert_eq!(runs[1].state, "live", "write {write}");
        for run in 1..=2 {
            let sentinels = rig
                .store
                .steps_of(NEED, run)?
                .into_iter()
                .filter(|r| r.node == SENTINEL_NODE)
                .count();
            assert_eq!(sentinels, 1, "write {write} run {run}");
        }
        assert_eq!(rig.mail.sentinel_count(NEED)?, 2, "write {write}");
        for row in rig.store.steps_of(NEED, 1)? {
            let task = rig.mail.task_state(&row.task)?;
            assert_eq!(task.status, TaskStatus::Done, "write {write} {}", row.node);
            assert_eq!(task.verdict.as_deref(), Some(VERDICT_SUPERSEDED));
            assert_eq!(row.state, StepState::Done, "write {write} {}", row.node);
        }
    }
    assert!(tried >= 6, "expected several failing writes, got {tried}");
    Ok(())
}

#[test]
fn a_restart_that_cannot_finish_raises_the_stuck_alarm_once_after_600_seconds() -> R {
    let mut rig = rig()?;
    // No newer run exists, so the entry is unknown and the resume fails.
    rig.store.set_run_state(NEED, 1, RUN_RESTARTING)?;

    rig.host.set_now(epoch());
    rig.tick()?;
    assert_eq!(rig.exceptions().matches(RESTART_STUCK).count(), 0);

    rig.host.set_now(epoch() + 700);
    rig.tick()?;
    rig.tick()?;
    assert_eq!(rig.exceptions().matches(RESTART_STUCK).count(), 1);
    assert!(rig.exceptions().contains("n1 r1"));
    assert_eq!(rig.state_of(1)?, RUN_RESTARTING);
    Ok(())
}

#[test]
fn the_phase_removes_pending_jobs_of_dead_runs_only() -> R {
    let mut rig = rig()?;
    let old_task = rig.store.steps_of(NEED, 1)?[0].task.clone();
    rig.add_job("j-old-pending", &old_task, 1)?;
    rig.add_job("j-old-running", &old_task, 1)?;
    if let Some(job) = rig.jobs.jobs.get_mut("j-old-running") {
        job.status = JobStatus::Dispatched;
    }
    // A withdrawn run of another need.
    rig.store.insert_run(&RunRow {
        need: "n2".into(),
        run: 1,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "withdrawn".into(),
    })?;
    rig.store.insert_step(
        "n2",
        &StepRow {
            run: 1,
            node: "plan".into(),
            task: "other-task".into(),
            state: StepState::Done,
            verdict: None,
        },
    )?;
    let mut spec = JobSpec::new("j-withdrawn", "planner", "pump-planner", "s", "b");
    spec.task = Some("other-task".into());
    enqueue(&mut rig.jobs, spec)?;
    rig.store.insert_step_job(
        "n2",
        &StepJobRow {
            job: "j-withdrawn".into(),
            run: 1,
            node: "plan".into(),
            task: "other-task".into(),
            attempt: 1,
        },
    )?;

    let next = rig.restart()?;
    let new_task = rig.store.steps_of(NEED, next.run)?[0].task.clone();
    rig.add_job("j-new-pending", &new_task, next.run)?;
    rig.tick()?;

    let left: Vec<&str> = rig.jobs.jobs.keys().map(String::as_str).collect();
    assert_eq!(left, vec!["j-new-pending", "j-old-running"]);
    Ok(())
}

#[test]
fn dead_runs_list_their_dispatched_jobs_once_and_late_results_close_out_quietly() -> R {
    let mut rig = rig()?;
    let dead_task = rig.store.steps_of(NEED, 1)?[0].task.clone();
    let sentinel = rig
        .store
        .steps_of(NEED, 1)?
        .into_iter()
        .find(|r| r.node == SENTINEL_NODE)
        .map(|r| r.task)
        .unwrap_or_default();
    rig.add_job("j-pending", &dead_task, 1)?;
    rig.add_job("j-flying", &dead_task, 1)?;
    if let Some(job) = rig.jobs.jobs.get_mut("j-flying") {
        job.status = JobStatus::Dispatched;
    }
    // Withdraw the run; a live run of another need keeps its own finished job.
    rig.store.set_run_state(NEED, 1, "withdrawn")?;
    MailWriter::open_record(&mut rig.mail, "n2", "record")?;
    rig.store.insert_run(&RunRow {
        need: "n2".into(),
        run: 1,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    })?;
    let live_task = rig.mail.send_step("pump", "pump", "plan", "live", &[])?;
    rig.store.insert_step(
        "n2",
        &StepRow {
            run: 1,
            node: "plan".into(),
            task: live_task.clone(),
            state: StepState::Running,
            verdict: None,
        },
    )?;
    let mut spec = JobSpec::new("j-live", "planner", "pump-planner", "s", "b");
    spec.task = Some(live_task.clone());
    enqueue(&mut rig.jobs, spec)?;
    rig.store.insert_step_job(
        "n2",
        &StepJobRow {
            job: "j-live".into(),
            run: 1,
            node: "plan".into(),
            task: live_task.clone(),
            attempt: 1,
        },
    )?;
    if let Some(job) = rig.jobs.jobs.get_mut("j-live") {
        job.status = JobStatus::Dispatched;
    }
    let abandoned = |rig: &Rig| -> Result<Vec<(String, String)>, VivariumError> {
        rig.mail.marked_replies(&sentinel, ABANDONED_MARKER)
    };

    rig.tick()?;
    assert!(!rig.jobs.jobs.contains_key("j-pending"));
    let replies = abandoned(&rig)?;
    assert_eq!(replies.len(), 1);
    assert_eq!(
        marked_payload(&replies[0].1, ABANDONED_MARKER),
        Some("j-flying")
    );

    let calls = rig.mail.calls.len();
    rig.tick()?;
    assert_eq!(abandoned(&rig)?.len(), 1);
    assert_eq!(rig.mail.calls.len(), calls);

    if let Some(job) = rig.jobs.jobs.get_mut("j-flying") {
        job.status = JobStatus::Finished;
    }
    rig.tick()?;
    assert!(rig.jobs.jobs["j-flying"].applied_at.is_some());
    // The live run's finished job is not this phase's business (the live-run
    // loop owns it), so call the phase alone.
    if let Some(job) = rig.jobs.jobs.get_mut("j-live") {
        job.status = JobStatus::Finished;
    }
    prune_dead_jobs(&rig.ctx, &mut rig.jobs, &rig.store, &mut rig.mail)?;
    assert!(rig.jobs.jobs["j-live"].applied_at.is_none());
    assert_eq!(rig.mail.calls.len(), calls);
    assert_eq!(abandoned(&rig)?.len(), 1);
    assert_eq!(rig.exceptions(), "");
    Ok(())
}
