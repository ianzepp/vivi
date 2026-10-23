//! Tests for the registry-driven tick: fanout start, polling waiting steps,
//! tick phases and the step job payload.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tempfile::{TempDir, tempdir};

use super::{OPERATOR_HOLD, tick_with};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::envelope::roles::Slice;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::handlers::{Registry, TickPhase};
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler, role_job};
use crate::pump::flow::run::{SENTINEL_NODE, start};
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;
type Reg = Registry<Jobs, FakeMail>;
type Log = Arc<Mutex<Vec<String>>>;

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
}

fn rig(record: &str) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, record)?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs: Jobs::default(),
    })
}

impl Rig {
    fn start(&mut self, entry: &str) -> R {
        start(&self.ctx, &self.store, &mut self.mail, NEED, Some(entry)).map(|_| ())
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

    fn rows(&self) -> Result<Vec<StepRow>, VivariumError> {
        self.store.steps_of(NEED, 1)
    }

    fn row(&self, node: &str) -> Result<StepRow, VivariumError> {
        self.rows()?
            .into_iter()
            .find(|row| row.node == node)
            .ok_or_else(|| VivariumError::Message(format!("no step {node}")))
    }
}

fn note(log: &Log, text: &str) -> R {
    log.lock()
        .map_err(|e| VivariumError::Other(e.to_string()))?
        .push(text.to_owned());
    Ok(())
}

/// Expands two slices when started.
struct Slicing;

impl StepHandler for Slicing {
    fn start(&self, _cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let slice = |id: &str| Slice {
            id: id.into(),
            title: format!("title {id}"),
            brief: format!("brief {id}"),
            target: "vivi".into(),
            write_scope: vec![format!("src/{id}.rs")],
            deps: Vec::new(),
        };
        Ok(StartAction::Fanout {
            slicer_step: step.clone(),
            slices: vec![slice("a"), slice("b")],
        })
    }
}

/// Opens and waits; once polled it answers `done`. Notes its polls.
struct Polled {
    polls: Log,
}

impl StepHandler for Polled {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::OpenAndWait)
    }

    fn poll(&self, _cx: &StartCx<'_>, step: &StepRow) -> Result<Option<String>, VivariumError> {
        note(&self.polls, &step.node)?;
        Ok(Some("done".into()))
    }
}

/// Starts a planner job and notes it.
struct Planning {
    log: Log,
}

impl StepHandler for Planning {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        note(&self.log, "start")?;
        Ok(StartAction::Job(Box::new(role_job(
            cx.ctx, cx.run, step, "planner", "planner", "brief",
        ))))
    }
}

struct Noting {
    log: Log,
}

impl TickPhase<Jobs, FakeMail> for Noting {
    fn name(&self) -> &'static str {
        "noting"
    }

    fn run(
        &self,
        _ctx: &Ctx,
        _st: &mut Jobs,
        _store: &PumpStore,
        _mail: &mut FakeMail,
    ) -> Result<(), VivariumError> {
        note(&self.log, "phase")
    }
}

fn log() -> Log {
    Arc::new(Mutex::new(Vec::new()))
}

fn seen(log: &Log) -> Vec<String> {
    log.lock().map(|l| l.clone()).unwrap_or_default()
}

fn slice_count(rig: &Rig) -> Result<usize, VivariumError> {
    Ok(rig
        .rows()?
        .iter()
        .filter(|row| row.node.starts_with("slice:"))
        .count())
}

#[test]
fn a_fanout_handler_expands_its_slices_and_the_step_runs() -> R {
    let mut rig = rig("kind: build")?;
    rig.start("plan")?;
    // `build1` is not an entry: give the run a pending fanout row directly.
    let task = rig.mail.send_step(
        "mind",
        "mind",
        "build",
        &format!("pump-step: {NEED} r1 build1"),
        &[],
    )?;
    rig.store.insert_step(
        NEED,
        &StepRow {
            run: 1,
            node: "build1".into(),
            task,
            state: StepState::Pending,
            verdict: None,
        },
    )?;
    let mut registry = Reg::new();
    registry.handler(FlowKind::Build, NodeClass::Fanout, Box::new(Slicing));
    rig.tick(&registry)?;
    assert_eq!(slice_count(&rig)?, 2);
    assert_eq!(rig.row("build1")?.state, StepState::Running);
    let sentinel = rig.row(SENTINEL_NODE)?.task;
    let bound = rig.mail.calls.iter().find_map(|call| match call {
        Call::Bind { units, .. } if units.len() == 3 => Some(units.clone()),
        _ => None,
    });
    let bound = bound.ok_or_else(|| VivariumError::Message("no bind of the slices".into()))?;
    assert!(bound.contains(&sentinel));
    for row in rig.rows()?.iter().filter(|r| r.node.starts_with("slice:")) {
        assert!(bound.contains(&row.task));
    }
    // A second tick does not expand again.
    rig.tick(&registry)?;
    assert_eq!(slice_count(&rig)?, 2);
    Ok(())
}

#[test]
fn a_waiting_step_is_polled_and_its_answer_advances_it() -> R {
    let mut rig = rig("kind: release")?;
    rig.start("wait")?;
    let polls = log();
    let mut registry = Reg::new();
    registry.handler(
        FlowKind::Release,
        NodeClass::Milestone,
        Box::new(Polled {
            polls: polls.clone(),
        }),
    );
    rig.tick(&registry)?;
    assert_eq!(seen(&polls), vec!["wait".to_string()]);
    assert_eq!(rig.store.runs_of(NEED)?[0].state, "done");
    Ok(())
}

#[test]
fn a_held_step_is_not_polled() -> R {
    let mut rig = rig("kind: release")?;
    rig.start("wait")?;
    let task = rig.row("wait")?.task;
    rig.store
        .set_step_state(&task, StepState::Waiting, Some(OPERATOR_HOLD))?;
    let polls = log();
    let mut registry = Reg::new();
    registry.handler(
        FlowKind::Release,
        NodeClass::Milestone,
        Box::new(Polled {
            polls: polls.clone(),
        }),
    );
    rig.tick(&registry)?;
    assert!(seen(&polls).is_empty());
    assert_eq!(rig.row("wait")?.state, StepState::Waiting);
    assert_eq!(rig.store.runs_of(NEED)?[0].state, "live");
    Ok(())
}

#[test]
fn a_registered_phase_runs_once_per_tick_after_the_live_runs() -> R {
    let mut rig = rig("kind: build")?;
    rig.start("plan")?;
    let notes = log();
    let mut registry = Reg::new();
    registry.handler(
        FlowKind::Build,
        NodeClass::Planner,
        Box::new(Planning { log: notes.clone() }),
    );
    registry.phase(Box::new(Noting { log: notes.clone() }));
    rig.tick(&registry)?;
    assert_eq!(seen(&notes), vec!["start".to_string(), "phase".to_string()]);
    rig.tick(&registry)?;
    assert_eq!(
        seen(&notes),
        vec![
            "start".to_string(),
            "phase".to_string(),
            "phase".to_string()
        ]
    );
    Ok(())
}

#[test]
fn an_enqueued_step_job_carries_the_run_kind_and_node_class() -> R {
    let mut rig = rig("kind: build")?;
    rig.start("plan")?;
    let mut registry = Reg::new();
    registry.handler(
        FlowKind::Build,
        NodeClass::Planner,
        Box::new(Planning { log: log() }),
    );
    rig.tick(&registry)?;
    let job = rig
        .jobs
        .jobs
        .values()
        .next()
        .ok_or_else(|| VivariumError::Message("no job enqueued".into()))?;
    assert_eq!(job.payload["kind"], "build");
    assert_eq!(job.payload["class"], "planner");
    assert_eq!(job.payload["need"], NEED);
    Ok(())
}
