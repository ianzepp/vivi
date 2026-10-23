//! Tests for the step collector helpers: a rejected word holds the step, and
//! `rerun_with` edits the rerun's spec.

use std::collections::BTreeMap;
use std::fs;

use tempfile::{TempDir, tempdir};

use super::{RoleCollector, RunCx, StepCollector};
use crate::error::VivariumError;
use crate::letter::fixture::record_body;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::def::FlowDef;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::tick::{OPERATOR_HOLD, WORD_REJECTED};
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::rung_result::ENV_FAILURE;
use crate::pump::jobs::{self, JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
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
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
    registry: Registry<Jobs, FakeMail>,
    def: FlowDef,
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, &record_body(NEED, "build"))?;
    start(&ctx, &store, &mut mail, NEED, Some("plan"))?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs: Jobs::default(),
        registry: Registry::new(),
        def: load(FlowKind::Build)?,
    })
}

impl Rig {
    fn plan_step(&self) -> Result<StepRow, VivariumError> {
        self.store
            .steps_of(NEED, 1)?
            .into_iter()
            .find(|row| row.node == "plan")
            .ok_or_else(|| VivariumError::Message("no plan step".into()))
    }

    fn exceptions(&self) -> String {
        fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }
}

#[test]
fn a_word_outside_the_node_class_holds_the_step_for_the_operator() -> R {
    let mut rig = rig()?;
    let step = rig.plan_step()?;
    let run = rig.store.runs_of(NEED)?.remove(0);
    let decide = |_: &_, _: &_| Err(VivariumError::Message("no decision".into()));
    let mut cx = RunCx {
        ctx: &rig.ctx,
        st: &mut rig.jobs,
        store: &rig.store,
        mail: &mut rig.mail,
        registry: &rig.registry,
        decide: &decide,
        def: rig.def.clone(),
        run,
    };
    cx.advance_word(&step, "no-such-word", serde_json::json!({}))?;

    let held = rig.plan_step()?;
    assert_eq!(held.state, StepState::Waiting);
    assert_eq!(held.verdict.as_deref(), Some(OPERATOR_HOLD));
    let log = rig.exceptions();
    assert_eq!(log.matches(WORD_REJECTED).count(), 1);
    assert!(log.contains("held for vivi pump answer"), "{log}");
    Ok(())
}

#[test]
fn rerun_with_applies_the_edit_and_inserts_the_next_attempt() -> R {
    let mut rig = rig()?;
    let step = rig.plan_step()?;
    let mut spec = JobSpec::new("plan-j1", "planner", "pump-planner", "s", "b");
    spec.task = Some(step.task.clone());
    spec.branch = Some("vivi/repair/x".into());
    jobs::enqueue(&mut rig.jobs, spec)?;
    let row = StepJobRow {
        job: "plan-j1".into(),
        run: 1,
        node: "plan".into(),
        task: step.task.clone(),
        attempt: 1,
    };
    rig.store.insert_step_job(NEED, &row)?;
    let job = rig.jobs.jobs["plan-j1"].clone();
    let run = rig.store.runs_of(NEED)?.remove(0);
    let decide = |_: &_, _: &_| Err(VivariumError::Message("no decision".into()));
    let mut cx = RunCx {
        ctx: &rig.ctx,
        st: &mut rig.jobs,
        store: &rig.store,
        mail: &mut rig.mail,
        registry: &rig.registry,
        decide: &decide,
        def: rig.def.clone(),
        run,
    };
    cx.rerun_with(&job, &row, &step, |spec| {
        spec.branch = Some("vivi/repair/x-t2".into());
    })?;

    let queued = &rig.jobs.jobs["plan-j1-a2"];
    assert_eq!(queued.branch.as_deref(), Some("vivi/repair/x-t2"));
    let stored = rig.store.step_job("plan-j1-a2")?.unwrap();
    assert_eq!(stored.attempt, row.attempt + 1);
    assert_eq!(queued.payload["attempt"], serde_json::json!(stored.attempt));
    assert_eq!(queued.payload["run"], serde_json::json!(stored.run));
    assert_eq!(queued.payload["node"], serde_json::json!(stored.node));
    assert_eq!(queued.payload["task"], serde_json::json!(stored.task));
    assert_eq!(rig.plan_step()?.state, StepState::Running);
    Ok(())
}

fn merge_job(publish_ref: Option<&str>, target: Option<&str>) -> PoolJob {
    let mut spec = JobSpec::new("j1", "merge", "merge", "s", "b");
    spec.publish_ref = publish_ref.map(str::to_owned);
    spec.target = target.map(str::to_owned);
    let mut jobs = Jobs::default();
    assert!(jobs::enqueue(&mut jobs, spec).is_ok());
    jobs.jobs["j1"].clone()
}

#[test]
fn a_merged_payload_names_the_branch_from_the_publish_ref() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let job = merge_job(Some("refs/heads/vivi/need/n1"), Some("default"));
    let payload = super::merged_payload(&ctx, &job, "abc");
    assert_eq!(
        payload,
        serde_json::json!({"sha": "abc", "commit": "abc", "branch": "vivi/need/n1"})
    );
}

#[test]
fn a_merged_payload_without_a_publish_ref_names_the_targets_main() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let expected = ctx.for_target("default").unwrap().cfg.main.clone();
    let job = merge_job(None, Some("default"));
    let payload = super::merged_payload(&ctx, &job, "abc");
    assert_eq!(payload["branch"], serde_json::json!(expected));
    assert_eq!(payload["commit"], serde_json::json!("abc"));
}

fn receipt(exit: i32, message: &str, stderr: &str) -> String {
    format!(
        "pool result j1: finished\npool p  slot s1  model m\nfrom main  default\n\
         push: none\nagent exit {exit} in 12s, job 20s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n{stderr}\n\n--- runner log tail\nlog line"
    )
}

/// Queue a finished job `id` of `role` with the pool result `text`.
fn finished_job(rig: &mut Rig, id: &str, role: &str, task: &str, text: String) -> R {
    let mut spec = JobSpec::new(id, role, role, "s", "b");
    spec.task = Some(task.to_owned());
    spec.from_ref = Some("main".into());
    jobs::enqueue(&mut rig.jobs, spec)?;
    let job = rig
        .jobs
        .jobs
        .get_mut(id)
        .ok_or_else(|| VivariumError::Message("no job".into()))?;
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt: text,
        at: 0,
    });
    Ok(())
}

/// A step row of `node` in the rig's run, sent as its own task.
fn step_of(rig: &mut Rig, node: &str) -> Result<StepRow, VivariumError> {
    if node == "plan" {
        return rig.plan_step();
    }
    let step = StepRow {
        run: 1,
        node: node.into(),
        task: format!("task-{node}"),
        state: StepState::Running,
        verdict: None,
    };
    rig.store.insert_step(NEED, &step)?;
    Ok(step)
}

/// Run the role collector over job `id` as the first attempt of `node`.
fn collect_role(rig: &mut Rig, id: &str, node: &str) -> Result<StepRow, VivariumError> {
    let step = step_of(rig, node)?;
    let class = rig
        .def
        .nodes
        .iter()
        .find(|n| n.id == node)
        .map(|n| n.class.clone())
        .ok_or_else(|| VivariumError::Message("no node".into()))?;
    let row = StepJobRow {
        job: id.into(),
        run: 1,
        node: node.into(),
        task: step.task.clone(),
        attempt: 1,
    };
    rig.store.insert_step_job(NEED, &row)?;
    let job = rig.jobs.jobs[id].clone();
    let run = rig.store.runs_of(NEED)?.remove(0);
    let decide = |_: &_, _: &_| Err(VivariumError::Message("no decision".into()));
    let mut cx = RunCx {
        ctx: &rig.ctx,
        st: &mut rig.jobs,
        store: &rig.store,
        mail: &mut rig.mail,
        registry: &rig.registry,
        decide: &decide,
        def: rig.def.clone(),
        run,
    };
    RoleCollector.collect(&mut cx, &job, &row, &step, &class)?;
    rig.store
        .steps_of(NEED, 1)?
        .into_iter()
        .find(|s| s.node == node)
        .ok_or_else(|| VivariumError::Message("step gone".into()))
}

fn env_lines(rig: &Rig) -> Vec<String> {
    rig.exceptions()
        .lines()
        .filter(|line| line.contains(ENV_FAILURE))
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_planner_job_with_agent_exit_127_holds_the_step_and_queues_no_attempt() -> R {
    let mut rig = rig()?;
    let step = rig.plan_step()?;
    let text = receipt(127, "", "");
    finished_job(&mut rig, "plan-j1", "planner", &step.task, text)?;
    let held = collect_role(&mut rig, "plan-j1", "plan")?;
    assert_eq!(held.state, StepState::Waiting);
    assert_eq!(held.verdict.as_deref(), Some(OPERATOR_HOLD));
    let lines = env_lines(&rig);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("planner job plan-j1"), "{lines:?}");
    assert_eq!(rig.jobs.jobs.len(), 1);
    assert!(rig.store.step_job("plan-j1-a2")?.is_none());
    Ok(())
}

#[test]
fn a_cto_job_with_agent_exit_127_holds_the_step() -> R {
    let mut rig = rig()?;
    finished_job(&mut rig, "cto-j1", "cto", "task-cto", receipt(127, "", ""))?;
    let held = collect_role(&mut rig, "cto-j1", "cto")?;
    assert_eq!(held.state, StepState::Waiting);
    assert_eq!(held.verdict.as_deref(), Some(OPERATOR_HOLD));
    let lines = env_lines(&rig);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("cto job cto-j1"), "{lines:?}");
    assert_eq!(rig.jobs.jobs.len(), 1);
    Ok(())
}

#[test]
fn an_auditor_with_exit_one_and_a_stderr_missing_tool_is_not_held() -> R {
    let mut rig = rig()?;
    let text = receipt(1, "no envelope", "bash: cargo: command not found");
    finished_job(&mut rig, "aud-j1", "auditor", "task-audit1", text)?;
    let step = collect_role(&mut rig, "aud-j1", "audit1")?;
    assert_ne!(step.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert!(env_lines(&rig).is_empty());
    let queued: Vec<_> = rig.jobs.jobs.keys().collect();
    assert!(rig.jobs.jobs.contains_key("aud-j1-c1"), "{queued:?}");
    Ok(())
}

#[test]
fn a_correction_job_of_an_exit_127_original_holds() -> R {
    let mut rig = rig()?;
    let step = rig.plan_step()?;
    let text = receipt(127, "", "");
    finished_job(&mut rig, "plan-j1", "planner", &step.task, text)?;
    let text = receipt(0, "junk", "");
    finished_job(&mut rig, "plan-j1-c1", "planner", &step.task, text)?;
    let held = collect_role(&mut rig, "plan-j1-c1", "plan")?;
    assert_eq!(held.state, StepState::Waiting);
    assert_eq!(held.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(env_lines(&rig).len(), 1);
    assert!(!rig.jobs.jobs.contains_key("plan-j1-c2"));
    Ok(())
}

#[test]
fn a_sync_merge_job_with_agent_exit_127_holds_the_step_and_raises_one_env_line() -> R {
    let mut rig = rig()?;
    finished_job(
        &mut rig,
        "sync-j1",
        "merge",
        "task-sync1",
        receipt(127, "", ""),
    )?;
    let held = collect_role(&mut rig, "sync-j1", "sync1")?;
    assert_eq!(held.state, StepState::Waiting);
    assert_eq!(held.verdict.as_deref(), Some(OPERATOR_HOLD));
    let lines = env_lines(&rig);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("merge job sync-j1"), "{lines:?}");
    assert_eq!(rig.jobs.jobs.len(), 1);
    Ok(())
}

#[test]
fn a_sync_merge_job_that_ran_and_published_nothing_still_answers_conflict() -> R {
    let mut rig = rig()?;
    // The conflict word routes onward, so the step's task must exist in the mail.
    let handle = MailWriter::send_step(&mut rig.mail, "mind", "mind", "sync1", "body", &[])?;
    let task = rig.mail.tasks.remove(&handle);
    rig.mail
        .tasks
        .extend(task.map(|t| ("task-sync1".to_owned(), t)));
    finished_job(
        &mut rig,
        "sync-j1",
        "merge",
        "task-sync1",
        receipt(0, "", ""),
    )?;
    let step = collect_role(&mut rig, "sync-j1", "sync1")?;
    assert!(env_lines(&rig).is_empty());
    assert_ne!(step.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(step.verdict.as_deref(), Some("conflict"));
    Ok(())
}
