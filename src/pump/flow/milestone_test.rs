//! The release milestone: answers `ready` once every prerequisite landed,
//! waits on open ones, alarms on held ones, and holds the step on one closed
//! without landing.

use std::collections::BTreeMap;
use std::fs;

use tempfile::{TempDir, tempdir};

use super::{MILESTONE_PREREQ_HELD, MILESTONE_UNLANDED, register};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::outcome::{StartAction, StartCx};
use crate::pump::flow::tick::{OPERATOR_HOLD, tick_with};
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;
type Reg = Registry<Jobs, FakeMail>;

const NEED: &str = "rel1";
const DEP: &str = "dep1";

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
    registry: Reg,
}

/// A release run started at its `wait` milestone, depending on `dep1`.
fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    mail.set_prerequisites(NEED, &[DEP]);
    MailWriter::open_record(&mut mail, NEED, "kind: release")?;
    start(&ctx, &store, &mut mail, NEED, None)?;
    let mut registry = Reg::new();
    register(&mut registry);
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs: Jobs::default(),
        registry,
    })
}

impl Rig {
    fn run(&self) -> Result<RunRow, VivariumError> {
        self.store
            .runs_of(NEED)?
            .into_iter()
            .next()
            .ok_or_else(|| VivariumError::Message("no run".into()))
    }

    fn wait_step(&self) -> Result<StepRow, VivariumError> {
        self.store
            .steps_of(NEED, 1)?
            .into_iter()
            .find(|row| row.node == "wait")
            .ok_or_else(|| VivariumError::Message("no wait step".into()))
    }

    fn tick(&mut self) -> R {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.mail,
            &self.registry,
        )
    }

    /// The handler's `start` and `poll` answers for the `wait` step.
    fn handler(&self) -> Result<(StartAction, Option<String>), VivariumError> {
        let run = self.run()?;
        let def = load(FlowKind::Release)?;
        let node = def
            .nodes
            .iter()
            .find(|n| n.id == "wait")
            .ok_or_else(|| VivariumError::Message("no wait node".into()))?;
        let handler = self
            .registry
            .handler_for(FlowKind::Release, node)
            .ok_or_else(|| VivariumError::Message("no milestone handler".into()))?;
        let cx = StartCx {
            ctx: &self.ctx,
            store: &self.store,
            mail: &self.mail,
            run: &run,
            def: &def,
        };
        let step = self.wait_step()?;
        Ok((handler.start(&cx, &step)?, handler.poll(&cx, &step)?))
    }

    /// The pump's `exceptions.txt`; empty when it was never written.
    fn exceptions(&self) -> String {
        match fs::read_to_string(self.ctx.home.join("exceptions.txt")) {
            Ok(text) => text,
            Err(_) => String::new(),
        }
    }

    fn alarms(&self, kind: &str) -> usize {
        self.exceptions()
            .lines()
            .filter(|line| line.contains(kind))
            .count()
    }

    /// The prerequisite closed with a pump record outcome, then done.
    fn closed_with(&mut self, outcome: &str) -> R {
        self.mail.open_record(DEP, "kind: build")?;
        self.mail.close_record(DEP, outcome)?;
        self.mail.close_need(DEP, "done")
    }

    /// The prerequisite has a live run of `kind` with one waiting step.
    fn dep_waiting_at(&self, kind: &str, node: &str) -> R {
        self.store.insert_run(&RunRow {
            need: DEP.into(),
            run: 1,
            flow: kind.into(),
            flow_version: 2,
            kind: kind.into(),
            entry: node.into(),
            state: "live".into(),
        })?;
        self.store.insert_step(
            DEP,
            &StepRow {
                run: 1,
                node: node.into(),
                task: format!("t-{node}"),
                state: StepState::Waiting,
                verdict: None,
            },
        )
    }
}

#[test]
fn no_prerequisites_is_ready() -> R {
    let mut rig = rig()?;
    rig.mail.set_prerequisites(NEED, &[]);
    let (started, polled) = rig.handler()?;
    assert_eq!(started, StartAction::OpenAndWait);
    assert_eq!(polled.as_deref(), Some("ready"));
    Ok(())
}

#[test]
fn a_landed_prerequisite_is_ready() -> R {
    let mut rig = rig()?;
    rig.closed_with("landed")?;
    let (_, polled) = rig.handler()?;
    assert_eq!(polled.as_deref(), Some("ready"));
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn an_open_prerequisite_waits_without_an_alarm() -> R {
    let rig = rig()?;
    let (_, polled) = rig.handler()?;
    assert_eq!(polled, None);
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_locked_prerequisite_waits_with_one_held_alarm() -> R {
    let mut rig = rig()?;
    rig.dep_waiting_at("build", "locked")?;
    rig.tick()?;
    rig.tick()?;
    let step = rig.wait_step()?;
    assert_eq!(step.state, StepState::Waiting);
    assert_ne!(step.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(rig.alarms(MILESTONE_PREREQ_HELD), 1, "{}", rig.exceptions());
    assert!(rig.exceptions().contains("held at locked"));
    assert_eq!(rig.alarms(MILESTONE_UNLANDED), 0);
    Ok(())
}

#[test]
fn a_repair_need_waiting_at_unrepaired_is_held_and_alarmed() -> R {
    let mut rig = rig()?;
    rig.dep_waiting_at("repair", "unrepaired")?;
    rig.tick()?;
    rig.tick()?;
    assert_eq!(rig.wait_step()?.state, StepState::Waiting);
    assert_eq!(rig.alarms(MILESTONE_PREREQ_HELD), 1, "{}", rig.exceptions());
    assert!(rig.exceptions().contains("held at unrepaired"));
    Ok(())
}

#[test]
fn a_withdrawn_prerequisite_holds_the_step_with_one_alarm() -> R {
    let mut rig = rig()?;
    rig.closed_with("withdrawn")?;
    rig.tick()?;
    rig.tick()?;
    let step = rig.wait_step()?;
    assert_eq!(step.state, StepState::Waiting);
    assert_eq!(step.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(rig.alarms(MILESTONE_UNLANDED), 1, "{}", rig.exceptions());
    assert!(
        rig.exceptions().contains(
            "release rel1 expects dep1 landed on main; actual: closed, outcome `withdrawn`"
        )
    );
    assert_eq!(rig.run()?.state, "live");
    Ok(())
}

#[test]
fn a_done_prerequisite_with_no_record_lets_the_release_land() -> R {
    let mut rig = rig()?;
    rig.mail.close_need(DEP, "done")?;
    rig.tick()?;
    let step = rig.wait_step()?;
    assert_eq!(step.state, StepState::Done);
    assert_eq!(step.verdict.as_deref(), Some("ready"));
    rig.tick()?;
    assert_eq!(rig.run()?.state, "done");
    assert_eq!(rig.exceptions(), "");
    Ok(())
}
