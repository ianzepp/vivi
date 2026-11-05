//! Tests for the idempotent restart and its tick phase, over the fake mail
//! port with one injected failure per write.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::{TempDir, tempdir};

use super::{RESTART_STUCK, RestartResult, prune_dead_jobs, register, restart};
use crate::mailspace::pump_note::Note;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::{SENTINEL_NODE, start};
use crate::pump::flow::build_prereq::PREREQ_UNREADABLE;
use crate::pump::flow::def::VERDICT_SUPERSEDED;
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::fanout::SLICE_NODE_PREFIX;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::run_state;
use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, NeedStatus, TaskState, TaskStatus};
use crate::pump::flow::slice_dispatch::SliceMail;
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobSpec, JobStatus, JobStore, PoolJob, enqueue};
use crate::pump::store::build_waits::PrerequisiteState;
use crate::pump::store::{DB_FILENAME, PumpStore};
use vivi_error::VivariumError;
use vivi_mail::letter::fixture::step_body;
use vivi_mail::letter::records::NoteWord;
use vivi_mail::letter::work::StepFront;

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
        let task = mail.send_step("pump", "pump", node, &step_body(NEED, 1, node, ""), &[])?;
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
    crate::pump::flow::intake::register(&mut registry);
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

    fn restart(&mut self) -> Result<RestartResult, VivariumError> {
        let old = self.old()?;
        restart(&self.ctx, &self.store, &mut self.mail, &old, "plan")
    }

    /// A restart requested at a non-default entry of the build flow.
    fn restart_at(&mut self, entry: &str) -> Result<RestartResult, VivariumError> {
        let old = self.old()?;
        restart(&self.ctx, &self.store, &mut self.mail, &old, entry)
    }

    /// Make `NEED` a build whose prerequisite `dep` has not landed.
    fn blocked_by(&mut self, dep: &str) {
        self.mail.set_prerequisites(NEED, &[dep]);
    }

    fn entry_of(&self, run: u32) -> Result<Option<String>, VivariumError> {
        self.store.restart_entry(NEED, run)
    }

    fn sentinel(&self) -> Result<String, VivariumError> {
        self.store
            .steps_of(NEED, 1)?
            .into_iter()
            .find(|r| r.node == SENTINEL_NODE)
            .map(|r| r.task)
            .ok_or_else(|| VivariumError::Other("no sentinel row".into()))
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
    let RestartResult::Started(next) = rig.restart()? else {
        panic!("a restart with a free start starts its next run");
    };

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
            matches!(rig.state_of(1)?.as_str(), run_state::RESTARTING),
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
    rig.store.set_run_state(NEED, 1, run_state::RESTARTING)?;

    rig.host.set_now(epoch());
    rig.tick()?;
    assert_eq!(rig.exceptions().matches(RESTART_STUCK).count(), 0);

    rig.host.set_now(epoch() + 700);
    rig.tick()?;
    rig.tick()?;
    assert_eq!(rig.exceptions().matches(RESTART_STUCK).count(), 1);
    assert!(rig.exceptions().contains("n1 r1"));
    assert_eq!(rig.state_of(1)?, run_state::RESTARTING);
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

    let RestartResult::Started(next) = rig.restart()? else {
        panic!("a restart with a free start starts its next run");
    };
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
    let abandoned = |rig: &Rig| -> Result<Vec<(String, Note)>, VivariumError> {
        rig.mail.notes(&sentinel, NoteWord::Abandoned)
    };

    rig.tick()?;
    assert!(!rig.jobs.jobs.contains_key("j-pending"));
    let replies = abandoned(&rig)?;
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].1.prose, "j-flying");

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

#[test]
fn a_blocked_restart_waits_with_its_entry_and_starts_no_successor() -> R {
    let mut rig = rig()?;
    rig.blocked_by("dep");
    assert_eq!(rig.restart_at("rework1")?, RestartResult::Waiting);

    assert_eq!(rig.entry_of(1)?.as_deref(), Some("rework1"));
    assert_eq!(rig.state_of(1)?, run_state::RESTARTING);
    assert_eq!(rig.store.runs_of(NEED)?.len(), 1, "no successor run");
    assert_eq!(rig.mail.sentinel_count(NEED)?, 1, "no successor sentinel");
    let rows = rig.store.steps_of(NEED, 1)?;
    assert_eq!(rows.len(), 4, "the old run's step rows are untouched");
    assert!(rows.iter().all(|r| r.state != StepState::Done));
    let sentinel = rig.sentinel()?;
    assert_eq!(rig.mail.task_state(&sentinel)?.status, TaskStatus::Open);
    assert!(
        rig.mail
            .position(|c| matches!(c, Call::CloseTask { handle, .. } if *handle == sentinel))
            .is_none(),
        "the old sentinel stays open"
    );
    let waits = rig.store.build_waits()?;
    assert_eq!(waits.len(), 1, "the blocker is queued as the build wait");
    assert_eq!(waits[0].need, NEED);
    assert_eq!(waits[0].prerequisites.len(), 1);
    assert_eq!(waits[0].prerequisites[0].handle, "dep");
    assert_eq!(waits[0].prerequisites[0].state, PrerequisiteState::Open);
    assert_eq!(rig.exceptions(), "", "an open prerequisite waits silently");
    Ok(())
}

#[test]
fn a_blocked_restart_keeps_waiting_over_ticks_and_a_reopened_store() -> R {
    let mut rig = rig()?;
    rig.blocked_by("dep");
    assert_eq!(rig.restart_at("rework1")?, RestartResult::Waiting);

    rig.host.set_now(epoch());
    for _ in 0..3 {
        rig.tick()?;
    }
    assert_eq!(rig.state_of(1)?, run_state::RESTARTING);
    assert_eq!(rig.store.runs_of(NEED)?.len(), 1);
    assert_eq!(rig.mail.sentinel_count(NEED)?, 1);

    // Past the stuck line the blocked restart is still waiting, not stuck.
    rig.host.set_now(epoch() + 700);
    rig.tick()?;
    assert_eq!(rig.exceptions().matches(RESTART_STUCK).count(), 0);

    let path = rig._dir.path().join(DB_FILENAME);
    let reopened = PumpStore::open(&path)?;
    let runs = reopened.runs_of(NEED)?;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, run_state::RESTARTING);
    assert_eq!(
        reopened.restart_entry(NEED, 1)?.as_deref(),
        Some("rework1"),
        "the requested entry survives"
    );
    assert_eq!(reopened.build_waits()?.len(), 1);
    Ok(())
}

#[test]
fn a_successor_row_committed_before_a_dependency_arrives_still_recovers() -> R {
    let mut rig = rig()?;
    let old = rig.old()?;
    let next = RunRow {
        run: old.run + 1,
        state: run_state::STARTING.to_string(),
        ..old.clone()
    };
    rig.store.insert_run(&next)?;
    rig.store.set_restart_entry(NEED, old.run, "plan")?;
    rig.store
        .set_run_state(NEED, old.run, run_state::RESTARTING)?;
    rig.blocked_by("dep");

    rig.tick()?;
    assert_eq!(rig.state_of(1)?, run_state::SUPERSEDED);
    assert_eq!(rig.state_of(2)?, run_state::LIVE);
    assert!(
        rig.store.build_waits()?.is_empty(),
        "an already-created successor is recovered, not re-gated"
    );
    Ok(())
}

/// A fake mail whose prerequisite read fails, as an unreadable graph does.
struct Unreadable(FakeMail);

impl MailWriter for Unreadable {
    fn send_step(
        &mut self,
        from: &str,
        to: &str,
        subject: &str,
        body: &str,
        depends_on: &[String],
    ) -> Result<String, VivariumError> {
        self.0.send_step(from, to, subject, body, depends_on)
    }

    fn bind(&mut self, need: &str, units: &[String]) -> Result<(), VivariumError> {
        self.0.bind(need, units)
    }

    fn reply(&mut self, handle: &str, body: &str) -> Result<(), VivariumError> {
        self.0.reply(handle, body)
    }

    fn post_to_need_audience(&mut self, need: &str, body: &str) -> Result<(), VivariumError> {
        self.0.post_to_need_audience(need, body)
    }

    fn close_task(
        &mut self,
        handle: &str,
        verdict: &str,
        branch: Option<&str>,
        commit: Option<&str>,
    ) -> Result<(), VivariumError> {
        self.0.close_task(handle, verdict, branch, commit)
    }

    fn reopen_task(&mut self, handle: &str) -> Result<(), VivariumError> {
        self.0.reopen_task(handle)
    }

    fn close_need(&mut self, need: &str, note: &str) -> Result<(), VivariumError> {
        self.0.close_need(need, note)
    }

    fn open_record(&mut self, need: &str, body: &str) -> Result<String, VivariumError> {
        MailWriter::open_record(&mut self.0, need, body)
    }

    fn close_record(&mut self, need: &str, outcome: &str) -> Result<(), VivariumError> {
        self.0.close_record(need, outcome)
    }
}

impl MailReader for Unreadable {
    fn task_state(&self, handle: &str) -> Result<TaskState, VivariumError> {
        self.0.task_state(handle)
    }

    fn need_state(&self, need: &str) -> Result<NeedStatus, VivariumError> {
        self.0.need_state(need)
    }

    fn need_marks(&self, need: &str) -> Result<Vec<String>, VivariumError> {
        self.0.need_marks(need)
    }

    fn open_record(&self, need: &str) -> Result<Option<(String, String)>, VivariumError> {
        MailReader::open_record(&self.0, need)
    }

    fn bound_units(&self, need: &str) -> Result<Vec<String>, VivariumError> {
        self.0.bound_units(need)
    }

    fn sentinel_count(&self, need: &str) -> Result<usize, VivariumError> {
        self.0.sentinel_count(need)
    }

    fn need_prerequisites(&self, _need: &str) -> Result<Vec<String>, VivariumError> {
        Err(VivariumError::Message("graph unreadable".into()))
    }

    fn last_closed_record(&self, need: &str) -> Result<Option<String>, VivariumError> {
        self.0.last_closed_record(need)
    }

    fn sent_step(&self, subject: &str, step: &StepFront) -> Result<Option<String>, VivariumError> {
        self.0.sent_step(subject, step)
    }
}

#[test]
fn an_unreadable_prerequisite_read_fails_closed_and_keeps_the_intent() -> R {
    let mut rig = rig()?;
    let old = rig.old()?;
    let inner = std::mem::replace(&mut rig.mail, FakeMail::new("pump"));
    let mut mail = Unreadable(inner);
    let result = restart(&rig.ctx, &rig.store, &mut mail, &old, "rework1")?;
    rig.mail = mail.0;
    assert_eq!(result, RestartResult::Waiting);

    assert_eq!(rig.entry_of(1)?.as_deref(), Some("rework1"));
    assert_eq!(rig.state_of(1)?, run_state::RESTARTING);
    assert_eq!(rig.store.runs_of(NEED)?.len(), 1, "no successor run");
    assert_eq!(rig.mail.sentinel_count(NEED)?, 1, "no successor sentinel");
    assert_eq!(
        rig.exceptions().matches(PREREQ_UNREADABLE).count(),
        1,
        "the shared unreadable diagnostic is raised once: {}",
        rig.exceptions()
    );
    // A later tick with a readable graph starts the deferred successor.
    rig.tick()?;
    assert_eq!(rig.state_of(2)?, run_state::LIVE);
    assert_eq!(rig.state_of(1)?, run_state::SUPERSEDED);
    Ok(())
}
