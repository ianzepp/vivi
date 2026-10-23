//! Tests for the per-step error boundary of the tick: a unit that fails
//! three ticks in a row is held, and the rest of its run keeps ticking.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use tempfile::{TempDir, tempdir};

use super::{OPERATOR_HOLD, STEP_OVERDUE, tick_with};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::envelope::roles::Slice;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::collect_step::{RunCx, StepCollector};
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::fanout::{self, VERDICT_NO_DEFECT, slice_node};
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::need_branch;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::scope::ScopeState;
use crate::pump::flow::step_error::STEP_ERROR_HELD;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{self, JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};

type R = Result<(), VivariumError>;
type Reg = Registry<Jobs, FakeMail>;

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
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, "kind: build")?;
    start(&ctx, &store, &mut mail, NEED, Some("plan"))?;
    Ok(Rig {
        _dir: dir,
        ctx,
        host,
        store,
        mail,
        jobs: Jobs::default(),
    })
}

impl Rig {
    fn plan(&self) -> Result<StepRow, VivariumError> {
        self.store
            .steps_of(NEED, 1)?
            .into_iter()
            .find(|row| row.node == "plan")
            .ok_or_else(|| VivariumError::Message("no plan step".into()))
    }

    fn tick(&mut self, registry: &Reg) -> R {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.mail,
            registry,
        )
    }

    fn exceptions(&self) -> String {
        fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }

    fn count(&self, kind: &str) -> usize {
        self.exceptions().matches(kind).count()
    }

    /// A job of the plan step, with `status`, bound to it by a step job row.
    fn plan_job(&mut self, id: &str, status: JobStatus) -> R {
        let task = self.plan()?.task;
        let mut spec = JobSpec::new(id, "planner", "pump-planner", "s", "b");
        spec.task = Some(task.clone());
        jobs::enqueue(&mut self.jobs, spec)?;
        self.store.insert_step_job(
            NEED,
            &StepJobRow {
                job: id.into(),
                run: 1,
                node: "plan".into(),
                task: task.clone(),
                attempt: 1,
            },
        )?;
        self.store.set_step_state(&task, StepState::Running, None)?;
        let job = self
            .jobs
            .jobs
            .get_mut(id)
            .ok_or_else(|| VivariumError::Message(format!("no job {id}")))?;
        job.status = status;
        Ok(())
    }
}

/// A start handler that fails its first `failures` calls, then waits.
struct Starting {
    calls: AtomicU32,
    failures: u32,
}

impl Starting {
    fn failing(failures: u32) -> Box<Self> {
        Box::new(Self {
            calls: AtomicU32::new(0),
            failures,
        })
    }
}

impl StepHandler for Starting {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.failures {
            return Err(VivariumError::Message("handler broke".into()));
        }
        Ok(StartAction::OpenAndWait)
    }
}

/// A collector that always fails.
struct BrokenCollector;

impl StepCollector<Jobs, FakeMail> for BrokenCollector {
    fn collect(
        &self,
        _cx: &mut RunCx<'_, Jobs, FakeMail>,
        _job: &PoolJob,
        _row: &StepJobRow,
        _step: &StepRow,
        _class: &NodeClass,
    ) -> Result<(), VivariumError> {
        Err(VivariumError::Message("collector broke".into()))
    }
}

fn registry_with(handler: Box<dyn StepHandler>) -> Reg {
    let mut registry = Reg::new();
    registry.handler(FlowKind::Build, NodeClass::Planner, handler);
    registry
}

#[test]
fn a_start_that_fails_three_ticks_running_holds_the_step_with_one_alarm() -> R {
    let mut rig = rig()?;
    let registry = registry_with(Starting::failing(u32::MAX));
    for _ in 0..2 {
        rig.tick(&registry)?;
        assert_eq!(rig.plan()?.state, StepState::Pending);
        assert!(rig.count("flow-run-error") >= 1);
        assert_eq!(rig.count(STEP_ERROR_HELD), 0);
    }
    rig.tick(&registry)?;
    let plan = rig.plan()?;
    assert_eq!(plan.state, StepState::Waiting);
    assert_eq!(plan.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(rig.count(STEP_ERROR_HELD), 1);
    let text = rig.exceptions();
    assert!(
        text.contains("step plan") && text.contains("handler broke") && text.contains("start"),
        "{text}"
    );
    rig.tick(&registry)?;
    assert_eq!(rig.plan()?.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(rig.count(STEP_ERROR_HELD), 1);
    Ok(())
}

#[test]
fn a_handler_that_recovers_before_the_limit_is_never_held() -> R {
    let mut rig = rig()?;
    let registry = registry_with(Starting::failing(2));
    for _ in 0..3 {
        rig.tick(&registry)?;
    }
    let plan = rig.plan()?;
    assert_eq!(plan.state, StepState::Waiting);
    assert_eq!(plan.verdict, None);
    assert_eq!(rig.count(STEP_ERROR_HELD), 0);
    Ok(())
}

#[test]
fn a_failing_step_does_not_skip_the_overdue_check_of_its_run() -> R {
    let mut rig = rig()?;
    rig.plan_job("plan-j1", JobStatus::Dispatched)?;
    rig.store
        .set_step_state(&rig.plan()?.task, StepState::Pending, None)?;
    let job = rig
        .jobs
        .jobs
        .get_mut("plan-j1")
        .ok_or_else(|| VivariumError::Message("no job".into()))?;
    job.dispatched_at = Some(1_000);
    job.budget = 100;
    rig.host.set_now(1_000 + 100 + 601);
    let registry = registry_with(Starting::failing(u32::MAX));
    rig.tick(&registry)?;
    assert_eq!(rig.plan()?.state, StepState::Pending, "the start failed");
    assert!(rig.count("flow-run-error") >= 1);
    assert_eq!(rig.count(STEP_OVERDUE), 1);
    Ok(())
}

#[test]
fn a_collector_that_fails_three_ticks_running_holds_the_step_and_applies_the_job() -> R {
    let mut rig = rig()?;
    rig.plan_job("plan-j1", JobStatus::Finished)?;
    if let Some(job) = rig.jobs.jobs.get_mut("plan-j1") {
        job.outcome = Some(JobOutcome {
            state: "finished".into(),
            detail: String::new(),
            receipt: String::new(),
            at: 0,
        });
    }
    let mut registry = Reg::new();
    registry.collector(
        FlowKind::Build,
        NodeClass::Planner,
        Box::new(BrokenCollector),
    );
    for _ in 0..2 {
        rig.tick(&registry)?;
        assert!(rig.jobs.jobs["plan-j1"].applied_at.is_none());
        assert_eq!(rig.plan()?.state, StepState::Running);
    }
    rig.tick(&registry)?;
    assert!(rig.jobs.jobs["plan-j1"].applied_at.is_some());
    let plan = rig.plan()?;
    assert_eq!(plan.state, StepState::Waiting);
    assert_eq!(plan.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(rig.count(STEP_ERROR_HELD), 1);
    assert!(rig.exceptions().contains("collector broke"));
    rig.tick(&registry)?;
    assert_eq!(rig.count(STEP_ERROR_HELD), 1);
    Ok(())
}

const BASE: &str = "0123456789abcdef0123456789abcdef01234567";

impl Rig {
    /// Add a step row `node` with a fresh task in `state`.
    fn add_step(&mut self, node: &str, state: StepState) -> Result<StepRow, VivariumError> {
        let body = format!("pump-step: {NEED} r1 {node}");
        let task = self.mail.send_step("mind", "mind", node, &body, &[])?;
        let row = StepRow {
            run: 1,
            node: node.into(),
            task,
            state,
            verdict: None,
        };
        self.store.insert_step(NEED, &row)?;
        Ok(row)
    }

    fn step_of(&self, node: &str) -> Result<StepRow, VivariumError> {
        self.store
            .steps_of(NEED, 1)?
            .into_iter()
            .find(|row| row.node == node)
            .ok_or_else(|| VivariumError::Message(format!("no step {node}")))
    }

    /// The plan step answered and one fanout (`build1`) open.
    fn with_open_fanout(&mut self) -> Result<StepRow, VivariumError> {
        let plan = self.plan()?;
        self.store
            .set_step_state(&plan.task, StepState::Done, Some("planned"))?;
        let slicer = self.add_step("slice", StepState::Done)?;
        let build1 = self.add_step("build1", StepState::Running)?;
        self.store.set_open_fanout(NEED, 1, &build1.task)?;
        let run = self
            .store
            .live_runs()?
            .into_iter()
            .next()
            .ok_or_else(|| VivariumError::Message("no live run".into()))?;
        let slices = ["a", "b"].map(|id| Slice {
            id: id.into(),
            title: format!("title {id}"),
            brief: format!("brief {id}"),
            target: "default".into(),
            write_scope: vec![format!("src/{id}.rs")],
            deps: Vec::new(),
        });
        fanout::expand(
            &self.ctx,
            &self.store,
            &mut self.mail,
            &run,
            &slicer,
            &slices,
        )?;
        for id in ["a", "b"] {
            let task = self.step_of(&slice_node(id))?.task;
            fanout::close_slice(&self.store, &mut self.mail, &task, VERDICT_NO_DEFECT)?;
        }
        Ok(build1)
    }

    /// A live need branch whose tip read fails every time.
    fn broken_need_tip(&self) -> R {
        self.store.upsert_need_branch(&NeedBranchRow {
            need: NEED.into(),
            branch: need_branch::branch(NEED),
            base: Some(BASE.into()),
            status: need_branch::LIVE.into(),
            run: 1,
            target: "default".into(),
            priors: Vec::new(),
        })?;
        self.host.reply("ls-remote", 128, "fatal: unreachable");
        Ok(())
    }
}

#[test]
fn a_fanout_whose_payload_read_fails_is_held_after_three_ticks() -> R {
    let mut rig = rig()?;
    let build1 = rig.with_open_fanout()?;
    rig.broken_need_tip()?;
    let registry = Reg::new();
    for _ in 0..2 {
        rig.tick(&registry)?;
        assert_eq!(rig.step_of("build1")?.state, StepState::Running);
        assert_eq!(rig.count(STEP_ERROR_HELD), 0);
    }
    rig.tick(&registry)?;
    let held = rig.step_of("build1")?;
    assert_eq!(held.task, build1.task);
    assert_eq!(held.state, StepState::Waiting);
    assert_eq!(held.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(rig.count(STEP_ERROR_HELD), 1);
    let text = rig.exceptions();
    assert!(
        text.contains("settle") && text.contains("ls-remote"),
        "{text}"
    );
    rig.tick(&registry)?;
    assert_eq!(rig.count(STEP_ERROR_HELD), 1);
    Ok(())
}

#[test]
fn a_failing_fanout_payload_does_not_skip_the_overdue_check() -> R {
    let mut rig = rig()?;
    rig.with_open_fanout()?;
    rig.broken_need_tip()?;
    rig.plan_job("plan-j1", JobStatus::Dispatched)?;
    let job = rig
        .jobs
        .jobs
        .get_mut("plan-j1")
        .ok_or_else(|| VivariumError::Message("no job".into()))?;
    job.dispatched_at = Some(1_000);
    job.budget = 100;
    rig.host.set_now(1_000 + 100 + 601);
    rig.tick(&Reg::new())?;
    assert_eq!(rig.step_of("build1")?.state, StepState::Running);
    assert!(rig.count("flow-run-error") >= 1);
    assert_eq!(rig.count(STEP_OVERDUE), 1);
    Ok(())
}

#[test]
fn a_scope_wait_slice_whose_holder_read_errors_is_held_after_three_ticks() -> R {
    let mut rig = rig()?;
    let plan = rig.plan()?;
    rig.store
        .set_step_state(&plan.task, StepState::Done, Some("planned"))?;
    let slice = rig.add_step(&slice_node("a"), StepState::Running)?;
    rig.store
        .insert_slice_scope(&slice.task, 1, &["src/a.rs".to_string()])?;
    rig.store.set_scope_state(&slice.task, ScopeState::Wait)?;
    rig.store
        .set_scope_holders(&slice.task, &["no-such-holder".to_string()])?;
    let registry = Reg::new();
    for _ in 0..2 {
        rig.tick(&registry)?;
        assert_eq!(rig.step_of(&slice_node("a"))?.state, StepState::Running);
        assert_eq!(rig.count(STEP_ERROR_HELD), 0);
    }
    rig.tick(&registry)?;
    let held = rig.step_of(&slice_node("a"))?;
    assert_eq!(held.state, StepState::Waiting);
    assert_eq!(held.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(rig.count(STEP_ERROR_HELD), 1);
    assert!(rig.exceptions().contains("release"));
    Ok(())
}
