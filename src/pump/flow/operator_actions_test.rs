//! Tests for the operator-actions phase: replan, withdraw and requeue on a
//! need with an active run go to the flow, once each; recover is refused; a
//! need without a run, and answer and unlock letters, are left alone.

use std::collections::BTreeMap;
use std::fs;

use super::{ACTION_REFUSED, has_active_run, register};
use crate::error::VivariumError;
use crate::mailspace::{PumpActionKind, PumpActionRequest};
use crate::pump::fixture::{Project, project};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::def::STEP_MARKER;
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::host::{Ctx, FakeHost};
use crate::pump::jobs::{JobSpec, JobStatus, JobStore, PoolJob, dispatch_pending, enqueue};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::{RoleRouting, RoutedPool, RoutedRole};

type R = Result<(), VivariumError>;

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
    project: Project,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
    registry: Registry<Jobs, FakeMail>,
    need: String,
    step: String,
}

/// A real need with an open record, a fake-port run r1 with a `build1` step
/// that has one job `hand-a2` at attempt 2.
fn rig() -> Result<Rig, VivariumError> {
    let project = project();
    let ctx = project.pump_ctx();
    let store = PumpStore::open(&project.home().join(DB_FILENAME))?;
    let need = project.need("fix auth");
    project.mailspace.pump_send(None, &need, None)?;
    let mut mail = FakeMail::new("pump");
    MailWriter::open_record(&mut mail, &need, "record")?;
    start(&ctx, &store, &mut mail, &need, None)?;
    let step = mail.send_step(
        "pump",
        "pump",
        "build1",
        &format!("{STEP_MARKER} {need} r1 build1"),
        &[],
    )?;
    mail.bind(&need, std::slice::from_ref(&step))?;
    store.insert_step(
        &need,
        &StepRow {
            run: 1,
            node: "build1".into(),
            task: step.clone(),
            state: StepState::Running,
            verdict: None,
        },
    )?;
    let mut jobs = Jobs::default();
    let mut spec = JobSpec::new("hand-a2", "hand", "hand", "subject", "body");
    spec.task = Some(step.clone());
    spec.budget = 77;
    enqueue(&mut jobs, spec)?;
    if let Some(job) = jobs.jobs.get_mut("hand-a2") {
        job.status = JobStatus::Finished;
    }
    store.insert_step_job(
        &need,
        &StepJobRow {
            job: "hand-a2".into(),
            run: 1,
            node: "build1".into(),
            task: step.clone(),
            attempt: 2,
        },
    )?;
    let mut registry = Registry::new();
    register(&mut registry);
    Ok(Rig {
        project,
        ctx,
        store,
        mail,
        jobs,
        registry,
        need,
        step,
    })
}

impl Rig {
    fn post(&self, request: &PumpActionRequest) -> Result<String, VivariumError> {
        Ok(self
            .project
            .mailspace
            .pump_post_action(None, &self.need, request)?
            .id)
    }

    fn phase(&mut self) -> R {
        for phase in self.registry.phases() {
            phase.run(&self.ctx, &mut self.jobs, &self.store, &mut self.mail)?;
        }
        Ok(())
    }

    fn applied(&self, id: &str) -> Result<bool, VivariumError> {
        Ok(self.store.applied_action_ids()?.contains(id))
    }

    fn exceptions(&self) -> String {
        fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }
}

#[test]
fn a_run_in_an_active_state_counts_and_a_missing_one_does_not() -> R {
    let rig = rig()?;
    assert!(has_active_run(&rig.store, &rig.need)?);
    assert!(!has_active_run(&rig.store, "other-need")?);
    rig.store.set_run_state(&rig.need, 1, "done")?;
    assert!(!has_active_run(&rig.store, &rig.need)?);
    Ok(())
}

#[test]
fn a_replan_restarts_the_live_run_at_plan_once() -> R {
    let mut rig = rig()?;
    let id = rig.post(&PumpActionRequest::replan(false))?;
    rig.phase()?;

    let runs = rig.store.runs_of(&rig.need)?;
    assert_eq!(runs.len(), 2);
    assert_eq!((runs[1].run, runs[1].entry.as_str()), (2, "plan"));
    assert_eq!(runs[0].state, "superseded");
    assert!(rig.applied(&id)?);

    rig.phase()?;
    assert_eq!(rig.store.runs_of(&rig.need)?.len(), 2);
    Ok(())
}

#[test]
fn a_withdraw_closes_the_record_once() -> R {
    let mut rig = rig()?;
    let id = rig.post(&PumpActionRequest::withdraw("stop"))?;
    rig.phase()?;
    rig.phase()?;

    let closes = rig
        .mail
        .calls
        .iter()
        .filter(|c| matches!(c, Call::CloseRecord { outcome, .. } if outcome == "withdrawn"))
        .count();
    assert_eq!(closes, 1);
    assert!(rig.applied(&id)?);
    Ok(())
}

#[test]
fn a_requeue_queues_the_newest_job_again_at_the_same_attempt_once() -> R {
    let mut rig = rig()?;
    let step = rig.step.clone();
    let id = rig.post(&PumpActionRequest::requeue(
        &step, None, false, false, "again",
    ))?;
    rig.phase()?;
    rig.phase()?;

    let copy = rig.jobs.jobs().get("hand-a2-q1").expect("queued copy");
    assert_eq!(copy.task.as_deref(), Some(step.as_str()));
    assert_eq!(copy.budget, 77);
    assert!(!rig.jobs.jobs().contains_key("hand-a2-q2"));
    let row = rig.store.step_job("hand-a2-q1")?.expect("step job row");
    assert_eq!((row.attempt, row.task.as_str()), (2, step.as_str()));
    assert_eq!(copy.payload["attempt"], 2);
    assert_eq!(copy.payload["task"], step.as_str());
    assert_eq!(copy.payload["node"], "build1");
    assert_eq!(copy.payload["need"], rig.need.as_str());
    let state = rig.store.step_by_task(&step)?.map(|s| s.state);
    assert_eq!(state, Some(StepState::Running));
    assert!(rig.applied(&id)?);

    if let Some(job) = rig.jobs.jobs.get_mut("hand-a2-q1") {
        job.status = JobStatus::Finished;
    }
    let again = rig.post(&PumpActionRequest::requeue(
        &step, None, false, false, "twice",
    ))?;
    rig.phase()?;
    assert!(rig.jobs.jobs().contains_key("hand-a2-q2"));
    assert!(rig.applied(&again)?);
    Ok(())
}

#[test]
fn a_requeue_of_a_task_outside_the_run_is_refused_and_spent() -> R {
    let mut rig = rig()?;
    let id = rig.post(&PumpActionRequest::requeue(
        "no-such-task",
        None,
        false,
        false,
        "x",
    ))?;
    rig.phase()?;

    assert!(rig.applied(&id)?);
    assert!(rig.exceptions().contains(ACTION_REFUSED));
    assert_eq!(rig.jobs.jobs().len(), 1);
    Ok(())
}

#[test]
fn an_action_on_a_need_without_a_run_is_left_for_the_old_pump() -> R {
    let mut rig = rig()?;
    rig.store.set_run_state(&rig.need, 1, "done")?;
    let id = rig.post(&PumpActionRequest::replan(false))?;
    rig.phase()?;

    assert!(!rig.applied(&id)?);
    assert_eq!(rig.store.runs_of(&rig.need)?.len(), 1);
    Ok(())
}

#[test]
fn a_recover_on_a_need_with_a_live_run_is_refused_once_and_spent() -> R {
    let mut rig = rig()?;
    let id = rig.post(&PumpActionRequest {
        kind: PumpActionKind::Recover,
        ..PumpActionRequest::withdraw("adopt")
    })?;
    rig.phase()?;
    rig.phase()?;

    assert!(rig.applied(&id)?);
    let text = rig.exceptions();
    assert_eq!(text.matches(ACTION_REFUSED).count(), 1);
    assert!(text.contains(&rig.need));
    assert_eq!(rig.store.runs_of(&rig.need)?.len(), 1);
    Ok(())
}

#[test]
fn a_requeue_of_a_dispatched_job_is_refused_and_queues_nothing() -> R {
    let mut rig = rig()?;
    if let Some(job) = rig.jobs.jobs.get_mut("hand-a2") {
        job.status = JobStatus::Dispatched;
    }
    let step = rig.step.clone();
    let id = rig.post(&PumpActionRequest::requeue(&step, None, false, false, "x"))?;
    rig.phase()?;

    assert!(rig.applied(&id)?);
    assert!(rig.exceptions().contains(ACTION_REFUSED));
    assert_eq!(rig.jobs.jobs().len(), 1);
    Ok(())
}

#[test]
fn a_requeue_releases_a_held_pending_job_and_dispatch_offers_it_again() -> R {
    let mut rig = rig()?;
    if let Some(job) = rig.jobs.jobs.get_mut("hand-a2") {
        job.status = JobStatus::Pending;
        job.dispatch_refusals = 3;
        job.held = Some(String::from("no usable bridge"));
    }
    let host = FakeHost::new();
    host.reply("pool status", 0, "slot primary bridge slot-1 free\n");
    host.reply(
        "pool dispatch",
        0,
        "dispatched t bridge slot-1 model/default unit/task\n",
    );
    let mut ctx = rig.ctx.clone();
    ctx.host = host.clone();
    ctx.routing = std::sync::Arc::new(RoleRouting {
        roles: vec![RoutedRole {
            name: String::from("hand"),
            kind: None,
            status: String::from("active"),
            pool: Some(String::from("primary")),
            read_only: None,
            model: None,
            models: Vec::new(),
        }],
        pools: BTreeMap::from([(
            String::from("primary"),
            RoutedPool {
                slots: 1,
                provider: String::from("local"),
                fallback: None,
                caps: BTreeMap::new(),
                models: Vec::new(),
                has_bands: false,
            },
        )]),
    });
    assert!(dispatch_pending(&ctx, &mut rig.jobs)?.is_empty());

    let step = rig.step.clone();
    let id = rig.post(&PumpActionRequest::requeue(&step, None, false, false, "go"))?;
    rig.phase()?;

    assert!(rig.applied(&id)?);
    assert!(!rig.exceptions().contains(ACTION_REFUSED));
    assert_eq!(rig.jobs.jobs().len(), 1, "released in place, not copied");
    let job = &rig.jobs.jobs()["hand-a2"];
    assert!(job.held.is_none());
    assert_eq!(job.dispatch_refusals, 0);
    assert_eq!(dispatch_pending(&ctx, &mut rig.jobs)?, ["hand-a2"]);
    assert_eq!(rig.jobs.jobs()["hand-a2"].status, JobStatus::Dispatched);
    Ok(())
}

/// Mark `id` Pending and held, its payload stamped with step `task` of `need`.
fn hold(jobs: &mut Jobs, id: &str, need: &str, task: &str) {
    if let Some(job) = jobs.jobs.get_mut(id) {
        job.status = JobStatus::Pending;
        job.dispatch_refusals = 3;
        job.held = Some(String::from("no usable bridge"));
        job.payload = serde_json::json!({ "need": need, "task": task });
    }
}

#[test]
fn a_requeue_releases_a_held_correction_job_in_place() -> R {
    let mut rig = rig()?;
    let (need, step) = (rig.need.clone(), rig.step.clone());
    let mut spec = JobSpec::new("hand-a2-c1", "hand", "hand", "fix", "body");
    spec.task = Some(step.clone());
    enqueue(&mut rig.jobs, spec)?;
    hold(&mut rig.jobs, "hand-a2-c1", &need, &step);
    rig.store.insert_step_job(
        &need,
        &StepJobRow {
            job: "hand-a2-c1".into(),
            run: 1,
            node: "build1".into(),
            task: step.clone(),
            attempt: 3,
        },
    )?;

    let id = rig.post(&PumpActionRequest::requeue(&step, None, false, false, "go"))?;
    rig.phase()?;

    assert!(rig.applied(&id)?);
    assert_eq!(rig.jobs.jobs().len(), 2, "released in place, not copied");
    let job = &rig.jobs.jobs()["hand-a2-c1"];
    assert!(job.held.is_none());
    assert_eq!(job.dispatch_refusals, 0);
    Ok(())
}

#[test]
fn a_requeue_releases_a_held_slice_merge_job_and_reruns_no_hand_job() -> R {
    let mut rig = rig()?;
    let (need, step) = (rig.need.clone(), rig.step.clone());
    // The slice's merge job has the slice task and no `step_jobs` row; the
    // slice's finished hand job (`hand-a2`) is the only row.
    let mut spec = JobSpec::new("merge-s1", "merge", "merge", "land", "body");
    spec.task = Some(step.clone());
    enqueue(&mut rig.jobs, spec)?;
    hold(&mut rig.jobs, "merge-s1", &need, &step);

    let id = rig.post(&PumpActionRequest::requeue(&step, None, false, false, "go"))?;
    rig.phase()?;

    assert!(rig.applied(&id)?);
    assert!(!rig.exceptions().contains(ACTION_REFUSED));
    assert_eq!(rig.jobs.jobs().len(), 2, "no job added");
    assert!(!rig.jobs.jobs().contains_key("hand-a2-q1"));
    let job = &rig.jobs.jobs()["merge-s1"];
    assert!(job.held.is_none());
    assert_eq!(job.dispatch_refusals, 0);
    assert_eq!(rig.jobs.jobs()["hand-a2"].status, JobStatus::Finished);
    Ok(())
}

#[test]
fn a_requeue_on_a_done_step_is_refused_and_queues_nothing() -> R {
    let mut rig = rig()?;
    rig.store
        .set_step_state(&rig.step, StepState::Done, Some("built"))?;
    let step = rig.step.clone();
    let id = rig.post(&PumpActionRequest::requeue(&step, None, false, false, "x"))?;
    rig.phase()?;

    assert!(rig.applied(&id)?);
    assert!(rig.exceptions().contains(ACTION_REFUSED));
    assert_eq!(rig.jobs.jobs().len(), 1);
    let state = rig.store.step_by_task(&step)?.map(|s| s.state);
    assert_eq!(state, Some(StepState::Done));
    Ok(())
}

#[test]
fn an_unlock_letter_is_left_unapplied_by_this_phase() -> R {
    let mut rig = rig()?;
    let id = rig.post(&PumpActionRequest::unlock(None, "go"))?;
    rig.phase()?;

    assert!(!rig.applied(&id)?);
    assert!(!rig.exceptions().contains(ACTION_REFUSED));
    Ok(())
}
