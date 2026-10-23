//! Tests for the admitter step handler.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use crate::error::VivariumError;
use crate::mailspace::shipped::ADMITTER_ROLE;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::{FlowDef, FlowKind};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::{Registry, shipped};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::outcome::{StartAction, StartCx};
use crate::pump::flow::step_inputs::POOL_RESULT_MARKER;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobSpec, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;

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
    store: PumpStore,
    mail: FakeMail,
    run: RunRow,
    def: FlowDef,
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.reply("need show", 0, "the need thread");
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from(ADMITTER_ROLE),
        kind: Some(String::from("admitter")),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: Some(true),
        model: None,
        models: Vec::new(),
    });
    let ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let run = RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 1,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    };
    store.insert_run(&run)?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail: FakeMail::new("pump"),
        run,
        def: load(FlowKind::Build)?,
    })
}

impl Rig {
    /// A Done planner step `node` whose task holds one pool result with `plan`.
    fn planned(&mut self, node: &str, plan: &str) -> R {
        let task = self.mail.send_step("pump", "pump", node, "body", &[])?;
        self.mail.reply(
            &task,
            &format!(
                "{POOL_RESULT_MARKER}j: finished\npool p  slot 1  model m\n\n--- receipt\n{plan}"
            ),
        )?;
        self.done_row(node, &task)
    }

    fn done_row(&self, node: &str, task: &str) -> R {
        self.store.insert_step(
            NEED,
            &StepRow {
                run: 1,
                node: node.into(),
                task: task.into(),
                state: StepState::Done,
                verdict: None,
            },
        )
    }

    fn start(&self, node: &str) -> Result<StartAction, VivariumError> {
        let reg: Reg = shipped(&self.ctx);
        let flow_node = self
            .def
            .nodes
            .iter()
            .find(|n| n.id == node)
            .ok_or_else(|| VivariumError::Message(format!("no node {node}")))?;
        let handler = reg
            .handler_for(FlowKind::Build, flow_node)
            .ok_or_else(|| VivariumError::Message(format!("no handler for {node}")))?;
        let step = StepRow {
            run: 1,
            node: node.into(),
            task: format!("task-{node}"),
            state: StepState::Running,
            verdict: None,
        };
        let cx = StartCx {
            ctx: &self.ctx,
            store: &self.store,
            mail: &self.mail,
            run: &self.run,
            def: &self.def,
        };
        handler.start(&cx, &step)
    }
}

fn job(action: StartAction) -> Result<JobSpec, VivariumError> {
    match action {
        StartAction::Job(spec) => Ok(*spec),
        other => Err(VivariumError::Message(format!("not a job: {other:?}"))),
    }
}

#[test]
fn admit1_is_a_read_only_admitter_job_carrying_the_plan_and_thread() -> R {
    let mut rig = rig()?;
    rig.planned("plan", "PLAN ONE TEXT")?;

    let spec = job(rig.start("admit1")?)?;

    assert_eq!(spec.role, ADMITTER_ROLE);
    assert!(spec.read_only);
    assert!(spec.body.contains("PLAN ONE TEXT"), "{}", spec.body);
    assert!(spec.body.contains("the need thread"), "{}", spec.body);
    assert!(spec.body.contains("need: n1"), "{}", spec.body);
    assert!(spec.body.contains("run: 1"), "{}", spec.body);
    Ok(())
}

#[test]
fn admit2_reads_plan2_not_plan() -> R {
    let mut rig = rig()?;
    rig.planned("plan", "PLAN ONE TEXT")?;
    rig.planned("plan2", "PLAN TWO TEXT")?;

    let spec = job(rig.start("admit2")?)?;

    assert!(spec.body.contains("PLAN TWO TEXT"), "{}", spec.body);
    assert!(!spec.body.contains("PLAN ONE TEXT"), "{}", spec.body);
    Ok(())
}

#[test]
fn a_missing_plan_reply_is_an_error_naming_the_node() -> R {
    let mut rig = rig()?;
    let task = rig.mail.send_step("pump", "pump", "plan", "body", &[])?;
    rig.done_row("plan", &task)?;

    let err = rig.start("admit1").unwrap_err();

    assert!(err.to_string().contains("admit1"), "{err}");
    Ok(())
}

#[test]
fn no_finished_planner_step_is_an_error() -> R {
    let rig = rig()?;

    let err = rig.start("admit1").unwrap_err();

    assert!(err.to_string().contains("admit1"), "{err}");
    Ok(())
}
