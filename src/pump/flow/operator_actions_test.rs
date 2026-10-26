//! Tests for the operator-actions phase: replan, withdraw and requeue on a
//! need with an active run go to the flow, once each; on a need with no active
//! run a withdraw closes the record and a requeue or replan is refused, also
//! once each; answer and unlock letters are left alone.

use std::collections::BTreeMap;
use std::fs;

use super::{ACTION_REFUSED, active_run, register};
use crate::error::VivariumError;
use crate::letter::fixture::step_body;
use crate::mailspace::PumpActionRequest;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::host::{Ctx, FakeHost};
use crate::pump::jobs::{
    JobOutcome, JobSpec, JobStatus, JobStore, PoolJob, dispatch_pending, enqueue,
};
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
        &step_body(&need, 1, "build1", ""),
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
    assert!(active_run(&rig.store, &rig.need)?.is_some());
    assert!(active_run(&rig.store, "other-need")?.is_none());
    rig.store.set_run_state(&rig.need, 1, "done")?;
    assert!(active_run(&rig.store, &rig.need)?.is_none());
    Ok(())
}

#[test]
fn a_replan_restarts_the_live_run_at_plan_once() -> R {
    let mut rig = rig()?;
    let id = rig.post(&PumpActionRequest::replan())?;
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
    let id = rig.post(&PumpActionRequest::requeue(&step, "again"))?;
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
    let again = rig.post(&PumpActionRequest::requeue(&step, "twice"))?;
    rig.phase()?;
    assert!(rig.jobs.jobs().contains_key("hand-a2-q2"));
    assert!(rig.applied(&again)?);
    Ok(())
}

/// A fully populated finished job, with runtime ownership and result state
/// that must not travel into a requeued copy.
fn rich_finished_job(step: &str) -> PoolJob {
    let mut spec = JobSpec::new("hand-a2", "hand", "hand", "subject", "body");
    spec.task = Some(step.to_string());
    spec.pool_candidates = vec!["alpha".into(), "beta".into()];
    spec.local_only = true;
    spec.pool_exclude = vec!["refused".into()];
    spec.target = Some("target-repo".into());
    spec.extra_targets = vec!["side-repo".into()];
    spec.from_ref = Some("refs/heads/from".into());
    spec.with = vec!["refs/heads/extra".into()];
    spec.model = Some("model-x".into());
    spec.command = Some("run.sh".into());
    spec.builtin_role = Some("tugboat".into());
    spec.branch = Some("vivi/work/x".into());
    spec.allow_default_branch = true;
    spec.publish_ref = Some("refs/heads/publish".into());
    spec.publish_expect = Some("expect-sha".into());
    spec.publish_contains = Some("contain-sha".into());
    spec.delete_ref = Some("refs/heads/delete".into());
    spec.delete_expect = Some("delete-sha".into());
    spec.budget = 900;
    spec.read_only = true;
    spec.units = vec!["unit-1".into()];
    spec.source_jobs = vec!["src-1".into()];
    spec.payload = serde_json::json!({"custom": 7});
    let mut job = PoolJob::from(spec);
    job.queued_at = 100;
    job.pool = Some("alpha".into());
    job.dispatched_at = Some(200);
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: "ok".into(),
        receipt: "receipt text".into(),
        at: 300,
    });
    job.applied_at = Some(400);
    job.dispatch_refusals = 2;
    job.held = Some("too many refusals".into());
    job
}

#[test]
fn a_requeue_preserves_the_complete_dispatch_contract() -> R {
    let mut rig = rig()?;
    let step = rig.step.clone();
    let rich = rich_finished_job(&step);
    *rig.jobs.jobs.get_mut("hand-a2").expect("rig job") = rich.clone();
    let id = rig.post(&PumpActionRequest::requeue(&step, "again"))?;
    rig.phase()?;

    let copy = rig.jobs.jobs.get("hand-a2-q1").expect("queued copy");
    // Only the identity and the restamped payload differ from the source.
    assert_eq!(copy.id, "hand-a2-q1");
    assert_eq!(copy.kind, rich.kind);
    assert_eq!(copy.role, rich.role);
    assert_eq!(copy.subject, rich.subject);
    assert_eq!(copy.body, rich.body);
    assert_eq!(copy.pool_candidates, rich.pool_candidates);
    assert_eq!(copy.local_only, rich.local_only);
    assert_eq!(copy.pool_exclude, rich.pool_exclude);
    assert_eq!(copy.target, rich.target);
    assert_eq!(copy.extra_targets, rich.extra_targets);
    assert_eq!(copy.from_ref, rich.from_ref);
    assert_eq!(copy.with, rich.with);
    assert_eq!(copy.model, rich.model);
    assert_eq!(copy.command, rich.command);
    assert_eq!(copy.builtin_role, rich.builtin_role);
    assert_eq!(copy.branch, rich.branch);
    assert_eq!(copy.allow_default_branch, rich.allow_default_branch);
    assert_eq!(copy.publish_ref, rich.publish_ref);
    assert_eq!(copy.publish_expect, rich.publish_expect);
    assert_eq!(copy.publish_contains, rich.publish_contains);
    assert_eq!(copy.delete_ref, rich.delete_ref);
    assert_eq!(copy.delete_expect, rich.delete_expect);
    assert_eq!(copy.budget, rich.budget);
    assert_eq!(copy.read_only, rich.read_only);
    assert_eq!(copy.units, rich.units);
    assert_eq!(copy.source_jobs, rich.source_jobs);
    assert_eq!(copy.payload["custom"], rich.payload["custom"]);
    assert_eq!(copy.task, rich.task);
    // Fresh pending runtime state: nothing owned or reported travels.
    assert_eq!(copy.status, JobStatus::Pending);
    assert_eq!(copy.pool, None);
    assert_eq!(copy.dispatched_at, None);
    assert_eq!(copy.outcome, None);
    assert_eq!(copy.applied_at, None);
    assert_eq!(copy.dispatch_refusals, 0);
    assert_eq!(copy.held, None);
    assert_ne!(copy.queued_at, rich.queued_at);
    // Same attempt, identity restamped into the payload.
    let row = rig.store.step_job("hand-a2-q1")?.expect("step job row");
    assert_eq!((row.attempt, row.task.as_str()), (2, step.as_str()));
    assert_eq!(copy.payload["attempt"], 2);
    assert!(rig.applied(&id)?);
    Ok(())
}

#[test]
fn a_requeue_of_a_task_outside_the_run_is_refused_and_spent() -> R {
    let mut rig = rig()?;
    let id = rig.post(&PumpActionRequest::requeue("no-such-task", "x"))?;
    rig.phase()?;

    assert!(rig.applied(&id)?);
    assert!(rig.exceptions().contains(ACTION_REFUSED));
    assert_eq!(rig.jobs.jobs().len(), 1);
    Ok(())
}

fn withdraw_closes(rig: &Rig) -> usize {
    rig.mail
        .calls
        .iter()
        .filter(|c| matches!(c, Call::CloseRecord { outcome, .. } if outcome == "withdrawn"))
        .count()
}

#[test]
fn a_withdraw_on_a_need_without_a_run_closes_its_record_once() -> R {
    let mut rig = rig()?;
    rig.store.set_run_state(&rig.need, 1, "done")?;
    let id = rig.post(&PumpActionRequest::withdraw("stop"))?;
    rig.phase()?;
    rig.phase()?;

    assert_eq!(withdraw_closes(&rig), 1);
    assert!(rig.applied(&id)?);
    assert!(!rig.exceptions().contains(ACTION_REFUSED));
    Ok(())
}

#[test]
fn a_withdraw_on_a_need_that_never_had_a_run_closes_its_record() -> R {
    let mut rig = rig()?;
    rig.need = rig.project.need("other work");
    rig.project.mailspace.pump_send(None, &rig.need, None)?;
    MailWriter::open_record(&mut rig.mail, &rig.need, "record")?;
    let id = rig.post(&PumpActionRequest::withdraw("stop"))?;
    rig.phase()?;

    assert_eq!(withdraw_closes(&rig), 1);
    assert!(rig.applied(&id)?);
    Ok(())
}

#[test]
fn a_replan_on_a_need_without_a_run_is_refused_once() -> R {
    let mut rig = rig()?;
    rig.store.set_run_state(&rig.need, 1, "done")?;
    let id = rig.post(&PumpActionRequest::replan())?;
    rig.phase()?;
    let alarms = rig.exceptions();
    rig.phase()?;

    assert!(rig.applied(&id)?);
    assert!(alarms.contains(ACTION_REFUSED));
    assert!(alarms.contains(&rig.need));
    assert!(alarms.contains("no active flow run"));
    assert_eq!(
        rig.exceptions(),
        alarms,
        "a spent letter raises nothing again"
    );
    assert_eq!(rig.store.runs_of(&rig.need)?.len(), 1);
    Ok(())
}

#[test]
fn a_requeue_on_a_need_without_a_run_is_refused_once() -> R {
    let mut rig = rig()?;
    rig.store.set_run_state(&rig.need, 1, "withdrawn")?;
    let id = rig.post(&PumpActionRequest::requeue(&rig.step, "x"))?;
    rig.phase()?;
    let alarms = rig.exceptions();
    rig.phase()?;

    assert!(rig.applied(&id)?);
    assert!(alarms.contains(ACTION_REFUSED));
    assert!(alarms.contains("vivi pump unlock"));
    assert_eq!(rig.exceptions(), alarms);
    assert_eq!(rig.jobs.jobs().len(), 1);
    Ok(())
}

#[test]
fn a_requeue_of_a_dispatched_job_is_refused_and_queues_nothing() -> R {
    let mut rig = rig()?;
    if let Some(job) = rig.jobs.jobs.get_mut("hand-a2") {
        job.status = JobStatus::Dispatched;
    }
    let step = rig.step.clone();
    let id = rig.post(&PumpActionRequest::requeue(&step, "x"))?;
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
    let id = rig.post(&PumpActionRequest::requeue(&step, "go"))?;
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

    let id = rig.post(&PumpActionRequest::requeue(&step, "go"))?;
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

    let id = rig.post(&PumpActionRequest::requeue(&step, "go"))?;
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
    let id = rig.post(&PumpActionRequest::requeue(&step, "x"))?;
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
