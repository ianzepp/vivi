//! Tests for the tick's alarms on work no live run looks after: an overdue
//! job of a run that is no longer live, and a running fanout step with no
//! open fanout row.

use std::collections::BTreeMap;
use std::fs;

use tempfile::{TempDir, tempdir};

use super::{FANOUT_UNBOUND, STEP_OVERDUE, tick_with};
use crate::error::VivariumError;
use crate::letter::fixture::step_body;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{self, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;
type Reg = Registry<Jobs, FakeMail>;

const NEED: &str = "n1";
const JOB: &str = "plan-j1";

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
    host: std::sync::Arc<FakeHost>,
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
    fn tick(&mut self) -> R {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.mail,
            &Reg::new(),
        )
    }

    fn exceptions(&self) -> String {
        fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }

    fn count(&self, kind: &str) -> usize {
        self.exceptions().matches(kind).count()
    }

    fn plan(&self) -> Result<StepRow, VivariumError> {
        self.store
            .steps_of(NEED, 1)?
            .into_iter()
            .find(|row| row.node == "plan")
            .ok_or_else(|| VivariumError::Message("no plan step".into()))
    }

    /// A dispatched job of the plan step, one second past its grace at
    /// `now`.
    fn late_plan_job(&mut self) -> R {
        let task = self.plan()?.task;
        let mut spec = JobSpec::new(JOB, "planner", "pump-planner", "s", "b");
        spec.task = Some(task.clone());
        jobs::enqueue(&mut self.jobs, spec)?;
        self.store.insert_step_job(
            NEED,
            &StepJobRow {
                job: JOB.into(),
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
            .get_mut(JOB)
            .ok_or_else(|| VivariumError::Message("no job".into()))?;
        job.status = JobStatus::Dispatched;
        job.dispatched_at = Some(1_000);
        job.budget = 100;
        job.pool = Some("pool-x".into());
        self.host.set_now(1_000 + 100 + 601);
        Ok(())
    }

    /// The plan step answered and a running fanout step `build1`.
    fn running_build1(&mut self) -> Result<StepRow, VivariumError> {
        let plan = self.plan()?;
        self.store
            .set_step_state(&plan.task, StepState::Done, Some("planned"))?;
        let body = step_body(NEED, 1, "build1", "");
        let task = self.mail.send_step("mind", "mind", "build1", &body, &[])?;
        let row = StepRow {
            run: 1,
            node: "build1".into(),
            task,
            state: StepState::Running,
            verdict: None,
        };
        self.store.insert_step(NEED, &row)?;
        Ok(row)
    }
}

#[test]
fn an_overdue_job_of_a_superseded_run_raises_once_naming_its_run_state() -> R {
    let mut rig = rig()?;
    rig.late_plan_job()?;
    rig.store.set_run_state(NEED, 1, "superseded")?;
    rig.tick()?;
    rig.tick()?;
    assert_eq!(rig.count(STEP_OVERDUE), 1);
    let text = rig.exceptions();
    assert!(
        text.contains(JOB)
            && text.contains("plan")
            && text.contains("r1")
            && text.contains("superseded")
            && text.contains("pool-x"),
        "{text}"
    );
    Ok(())
}

#[test]
fn an_overdue_job_within_its_grace_of_a_dead_run_raises_nothing() -> R {
    let mut rig = rig()?;
    rig.late_plan_job()?;
    rig.store.set_run_state(NEED, 1, "withdrawn")?;
    rig.host.set_now(1_000 + 100 + 600);
    rig.tick()?;
    assert_eq!(rig.count(STEP_OVERDUE), 0);
    Ok(())
}

#[test]
fn an_overdue_job_of_a_live_run_still_raises_once() -> R {
    let mut rig = rig()?;
    rig.late_plan_job()?;
    rig.tick()?;
    rig.tick()?;
    assert_eq!(rig.count(STEP_OVERDUE), 1);
    let text = rig.exceptions();
    assert!(text.contains(JOB) && !text.contains("not live"), "{text}");
    Ok(())
}

#[test]
fn a_running_fanout_step_with_no_open_row_raises_once() -> R {
    let mut rig = rig()?;
    let build1 = rig.running_build1()?;
    rig.tick()?;
    rig.tick()?;
    assert_eq!(rig.count(FANOUT_UNBOUND), 1);
    let text = rig.exceptions();
    assert!(
        text.contains(NEED) && text.contains("r1") && text.contains(&build1.task),
        "{text}"
    );
    Ok(())
}

#[test]
fn a_running_fanout_step_with_an_open_row_raises_nothing() -> R {
    let mut rig = rig()?;
    let build1 = rig.running_build1()?;
    rig.store.set_open_fanout(NEED, 1, &build1.task)?;
    rig.tick()?;
    assert_eq!(rig.count(FANOUT_UNBOUND), 0);
    Ok(())
}
