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
use crate::pump::flow::advance::start::start;
use crate::pump::flow::collect_step::{RunCx, StepCollector};
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::step_error::STEP_ERROR_HELD;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{self, JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

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
