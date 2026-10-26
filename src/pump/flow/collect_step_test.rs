//! Tests for the step collector helpers: a rejected word holds the step, and
//! `rerun_with` edits the rerun's spec.

use std::collections::BTreeMap;
use std::fs;

use tempfile::{TempDir, tempdir};

use super::{RoleCollector, RunCx, StepCollector};
use crate::error::VivariumError;
use crate::letter::fixture::record_body;
use crate::pump::config::{DispatchConfig, PumpConfig, StepBudgets};
use crate::pump::envelope::EnvelopeError;
use crate::pump::envelope::correct;
use crate::pump::envelope::roles::RoleKind;
use crate::pump::flow::advance::route::JoinInput;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::budget_retry::{PAYLOAD_KEY, raised_budget};
use crate::pump::flow::def::FlowDef;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::def::FlowNode;
use crate::pump::flow::def::NodeClass;
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
    rig_with(DispatchConfig::default())
}

fn rig_with(dispatch: DispatchConfig) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(
        dir.path(),
        PumpConfig {
            dispatch,
            ..PumpConfig::default()
        },
    );
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
    spec.pool_candidates = vec!["alpha".into()];
    spec.pool_exclude = vec!["refused".into()];
    spec.extra_targets = vec!["side-repo".into()];
    spec.from_ref = Some("refs/heads/from".into());
    spec.with = vec!["refs/heads/extra".into()];
    spec.model = Some("model-old".into());
    spec.command = Some("run.sh".into());
    spec.builtin_role = Some("tugboat".into());
    spec.allow_default_branch = true;
    spec.publish_expect = Some("expect-sha".into());
    spec.publish_contains = Some("contain-sha".into());
    spec.delete_ref = Some("refs/heads/delete".into());
    spec.delete_expect = Some("delete-sha".into());
    spec.budget = 600;
    spec.read_only = true;
    spec.units = vec!["unit-1".into()];
    spec.source_jobs = vec!["src-1".into()];
    spec.payload = serde_json::json!({"plan_file": "plan/one.md"});
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
        spec.model = Some("model-new".into());
    })?;

    let queued = &rig.jobs.jobs["plan-j1-a2"];
    assert_eq!(queued.branch.as_deref(), Some("vivi/repair/x-t2"));
    // The edit wins over the copied field.
    assert_eq!(queued.model.as_deref(), Some("model-new"));
    // Every other execution input survives the copy.
    assert_eq!(queued.pool_candidates, vec!["alpha"]);
    assert_eq!(queued.pool_exclude, vec!["refused"]);
    assert_eq!(queued.extra_targets, vec!["side-repo"]);
    assert_eq!(queued.from_ref.as_deref(), Some("refs/heads/from"));
    assert_eq!(queued.with, vec!["refs/heads/extra"]);
    assert_eq!(queued.command.as_deref(), Some("run.sh"));
    assert_eq!(queued.builtin_role.as_deref(), Some("tugboat"));
    assert!(queued.allow_default_branch);
    assert_eq!(queued.publish_expect.as_deref(), Some("expect-sha"));
    assert_eq!(queued.publish_contains.as_deref(), Some("contain-sha"));
    assert_eq!(queued.delete_ref.as_deref(), Some("refs/heads/delete"));
    assert_eq!(queued.delete_expect.as_deref(), Some("delete-sha"));
    assert_eq!(queued.budget, 600);
    assert!(queued.read_only);
    assert_eq!(queued.units, vec!["unit-1"]);
    assert_eq!(queued.source_jobs, vec!["src-1"]);
    // Next-row accounting: the row is stored at the next attempt.
    let stored = rig.store.step_job("plan-j1-a2")?.unwrap();
    assert_eq!(stored.attempt, row.attempt + 1);
    assert_eq!(queued.payload["attempt"], serde_json::json!(stored.attempt));
    assert_eq!(queued.payload["run"], serde_json::json!(stored.run));
    assert_eq!(queued.payload["node"], serde_json::json!(stored.node));
    assert_eq!(queued.payload["task"], serde_json::json!(stored.task));
    // Payload keys the stamp does not own survive the copy.
    assert_eq!(
        queued.payload["plan_file"],
        serde_json::json!("plan/one.md")
    );
    assert_eq!(rig.plan_step()?.state, StepState::Running);
    Ok(())
}

#[test]
fn rerun_with_preserves_execution_context() -> R {
    let mut rig = rig()?;
    let step = rig.plan_step()?;
    let mut spec = JobSpec::new(
        "plan-j1",
        "planner",
        "pump-planner",
        "plan the slice",
        "body",
    );
    spec.task = Some(step.task.clone());
    spec.target = Some("default".into());
    spec.from_ref = Some("refs/heads/from".into());
    spec.with = vec!["refs/heads/extra".into()];
    spec.extra_targets = vec!["side-repo".into()];
    spec.pool_candidates = vec!["alpha".into()];
    spec.pool_exclude = vec!["refused".into()];
    spec.local_only = true;
    spec.read_only = true;
    spec.branch = Some("vivi/repair/x".into());
    spec.allow_default_branch = true;
    spec.publish_ref = Some("refs/heads/vivi/repair/x".into());
    spec.publish_expect = Some("expect-sha".into());
    spec.publish_contains = Some("contain-sha".into());
    spec.delete_ref = Some("refs/heads/delete".into());
    spec.delete_expect = Some("delete-sha".into());
    spec.model = Some("model-q4".into());
    spec.command = Some("harness.sh".into());
    spec.builtin_role = Some("tugboat".into());
    spec.units = vec!["unit-1".into()];
    spec.source_jobs = vec!["src-1".into()];
    spec.budget = 600;
    spec.payload = serde_json::json!({"plan_file": "plan/one.md", (PAYLOAD_KEY): true});
    jobs::enqueue(&mut rig.jobs, spec)?;
    // Runtime ownership and result state on the finished job must not
    // travel: make the queued job dirty before the rerun.
    let owned = rig.jobs.jobs.get_mut("plan-j1").unwrap();
    owned.status = JobStatus::Dispatched;
    owned.pool = Some("fast".into());
    owned.dispatched_at = Some(11);
    owned.outcome = Some(JobOutcome {
        state: "lost".to_owned(),
        detail: "detail".to_owned(),
        receipt: "agent exit 124 after 600s\n".to_owned(),
        at: 12,
    });
    owned.applied_at = Some(13);
    owned.dispatch_refusals = 2;
    owned.held = Some("refused".into());
    let job = rig.jobs.jobs["plan-j1"].clone();
    let row = StepJobRow {
        job: "plan-j1".into(),
        run: 1,
        node: "plan".into(),
        task: step.task.clone(),
        attempt: 1,
    };
    rig.store.insert_step_job(NEED, &row)?;
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
        run: run.clone(),
    };
    // Repair-style fresh refs: the edit must survive into the queued attempt.
    let edit = |spec: &mut JobSpec| {
        spec.publish_ref = Some("refs/heads/vivi/repair/x-t2".into());
        spec.branch = Some("vivi/repair/x-t2".into());
    };
    cx.rerun_with(&job, &row, &step, &edit)?;
    // A literal replay of the same rerun specification enqueues no duplicate
    // and no different work: enqueue recognizes the identical spec as the
    // already-queued attempt, and the step-job row's primary key refuses the
    // repeat instead of storing it twice.
    assert!(cx.rerun_with(&job, &row, &step, &edit).is_err());

    let queued = &rig.jobs.jobs["plan-j1-a2"];
    assert_eq!(queued.id, "plan-j1-a2");
    // Fresh pending runtime state: no pool ownership, timestamps, outcome,
    // applied stamp, refusal count or held reason travels.
    assert_eq!(queued.status, JobStatus::Pending);
    assert_eq!(queued.pool, None);
    assert_eq!(queued.dispatched_at, None);
    assert_eq!(queued.outcome, None);
    assert_eq!(queued.applied_at, None);
    assert_eq!(queued.dispatch_refusals, 0);
    assert_eq!(queued.held, None);
    // Every execution input survives the copy.
    assert_eq!(queued.kind, "planner");
    assert_eq!(queued.role, "pump-planner");
    assert_eq!(queued.subject, "plan the slice");
    assert_eq!(queued.body, "body");
    assert_eq!(queued.task.as_deref(), Some(step.task.as_str()));
    assert_eq!(queued.target.as_deref(), Some("default"));
    assert_eq!(queued.from_ref.as_deref(), Some("refs/heads/from"));
    assert_eq!(queued.with, vec!["refs/heads/extra".to_owned()]);
    assert_eq!(queued.extra_targets, vec!["side-repo".to_owned()]);
    assert_eq!(queued.pool_candidates, vec!["alpha".to_owned()]);
    assert_eq!(queued.pool_exclude, vec!["refused".to_owned()]);
    assert!(queued.local_only);
    assert!(queued.read_only);
    assert!(queued.allow_default_branch);
    assert_eq!(queued.publish_expect.as_deref(), Some("expect-sha"));
    assert_eq!(queued.publish_contains.as_deref(), Some("contain-sha"));
    assert_eq!(queued.delete_ref.as_deref(), Some("refs/heads/delete"));
    assert_eq!(queued.delete_expect.as_deref(), Some("delete-sha"));
    assert_eq!(queued.model.as_deref(), Some("model-q4"));
    assert_eq!(queued.command.as_deref(), Some("harness.sh"));
    assert_eq!(queued.builtin_role.as_deref(), Some("tugboat"));
    assert_eq!(queued.units, vec!["unit-1".to_owned()]);
    assert_eq!(queued.source_jobs, vec!["src-1".to_owned()]);
    // An ordinary rerun keeps the old stored budget.
    assert_eq!(queued.budget, 600);
    // Retained callback edits: the repair-style fresh ref wins over the copy.
    assert_eq!(queued.branch.as_deref(), Some("vivi/repair/x-t2"));
    assert_eq!(
        queued.publish_ref.as_deref(),
        Some("refs/heads/vivi/repair/x-t2")
    );
    // The preserved payload keeps unrelated keys and the spent budget-retry
    // marker from the original job.
    assert_eq!(
        queued.payload["plan_file"],
        serde_json::json!("plan/one.md")
    );
    assert_eq!(queued.payload[PAYLOAD_KEY], serde_json::json!(true));
    // The stored row carries the incremented attempt; the stamp matches it.
    let stored = rig.store.step_job("plan-j1-a2")?.unwrap();
    assert_eq!(stored.attempt, row.attempt + 1);
    assert_eq!(queued.payload["attempt"], serde_json::json!(stored.attempt));
    assert_eq!(queued.payload["run"], serde_json::json!(stored.run));
    assert_eq!(queued.payload["node"], serde_json::json!(stored.node));
    assert_eq!(queued.payload["task"], serde_json::json!(stored.task));
    // The replay landed no duplicate: one queued attempt, one stored row.
    assert_eq!(rig.jobs.jobs.len(), 2);
    assert_eq!(rig.store.last_attempt(&step.task)?, stored.attempt);
    // The same task and run are running again.
    assert_eq!(rig.plan_step()?.state, StepState::Running);

    // A true budget-retry marker survives subsequent copies: rerunning the
    // marked attempt copies it onward unchanged — still true — and the
    // ordinary copy raises no budget of its own.
    let marked = rig.jobs.jobs["plan-j1-a2"].clone();
    let marked_row = rig.store.step_job("plan-j1-a2")?.unwrap();
    assert_eq!(marked_row.attempt, 2);
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
    cx.rerun_with(&marked, &marked_row, &step, &edit)?;
    let carried = &rig.jobs.jobs["plan-j1-a2-a3"];
    assert_eq!(carried.payload[PAYLOAD_KEY], serde_json::json!(true));
    assert_eq!(carried.budget, 600);
    assert_eq!(rig.jobs.jobs.len(), 3);
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

/// Queue the broken reply of `plan-j1` and correct it through the real
/// [`RunCx::correct`] enqueue path, returning the queued job's budget.
fn corrected_budget(dispatch: DispatchConfig) -> Result<u64, VivariumError> {
    let mut rig = rig_with(dispatch)?;
    let step = rig.plan_step()?;
    let mut spec = JobSpec::new("plan-j1", "planner", "pump-planner", "s", "b");
    spec.task = Some(step.task.clone());
    spec.from_ref = Some("main".into());
    jobs::enqueue(&mut rig.jobs, spec)?;
    let mut job = rig.jobs.jobs["plan-j1"].clone();
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt: "no envelope here".into(),
        at: 0,
    });
    let row = StepJobRow {
        job: "plan-j1".into(),
        run: 1,
        node: "plan".into(),
        task: step.task.clone(),
        attempt: 1,
    };
    rig.store.insert_step_job(NEED, &row)?;
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
    cx.correct(&job, &row, RoleKind::Planner, &EnvelopeError::NoOpening)?;
    Ok(rig.jobs.jobs["plan-j1-c1"].budget)
}

#[test]
fn envelope_fix_uses_config_and_defaults_to_300() -> R {
    // The explicit `envelope-fix` override wins over a differing shared and
    // planner budget.
    let explicit = DispatchConfig {
        budget: 1_200,
        step_budgets: StepBudgets {
            planner: Some(900),
            envelope_fix: Some(45),
            ..StepBudgets::default()
        },
        ..DispatchConfig::default()
    };
    assert_eq!(corrected_budget(explicit)?, 45);
    // Absent the override, a changed shared dispatch budget does not leak
    // into the correction: it still queues at the 300-second default.
    let changed_budget = DispatchConfig {
        budget: 1_200,
        ..DispatchConfig::default()
    };
    assert_eq!(corrected_budget(changed_budget)?, 300);
    // Absent the override, a changed planner step budget does not leak in
    // either: a correction is not a full-role rerun of the failed role.
    let changed_planner = DispatchConfig {
        step_budgets: StepBudgets {
            planner: Some(900),
            ..StepBudgets::default()
        },
        ..DispatchConfig::default()
    };
    assert_eq!(corrected_budget(changed_planner)?, 300);
    Ok(())
}

#[test]
fn raised_retry_preserves_execution_context() -> R {
    // The stored budget (500) is distinct from the current configuration
    // (333): the raised rerun doubles the stored job's budget, never a
    // reload's or a configuration value.
    let mut rig = rig_with(DispatchConfig {
        budget: 333,
        ..DispatchConfig::default()
    })?;
    let step = rig.plan_step()?;
    let mut spec = JobSpec::new(
        "plan-j1",
        "planner",
        "pump-planner",
        "plan the slice",
        "body",
    );
    spec.task = Some(step.task.clone());
    spec.target = Some("default".into());
    spec.from_ref = Some("refs/heads/from".into());
    spec.with = vec!["refs/heads/extra".into()];
    spec.extra_targets = vec!["side-repo".into()];
    spec.pool_candidates = vec!["alpha".into()];
    spec.pool_exclude = vec!["refused".into()];
    spec.local_only = true;
    spec.read_only = true;
    spec.branch = Some("vivi/repair/x".into());
    spec.allow_default_branch = true;
    spec.publish_ref = Some("refs/heads/vivi/repair/x".into());
    spec.publish_expect = Some("expect-sha".into());
    spec.publish_contains = Some("contain-sha".into());
    spec.delete_ref = Some("refs/heads/delete".into());
    spec.delete_expect = Some("delete-sha".into());
    spec.model = Some("model-q4".into());
    spec.command = Some("harness.sh".into());
    spec.builtin_role = Some("tugboat".into());
    spec.units = vec!["unit-1".into()];
    spec.source_jobs = vec!["src-1".into()];
    spec.budget = 500;
    spec.payload = serde_json::json!({"plan_file": "plan/one.md"});
    jobs::enqueue(&mut rig.jobs, spec)?;
    // Runtime ownership on the killed job must not travel: make the
    // original dirty before the kill collects.
    let owned = rig.jobs.jobs.get_mut("plan-j1").unwrap();
    owned.pool = Some("fast".into());
    owned.dispatched_at = Some(11);
    owned.dispatch_refusals = 2;
    owned.held = Some("refused".into());
    owned.status = JobStatus::Timeout;
    let row = StepJobRow {
        job: "plan-j1".into(),
        run: 1,
        node: "plan".into(),
        task: step.task.clone(),
        attempt: 1,
    };
    rig.store.insert_step_job(NEED, &row)?;
    let job = rig.jobs.jobs["plan-j1"].clone();
    let class = class_of(&rig, "plan")?;
    {
        let mut cx = run_cx(&mut rig);
        RoleCollector.collect(&mut cx, &job, &row, &step, &class)?;
    }

    let queued = &rig.jobs.jobs["plan-j1-a2"];
    assert_eq!(queued.id, "plan-j1-a2");
    // Only the retry identity, the attempt, the raised budget and the
    // spent marker differ: the stored 500 doubled, the configuration's
    // 333 was never consulted.
    assert_eq!(queued.budget, 1_000);
    // Fresh pending runtime state: no pool ownership, timestamps, outcome,
    // applied stamp, refusal count or held reason travels.
    assert_eq!(queued.status, JobStatus::Pending);
    assert_eq!(queued.pool, None);
    assert_eq!(queued.dispatched_at, None);
    assert_eq!(queued.outcome, None);
    assert_eq!(queued.applied_at, None);
    assert_eq!(queued.dispatch_refusals, 0);
    assert_eq!(queued.held, None);
    // Every execution input survives the copy.
    assert_eq!(queued.kind, "planner");
    assert_eq!(queued.role, "pump-planner");
    assert_eq!(queued.subject, "plan the slice");
    assert_eq!(queued.body, "body");
    assert_eq!(queued.task.as_deref(), Some(step.task.as_str()));
    assert_eq!(queued.target.as_deref(), Some("default"));
    assert_eq!(queued.from_ref.as_deref(), Some("refs/heads/from"));
    assert_eq!(queued.with, vec!["refs/heads/extra".to_owned()]);
    assert_eq!(queued.extra_targets, vec!["side-repo".to_owned()]);
    // Routing restrictions survive.
    assert_eq!(queued.pool_candidates, vec!["alpha".to_owned()]);
    assert_eq!(queued.pool_exclude, vec!["refused".to_owned()]);
    assert!(queued.local_only);
    assert!(queued.read_only);
    assert!(queued.allow_default_branch);
    // Publication constraints survive.
    assert_eq!(
        queued.publish_ref.as_deref(),
        Some("refs/heads/vivi/repair/x")
    );
    assert_eq!(queued.publish_expect.as_deref(), Some("expect-sha"));
    assert_eq!(queued.publish_contains.as_deref(), Some("contain-sha"));
    assert_eq!(queued.delete_ref.as_deref(), Some("refs/heads/delete"));
    assert_eq!(queued.delete_expect.as_deref(), Some("delete-sha"));
    // The model, the command and the built-in charter survive.
    assert_eq!(queued.model.as_deref(), Some("model-q4"));
    assert_eq!(queued.command.as_deref(), Some("harness.sh"));
    assert_eq!(queued.builtin_role.as_deref(), Some("tugboat"));
    assert_eq!(queued.branch.as_deref(), Some("vivi/repair/x"));
    assert_eq!(queued.units, vec!["unit-1".to_owned()]);
    assert_eq!(queued.source_jobs, vec!["src-1".to_owned()]);
    // The raised retry is marked durably; unrelated payload keys survive.
    assert_eq!(queued.payload[PAYLOAD_KEY], serde_json::json!(true));
    assert_eq!(
        queued.payload["plan_file"],
        serde_json::json!("plan/one.md")
    );
    // Same task and run, one incremented ordinary attempt.
    let stored = rig.store.step_job("plan-j1-a2")?.unwrap();
    assert_eq!(stored.run, row.run);
    assert_eq!(stored.node, "plan");
    assert_eq!(stored.task, row.task);
    assert_eq!(stored.attempt, row.attempt + 1);
    assert_eq!(queued.payload["task"], serde_json::json!(stored.task));
    assert_eq!(queued.payload["run"], serde_json::json!(stored.run));
    assert_eq!(queued.payload["attempt"], serde_json::json!(stored.attempt));
    assert_eq!(queued.payload["node"], serde_json::json!(stored.node));
    // The step keeps running with no verdict; no correction was queued.
    assert_eq!(rig.plan_step()?.state, StepState::Running);
    assert_eq!(rig.plan_step()?.verdict, None);
    assert!(!rig.jobs.jobs.contains_key("plan-j1-c1"));
    // Repeating the collection on the still-killed original queues no
    // duplicate: the raised attempt is already there.
    {
        let job = rig.jobs.jobs["plan-j1"].clone();
        let mut cx = run_cx(&mut rig);
        RoleCollector.collect(&mut cx, &job, &row, &step, &class)?;
    }
    assert_eq!(rig.jobs.jobs.len(), 2);

    // The auditor's routing restriction survives its raised rerun too.
    let audit_step = step_of(&mut rig, "audit1")?;
    let mut audit_spec = JobSpec::new("aud-j1", "auditor", "pump-auditor", "s", "b");
    audit_spec.task = Some(audit_step.task.clone());
    audit_spec.with = vec!["refs/heads/audit-ref".into()];
    audit_spec.local_only = true;
    audit_spec.budget = 500;
    jobs::enqueue(&mut rig.jobs, audit_spec)?;
    rig.jobs.jobs.get_mut("aud-j1").unwrap().status = JobStatus::Timeout;
    let audit_row = StepJobRow {
        job: "aud-j1".into(),
        run: 1,
        node: "audit1".into(),
        task: audit_step.task.clone(),
        attempt: 1,
    };
    rig.store.insert_step_job(NEED, &audit_row)?;
    let audit_job = rig.jobs.jobs["aud-j1"].clone();
    let audit_class = class_of(&rig, "audit1")?;
    {
        let mut cx = run_cx(&mut rig);
        RoleCollector.collect(&mut cx, &audit_job, &audit_row, &audit_step, &audit_class)?;
    }
    assert_eq!(
        rig.jobs.jobs["aud-j1-a2"].with,
        vec!["refs/heads/audit-ref".to_owned()]
    );
    assert!(rig.jobs.jobs["aud-j1-a2"].local_only);
    assert_eq!(rig.jobs.jobs["aud-j1-a2"].budget, 1_000);

    // A literal replay of the same raised requests queues no duplicate
    // work: one raised attempt per killed job, nothing more.
    {
        let job = rig.jobs.jobs["plan-j1"].clone();
        let audit_job = rig.jobs.jobs["aud-j1"].clone();
        let mut cx = run_cx(&mut rig);
        RoleCollector.collect(&mut cx, &job, &row, &step, &class)?;
        RoleCollector.collect(&mut cx, &audit_job, &audit_row, &audit_step, &audit_class)?;
    }
    assert_eq!(rig.jobs.jobs.len(), 4);
    assert_eq!(rig.store.step_job("plan-j1-a2")?.unwrap().attempt, 2);
    assert_eq!(rig.store.step_job("aud-j1-a2")?.unwrap().attempt, 2);
    Ok(())
}

#[test]
fn lost_job_retry_keeps_its_budget() -> R {
    let mut rig = rig()?;
    let step = rig.plan_step()?;
    let mut spec = JobSpec::new("plan-j1", "planner", "pump-planner", "s", "b");
    spec.task = Some(step.task.clone());
    spec.budget = 600;
    spec.payload = serde_json::json!({"plan_file": "plan/one.md"});
    jobs::enqueue(&mut rig.jobs, spec)?;
    // The pool lost the job: the terminal state only, no agent exit and no
    // host evidence, so no environment failure and no budget kill applies.
    let lost = rig.jobs.jobs.get_mut("plan-j1").unwrap();
    lost.status = JobStatus::Lost;
    lost.outcome = Some(JobOutcome {
        state: "lost".to_owned(),
        detail: "worker vanished".to_owned(),
        receipt: String::new(),
        at: 0,
    });
    let job = rig.jobs.jobs["plan-j1"].clone();
    let raised = raised_budget(job.budget).expect("600 doubles inside the pool range");
    assert_ne!(job.budget, raised);
    // The RoleCollector retries the lost job as the next ordinary attempt.
    let collected = collect_role(&mut rig, "plan-j1", "plan")?;
    assert_eq!(collected.state, StepState::Running);
    // No budget change and no raised marker: a lost job is not a budget kill.
    let queued = &rig.jobs.jobs["plan-j1-a2"];
    assert_eq!(queued.budget, job.budget);
    assert_eq!(queued.payload.get(PAYLOAD_KEY), None);
    // The unrelated payload key survives; only the stamp's own keys join it.
    assert_eq!(
        queued.payload["plan_file"],
        serde_json::json!("plan/one.md")
    );
    assert_eq!(queued.task.as_deref(), Some(step.task.as_str()));
    let stored = rig.store.step_job("plan-j1-a2")?.unwrap();
    assert_eq!(stored.attempt, 2);
    assert_eq!(queued.payload["attempt"], serde_json::json!(stored.attempt));
    // No correction was queued alongside the retry.
    assert_eq!(rig.jobs.jobs.len(), 2);
    assert_eq!(rig.plan_step()?.state, StepState::Running);
    Ok(())
}

/// The pool summary head of a job the runner killed at its time budget.
const KILL_SUMMARY: &str = "unit a1\nagent exit 124 after 600s\npool: local";

/// A valid planner envelope: without the kill the finished summary would
/// parse it and answer `planned`.
const PLANNED: &str = "+++\nanswer = \"planned\"\n+++\nThe plan.";

#[test]
fn a_first_budget_kill_queues_one_raised_rerun() -> R {
    let mut rig = rig()?;
    let step = rig.plan_step()?;
    let mut spec = JobSpec::new("plan-j1", "planner", "pump-planner", "s", "b");
    spec.task = Some(step.task.clone());
    spec.budget = 600;
    jobs::enqueue(&mut rig.jobs, spec)?;
    let killed = rig.jobs.jobs.get_mut("plan-j1").unwrap();
    killed.status = JobStatus::Timeout;
    let collected = collect_role(&mut rig, "plan-j1", "plan")?;
    // The same task and run stay running: the step is not closed, not
    // routed to the cto and not corrected — the kill queued the raised
    // rerun instead.
    assert_eq!(collected.state, StepState::Running);
    assert_eq!(collected.verdict, None);
    let queued = &rig.jobs.jobs["plan-j1-a2"];
    assert_eq!(queued.budget, 1_200);
    assert_eq!(queued.payload[PAYLOAD_KEY], serde_json::json!(true));
    assert_eq!(queued.task.as_deref(), Some(step.task.as_str()));
    let stored = rig.store.step_job("plan-j1-a2")?.unwrap();
    assert_eq!(stored.attempt, 2);
    // Exactly one raised job and no correction; no alarm was raised.
    assert_eq!(rig.jobs.jobs.len(), 2);
    assert!(!rig.jobs.jobs.contains_key("plan-j1-c1"));
    assert!(rig.exceptions().is_empty());
    Ok(())
}

#[test]
fn correction_budget_kill_does_not_rerun_the_role() -> R {
    // A killed envelope-fix correction answers `failed` for both kill
    // representations: never a full-role raised rerun and never another
    // correction, not even when a valid envelope rides the receipt.
    for (name, status, receipt) in [
        ("timeout status", JobStatus::Timeout, String::new()),
        (
            "finished summary exit 124",
            JobStatus::Finished,
            format!("{KILL_SUMMARY}\n\n--- receipt\n{PLANNED}"),
        ),
    ] {
        let mut rig = rig()?;
        let step = rig.plan_step()?;
        let mut spec = JobSpec::new("plan-j1-c1", correct::KIND, "planner", "s", "b");
        spec.task = Some(step.task.clone());
        jobs::enqueue(&mut rig.jobs, spec)?;
        let killed = rig.jobs.jobs.get_mut("plan-j1-c1").unwrap();
        killed.status = status;
        if !receipt.is_empty() {
            killed.outcome = Some(JobOutcome {
                state: "finished".into(),
                detail: String::new(),
                receipt,
                at: 0,
            });
        }
        let failed = collect_role(&mut rig, "plan-j1-c1", "plan")?;
        assert_eq!(failed.verdict.as_deref(), Some("failed"), "{name}");
        // The existing failed route took over: a fresh cto step is pending,
        // and no raised rerun or second correction was queued.
        let cto = rig
            .store
            .steps_of(NEED, 1)?
            .into_iter()
            .find(|row| row.node == "cto")
            .ok_or_else(|| VivariumError::Message("no cto step".into()))?;
        assert_eq!(cto.state, StepState::Pending, "{name}");
        assert_eq!(rig.jobs.jobs.len(), 1, "{name}");
        assert!(!rig.jobs.jobs.contains_key("plan-j1-a2"), "{name}");
        assert!(!rig.jobs.jobs.contains_key("plan-j1-c2"), "{name}");
        assert!(rig.exceptions().is_empty(), "{name}");
    }
    Ok(())
}

/// A budget kill's two pool representations.
#[derive(Debug)]
enum Kill {
    /// The pool's terminal timeout status.
    Timeout,
    /// A finished job whose bounded pool summary reports agent exit 124.
    /// The receipt also carries a valid partial envelope, so a skipped
    /// budget-kill classification would parse it and advance the step
    /// instead of retrying.
    Finished,
}

/// Every thinking class of the build flow: the node to collect, its role
/// as the job names it, and a valid partial envelope for the class (it
/// parses cleanly, so only the kill decides the collection).
const THINKING: &[(&str, &str, &str)] = &[
    ("research", "researcher", "answer = \"READY\""),
    ("plan", "planner", "answer = \"planned\""),
    ("admit1", "admitter", "answer = \"admitted\""),
    (
        "slice",
        "slicer",
        "answer = \"sliced\"\n[[slice]]\nid = \"s1\"\ntitle = \"t\"\nbrief = \"b\"\ntarget = \"default\"",
    ),
    ("audit1", "auditor", "answer = \"clean_pass\""),
    ("judge1", "judge", "answer = \"block\"\nreason = \"r\""),
    ("cto", "cto", "answer = \"lock\"\nreason = \"r\""),
];

/// The decision callback of these tests: it refuses, so a stray routing
/// decision fails the test instead of passing silently.
fn refuse_decision(_: &FlowNode, _: &[JoinInput]) -> Result<String, VivariumError> {
    Err(VivariumError::Message("no decision".into()))
}

/// A fresh [`RunCx`] over `rig`'s run; the decision callback refuses, so a
/// stray routing decision fails the test instead of passing silently.
fn run_cx(rig: &mut Rig) -> RunCx<'_, Jobs, FakeMail> {
    RunCx {
        ctx: &rig.ctx,
        st: &mut rig.jobs,
        store: &rig.store,
        mail: &mut rig.mail,
        registry: &rig.registry,
        decide: &refuse_decision,
        def: rig.def.clone(),
        run: rig
            .store
            .runs_of(NEED)
            .expect("the rig stores its run")
            .remove(0),
    }
}

/// The build flow's class of `node`.
fn class_of(rig: &Rig, node: &str) -> Result<NodeClass, VivariumError> {
    rig.def
        .nodes
        .iter()
        .find(|n| n.id == node)
        .map(|n| n.class.clone())
        .ok_or_else(|| VivariumError::Message(format!("no {node} node in the build flow")))
}

/// Queue `id` as the first attempt of `node` at `budget`, mark it killed
/// per `kill`, and collect it through the real [`RoleCollector`] path.
/// Returns the collected step; everything else is read off `rig`.
fn collect_killed(
    rig: &mut Rig,
    id: &str,
    node: &str,
    budget: u64,
    kill: Kill,
) -> Result<StepRow, VivariumError> {
    let step = step_of(rig, node)?;
    let role = THINKING
        .iter()
        .find(|(n, _, _)| *n == node)
        .map(|(_, role, _)| *role)
        .ok_or_else(|| VivariumError::Message(format!("{node} is not a thinking node")))?;
    let mut spec = JobSpec::new(id, role, role, "s", "b");
    spec.task = Some(step.task.clone());
    spec.from_ref = Some("main".into());
    spec.budget = budget;
    jobs::enqueue(&mut rig.jobs, spec)?;
    let job = rig.jobs.jobs.get_mut(id).unwrap();
    match kill {
        Kill::Timeout => job.status = JobStatus::Timeout,
        Kill::Finished => {
            job.status = JobStatus::Finished;
            let envelope = THINKING
                .iter()
                .find(|(n, _, _)| *n == node)
                .map(|(_, _, envelope)| *envelope)
                .unwrap_or_default();
            job.outcome = Some(JobOutcome {
                state: "finished".into(),
                detail: String::new(),
                receipt: format!("{KILL_SUMMARY}\n\n--- receipt\n{envelope}"),
                at: 0,
            });
        }
    }
    let row = StepJobRow {
        job: id.into(),
        run: 1,
        node: node.into(),
        task: step.task.clone(),
        attempt: 1,
    };
    rig.store.insert_step_job(NEED, &row)?;
    let job = rig.jobs.jobs[id].clone();
    let class = class_of(rig, node)?;
    let mut cx = run_cx(rig);
    RoleCollector.collect(&mut cx, &job, &row, &step, &class)?;
    rig.store
        .steps_of(NEED, 1)?
        .into_iter()
        .find(|s| s.node == node)
        .ok_or_else(|| VivariumError::Message("step gone".into()))
}

#[test]
fn first_budget_kill_retries_once_at_raised_budget() -> R {
    // Every thinking class, in both kill representations: the stored job
    // budget (500) is distinct from the current configuration (333), so
    // the raised rerun can only be the stored budget's double.
    for &(node, _, _) in THINKING {
        for kill in [Kill::Timeout, Kill::Finished] {
            let name = format!("{node} {kill:?}");
            let mut rig = rig_with(DispatchConfig {
                budget: 333,
                ..DispatchConfig::default()
            })?;
            let step = collect_killed(&mut rig, "j1", node, 500, kill)?;
            // The step is neither answered nor routed: it keeps running
            // with no verdict, no cto route, no new run and no exception.
            assert_eq!(step.state, StepState::Running, "{name}");
            assert_eq!(step.verdict, None, "{name}");
            let mut expected: Vec<String> = vec!["plan".to_owned(), "sentinel".to_owned()];
            if node != "plan" {
                expected.push(node.to_owned());
            }
            expected.sort();
            let mut nodes: Vec<String> = rig
                .store
                .steps_of(NEED, 1)?
                .into_iter()
                .map(|s| s.node)
                .collect();
            nodes.sort();
            assert_eq!(nodes, expected, "{name}");
            assert_eq!(rig.store.runs_of(NEED)?.len(), 1, "{name}");
            assert!(rig.exceptions().is_empty(), "{name}");
            // Exactly one pending rerun, raised to the stored budget's
            // double (1000, not the configuration's 333 or 600).
            assert_eq!(rig.jobs.jobs.len(), 2, "{name}");
            let queued = &rig.jobs.jobs["j1-a2"];
            assert_eq!(queued.status, JobStatus::Pending, "{name}");
            assert_eq!(queued.budget, 1_000, "{name}");
            // The rerun is durably marked as the spent budget retry.
            assert_eq!(
                queued.payload[PAYLOAD_KEY],
                serde_json::json!(true),
                "{name}"
            );
            assert_eq!(queued.task.as_deref(), Some(step.task.as_str()), "{name}");
            // Next-row accounting: the same task and run at the next
            // attempt, stamped into the payload.
            let stored = rig.store.step_job("j1-a2")?.unwrap();
            assert_eq!(stored.run, 1, "{name}");
            assert_eq!(stored.node, node, "{name}");
            assert_eq!(stored.task, step.task, "{name}");
            assert_eq!(stored.attempt, 2, "{name}");
            assert_eq!(
                queued.payload["attempt"],
                serde_json::json!(stored.attempt),
                "{name}"
            );
            assert_eq!(
                queued.payload["run"],
                serde_json::json!(stored.run),
                "{name}"
            );
            assert_eq!(
                queued.payload["task"],
                serde_json::json!(stored.task),
                "{name}"
            );
            assert_eq!(
                queued.payload["node"],
                serde_json::json!(stored.node),
                "{name}"
            );
            // No correction and no second automatic execution.
            assert!(!rig.jobs.jobs.contains_key("j1-c1"), "{name}");
            assert!(!rig.jobs.jobs.contains_key("j1-a3"), "{name}");
            // Repeating the collection is idempotent: the raised rerun is
            // already queued, so the repeat queues no duplicate and leaves
            // the stored attempt alone.
            {
                let job = rig.jobs.jobs["j1"].clone();
                let row = rig.store.step_job("j1")?.unwrap();
                let class = class_of(&rig, node)?;
                let mut cx = run_cx(&mut rig);
                RoleCollector.collect(&mut cx, &job, &row, &step, &class)?;
            }
            assert_eq!(rig.jobs.jobs.len(), 2, "{name}");
            assert_eq!(rig.store.step_job("j1-a2")?.unwrap().attempt, 2, "{name}");
            let after = rig
                .store
                .steps_of(NEED, 1)?
                .into_iter()
                .find(|s| s.node == node)
                .ok_or_else(|| VivariumError::Message("step gone".into()))?;
            assert_eq!(after.state, StepState::Running, "{name}");
            assert_eq!(after.verdict, None, "{name}");
            // Settled like the tick settles a collected job: the killed
            // original is applied, the raised rerun is not.
            assert!(jobs::mark_applied(&mut rig.jobs, "j1", 7), "{name}");
            assert_eq!(rig.jobs.jobs["j1"].applied_at, Some(7), "{name}");
            assert_eq!(rig.jobs.jobs["j1-a2"].applied_at, None, "{name}");
        }
    }
    // The cap: a stored budget whose double passes the pool ceiling
    // raises once, to the ceiling itself, never to an invalid budget.
    let mut rig = rig_with(DispatchConfig {
        budget: 333,
        ..DispatchConfig::default()
    })?;
    let step = collect_killed(&mut rig, "j1", "plan", 30_000, Kill::Timeout)?;
    assert_eq!(step.state, StepState::Running);
    let queued = &rig.jobs.jobs["j1-a2"];
    assert_eq!(queued.budget, 43_200);
    assert_eq!(queued.budget, raised_budget(30_000).unwrap());
    assert_eq!(queued.payload[PAYLOAD_KEY], serde_json::json!(true));
    Ok(())
}

#[test]
fn second_budget_kill_routes_failed() -> R {
    let mut rig = rig_with(DispatchConfig {
        budget: 333,
        ..DispatchConfig::default()
    })?;
    let step = rig.plan_step()?;
    // First kill: the planner job is raised once, at its own double.
    let mut spec = JobSpec::new("plan-j1", "planner", "pump-planner", "s", "b");
    spec.task = Some(step.task.clone());
    spec.budget = 500;
    jobs::enqueue(&mut rig.jobs, spec)?;
    rig.jobs.jobs.get_mut("plan-j1").unwrap().status = JobStatus::Timeout;
    let row = StepJobRow {
        job: "plan-j1".into(),
        run: 1,
        node: "plan".into(),
        task: step.task.clone(),
        attempt: 1,
    };
    rig.store.insert_step_job(NEED, &row)?;
    let job = rig.jobs.jobs["plan-j1"].clone();
    let class = class_of(&rig, "plan")?;
    {
        let mut cx = run_cx(&mut rig);
        RoleCollector.collect(&mut cx, &job, &row, &step, &class)?;
    }
    assert_eq!(rig.jobs.jobs["plan-j1-a2"].budget, 1_000);
    assert_eq!(
        rig.jobs.jobs["plan-j1-a2"].payload[PAYLOAD_KEY],
        serde_json::json!(true)
    );
    // The tick settles the collected original before the rerun reports.
    assert!(jobs::mark_applied(&mut rig.jobs, "plan-j1", 7));
    // Second kill, in the other representation: the raised job finishes
    // with agent exit 124 in its summary and a valid envelope in the
    // receipt — only the spent marker makes this a failure.
    let raised = rig.jobs.jobs.get_mut("plan-j1-a2").unwrap();
    raised.status = JobStatus::Finished;
    raised.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt: format!("{KILL_SUMMARY}\n\n--- receipt\n{PLANNED}"),
        at: 0,
    });
    let raised_row = rig.store.step_job("plan-j1-a2")?.unwrap();
    let raised_job = rig.jobs.jobs["plan-j1-a2"].clone();
    {
        let mut cx = run_cx(&mut rig);
        RoleCollector.collect(&mut cx, &raised_job, &raised_row, &step, &class)?;
    }
    // The existing failed transition took over: the shipped build flow
    // routes a planner `failed` to the cto gate, pending a fresh task.
    let failed = rig
        .store
        .steps_of(NEED, 1)?
        .into_iter()
        .find(|s| s.node == "plan")
        .ok_or_else(|| VivariumError::Message("plan step gone".into()))?;
    assert_eq!(failed.verdict.as_deref(), Some("failed"));
    let cto = rig
        .store
        .steps_of(NEED, 1)?
        .into_iter()
        .find(|row| row.node == "cto")
        .ok_or_else(|| VivariumError::Message("no cto step".into()))?;
    assert_eq!(cto.state, StepState::Pending);
    // No third automatic execution and no envelope correction.
    assert_eq!(rig.jobs.jobs.len(), 2);
    assert!(!rig.jobs.jobs.contains_key("plan-j1-a3"));
    assert!(!rig.jobs.jobs.contains_key("plan-j1-c1"));
    assert!(rig.exceptions().is_empty());
    Ok(())
}
