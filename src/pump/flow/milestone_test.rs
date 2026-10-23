//! The release milestone: waits while the need has an unfinished prerequisite
//! need, answers `ready` when none is left, and keeps waiting on an unreadable
//! graph.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::register;
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
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;
type Reg = Registry<Jobs, FakeMail>;

const NEED: &str = "rel1";
const GRAPH: &str = "graph blocked";

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
    registry: Reg,
}

/// A release run started at its `wait` milestone.
fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, "kind: release")?;
    start(&ctx, &store, &mut mail, NEED, None)?;
    let mut registry = Reg::new();
    register(&mut registry);
    Ok(Rig {
        _dir: dir,
        ctx,
        host,
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
}

#[test]
fn a_blocked_need_keeps_waiting() -> R {
    let rig = rig()?;
    rig.host.reply(
        GRAPH,
        0,
        &format!(r#"[{{"source_id":"{NEED}","blocked_by":["dep1"]}}]"#),
    );
    let (started, polled) = rig.handler()?;
    assert_eq!(started, StartAction::OpenAndWait);
    assert_eq!(polled, None);
    Ok(())
}

#[test]
fn a_need_absent_from_the_blocked_graph_answers_ready() -> R {
    let rig = rig()?;
    rig.host
        .reply(GRAPH, 0, r#"[{"source_id":"other","blocked_by":["dep1"]}]"#);
    let (_, polled) = rig.handler()?;
    assert_eq!(polled.as_deref(), Some("ready"));
    Ok(())
}

#[test]
fn an_unreadable_graph_keeps_waiting() -> R {
    let rig = rig()?;
    rig.host.reply(GRAPH, 1, "error: no such graph");
    let (_, polled) = rig.handler()?;
    assert_eq!(polled, None);
    Ok(())
}

#[test]
fn a_release_run_waits_while_blocked_then_lands_when_the_graph_clears() -> R {
    let mut rig = rig()?;
    rig.host.reply(
        GRAPH,
        0,
        &format!(r#"[{{"source_id":"{NEED}","blocked_by":["dep1"]}}]"#),
    );
    rig.tick()?;
    rig.tick()?;
    assert_eq!(rig.wait_step()?.state, StepState::Waiting);
    assert_eq!(rig.run()?.state, "live");

    rig.host.reply(GRAPH, 0, "[]");
    rig.tick()?;
    let step = rig.wait_step()?;
    assert_eq!(step.state, StepState::Done);
    assert_eq!(step.verdict.as_deref(), Some("ready"));
    rig.tick()?;
    assert_eq!(rig.run()?.state, "done");
    Ok(())
}
