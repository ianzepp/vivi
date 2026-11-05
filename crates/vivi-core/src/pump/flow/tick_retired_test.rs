//! Tests for the run-state guard in `RunCx::run`: a run that a collected
//! result retires during its own tick collects no further result, starts no
//! further step and opens no further job, while the successor is unaffected.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tempfile::{TempDir, tempdir};

use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::restart::restart;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::collect_step::{RunCx, StepCollector};
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::run_state;
use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobSpec, JobStatus, JobStore, PoolJob, enqueue};
use crate::pump::store::{DB_FILENAME, PumpStore};
use vivi_error::VivariumError;
use vivi_mail::letter::fixture::step_body;

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

/// The planner collector: a collected terminal restart of the run at `plan`.
struct RestartOnCollect;

impl StepCollector<Jobs, FakeMail> for RestartOnCollect {
    fn collect(
        &self,
        cx: &mut RunCx<'_, Jobs, FakeMail>,
        _job: &PoolJob,
        _row: &StepJobRow,
        _step: &StepRow,
        _class: &NodeClass,
    ) -> Result<(), VivariumError> {
        restart(cx.ctx, cx.store, cx.mail, &cx.run, "plan").map(drop)
    }
}

/// A collector that only counts: an old result it sees was routed.
struct Counted(Arc<AtomicUsize>);

impl StepCollector<Jobs, FakeMail> for Counted {
    fn collect(
        &self,
        _cx: &mut RunCx<'_, Jobs, FakeMail>,
        _job: &PoolJob,
        _row: &StepJobRow,
        _step: &StepRow,
        _class: &NodeClass,
    ) -> Result<(), VivariumError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// A handler that starts a step by enqueueing a job, and counts its starts.
struct StartJob {
    id: &'static str,
    starts: Arc<AtomicUsize>,
}

impl StepHandler for StartJob {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        let spec = JobSpec::new(self.id, "slicer", "pump-slicer", "s", "b");
        Ok(StartAction::Job(Box::new(spec)))
    }

    fn poll(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<Option<String>, VivariumError> {
        Ok(None)
    }
}

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
    registry: Registry<Jobs, FakeMail>,
    admitter: Arc<AtomicUsize>,
    slicer: Arc<AtomicUsize>,
}

/// A need with a live run 1: sentinel, `plan` and `admit1` running with one
/// finished result each, and `slice` still pending with a handler that would
/// start a job.
fn rig(blocked: bool) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("pump");
    MailWriter::open_record(&mut mail, NEED, "record")?;
    start(&ctx, &store, &mut mail, NEED, None)?;
    if blocked {
        mail.set_prerequisites(NEED, &["dep"]);
    }
    let mut tasks = BTreeMap::new();
    for (node, state) in [
        ("plan", StepState::Running),
        ("admit1", StepState::Running),
        ("slice", StepState::Pending),
    ] {
        let task = mail.send_step("pump", "pump", node, &step_body(NEED, 1, node, ""), &[])?;
        mail.bind(NEED, std::slice::from_ref(&task))?;
        store.insert_step(
            NEED,
            &StepRow {
                run: 1,
                node: node.into(),
                task: task.clone(),
                state,
                verdict: None,
            },
        )?;
        tasks.insert(node, task);
    }
    let admitter = Arc::new(AtomicUsize::new(0));
    let slicer = Arc::new(AtomicUsize::new(0));
    let mut registry = Registry::new();
    registry.collector(
        FlowKind::Build,
        NodeClass::Planner,
        Box::new(RestartOnCollect),
    );
    registry.collector(
        FlowKind::Build,
        NodeClass::Admitter,
        Box::new(Counted(Arc::clone(&admitter))),
    );
    registry.handler(
        FlowKind::Build,
        NodeClass::Slicer,
        Box::new(StartJob {
            id: "j-old-slice",
            starts: Arc::clone(&slicer),
        }),
    );
    let mut jobs = Jobs::default();
    // The restarting result is collected first: the map is sorted by id.
    for (id, node) in [("j-a-plan", "plan"), ("j-b-admit", "admit1")] {
        let task = tasks.get(node).cloned().unwrap_or_default();
        let mut spec = JobSpec::new(id, "planner", "pump-planner", "s", "b");
        spec.task = Some(task.clone());
        enqueue(&mut jobs, spec)?;
        store.insert_step_job(
            NEED,
            &StepJobRow {
                job: id.into(),
                run: 1,
                node: node.into(),
                task,
                attempt: 1,
            },
        )?;
        if let Some(job) = jobs.jobs.get_mut(id) {
            job.status = JobStatus::Finished;
        }
    }
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs,
        registry,
        admitter,
        slicer,
    })
}

impl Rig {
    fn tick(&mut self) -> R {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.mail,
            &self.registry,
        )
    }

    fn state_of(&self, run: u32) -> Result<String, VivariumError> {
        self.store
            .runs_of(NEED)?
            .into_iter()
            .find(|row| row.run == run)
            .map(|row| row.state)
            .ok_or_else(|| VivariumError::Other(format!("no run {run}")))
    }

    fn calls(&self, counter: &Arc<AtomicUsize>) -> usize {
        counter.load(Ordering::SeqCst)
    }

    fn exceptions(&self) -> String {
        std::fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }
}

#[test]
fn a_collected_restart_stops_the_old_run_collecting_and_starting_more_work() -> R {
    // The restart is deferred by an open prerequisite, so the old run stays
    // `restarting` with its steps still open: only the state guard keeps it
    // from routing the second result and opening a new job.
    let mut rig = rig(true)?;
    rig.tick()?;

    assert_eq!(rig.state_of(1)?, run_state::RESTARTING);
    assert_eq!(
        rig.calls(&rig.admitter),
        0,
        "another old result is not routed"
    );
    assert_eq!(
        rig.calls(&rig.slicer),
        0,
        "a pending step of the old run does not start"
    );
    assert!(
        !rig.jobs.jobs.contains_key("j-old-slice"),
        "no new job is opened for the old run: {:?}",
        rig.jobs.jobs.keys().collect::<Vec<_>>()
    );
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_collected_restart_leaves_the_successor_untouched() -> R {
    let mut rig = rig(false)?;
    rig.tick()?;

    assert_eq!(rig.state_of(1)?, run_state::SUPERSEDED);
    assert_eq!(rig.state_of(2)?, run_state::LIVE);
    assert_eq!(rig.calls(&rig.admitter), 0);
    assert_eq!(rig.calls(&rig.slicer), 0);
    assert!(!rig.jobs.jobs.contains_key("j-old-slice"));

    rig.tick()?;
    assert_eq!(rig.state_of(1)?, run_state::SUPERSEDED);
    assert_eq!(rig.state_of(2)?, run_state::LIVE);
    assert_eq!(rig.mail.sentinel_count(NEED)?, 2, "one sentinel per run");
    assert_eq!(rig.exceptions(), "");
    Ok(())
}
