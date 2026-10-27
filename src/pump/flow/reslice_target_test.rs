//! Tests for the target a reslice slicer job runs against: the original
//! slice's own target wins over the root need's target, read lazily.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use super::{Mode, request};
use crate::error::VivariumError;
use crate::letter::fixture::{need_body_with, task_body_with};
use crate::letter::work::{NeedFront, TaskFront};
use crate::mailspace::shipped::{ADMITTER_ROLE, SLICER_ROLE};
use crate::pump::config::{PumpConfig, PumpTarget};
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
    run: RunRow,
    slice: StepRow,
    def: FlowDef,
}

/// What the rig plants as the root letter of `NEED`.
enum Root {
    /// A valid letter whose front matter carries this `target:`.
    Targeted(&'static str),
    /// A letter that does not parse.
    Unreadable,
}

fn step(node: &str, task: &str) -> StepRow {
    StepRow {
        run: 1,
        node: node.into(),
        task: task.into(),
        state: StepState::Running,
        verdict: None,
    }
}

/// A rig with the `default` (primary) and `vivi` targets configured, a root
/// letter of `root`, and one running slice whose letter names `slice_target`.
fn target_rig(root: Root, slice_target: Option<&str>) -> Result<Rig, VivariumError> {
    let mut cfg = PumpConfig::default();
    for (id, main) in [("default", "default/main"), ("vivi", "vivi/main")] {
        cfg.targets.insert(
            id.to_owned(),
            PumpTarget {
                main: main.to_owned(),
                ..PumpTarget::default()
            },
        );
    }
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), cfg);
    host.reply("need show", 0, "the need thread");
    let mut routing = (*ctx.routing).clone();
    routing.roles.retain(|role| role.name != SLICER_ROLE);
    routing.roles.push(RoutedRole {
        name: SLICER_ROLE.into(),
        kind: Some(String::from("slicer")),
        status: String::from("active"),
        pool: Some(String::from("test-slicer-pool")),
        read_only: None,
        model: None,
        models: Vec::new(),
    });
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
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    };
    store.insert_run(&run)?;
    let mut mail = FakeMail::new(&ctx.cfg.reply_from);
    seed_root(&mut mail, root)?;
    let sentinel = mail.send_step("pump", "pump", "run", "sentinel", &[])?;
    store.insert_step(NEED, &step("sentinel", &sentinel))?;
    let slice_body = task_body_with(
        &TaskFront {
            target: slice_target.map(String::from),
            ..TaskFront::default()
        },
        "slice body text",
    );
    let slice_task = mail.send_step("pump", "pump", "slice a", &slice_body, &[])?;
    let slice = step("slice:a", &slice_task);
    store.insert_step(NEED, &slice)?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs: Jobs::default(),
        run,
        slice,
        def: load(FlowKind::Build)?,
    })
}

/// Stores the root letter of `NEED` under the need handle itself.
fn seed_root(mail: &mut FakeMail, root: Root) -> R {
    let body = match root {
        Root::Targeted(target) => {
            let mut front = NeedFront::new(vec![String::from("the need is done")]);
            front.target = Some(target.to_owned());
            need_body_with(&front, "prose")
        }
        Root::Unreadable => String::from("+++\ndone_when = 5\n+++\nprose"),
    };
    let sent = mail.send_step("pump", "pump", "the need", &body, &[])?;
    if let Some(task) = mail.tasks.remove(&sent) {
        mail.tasks.insert(NEED.to_string(), task);
    }
    Ok(())
}

impl Rig {
    /// A Done planner step whose task holds one finished pool reply.
    fn planned(&mut self, plan: &str) -> R {
        let task = self.mail.send_step("pump", "pump", "plan", "body", &[])?;
        self.mail.reply(
            &task,
            &format!(
                "{POOL_RESULT_MARKER}j: finished\npool p  slot 1  model m\n\n--- receipt\n{plan}"
            ),
        )?;
        self.store.insert_step(
            NEED,
            &StepRow {
                run: 1,
                node: String::from("plan"),
                task,
                state: StepState::Done,
                verdict: None,
            },
        )
    }

    /// Start the shipped admitter on `admit1` over the finished planner reply.
    fn admit1(&self) -> Result<JobSpec, VivariumError> {
        let reg: Registry<Jobs, FakeMail> = shipped(&self.ctx);
        let node = self
            .def
            .nodes
            .iter()
            .find(|n| n.id == "admit1")
            .ok_or_else(|| VivariumError::Message(String::from("no node admit1")))?;
        let handler = reg
            .handler_for(FlowKind::Build, node)
            .ok_or_else(|| VivariumError::Message(String::from("no handler for admit1")))?;
        let step = StepRow {
            run: 1,
            node: String::from("admit1"),
            task: String::from("task-admit1"),
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
        match handler.start(&cx, &step)? {
            StartAction::Job(spec) => Ok(*spec),
            other => Err(VivariumError::Message(format!("not a job: {other:?}"))),
        }
    }

    /// Request one reslice of the rig's slice.
    fn ask(&mut self, mode: Mode, input: &str) -> Result<String, VivariumError> {
        request(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.mail,
            &self.run,
            &self.slice,
            mode,
            input,
        )
    }

    /// The enqueued reslice job of `task`.
    fn job(&self, task: &str) -> Result<&PoolJob, VivariumError> {
        self.jobs
            .jobs
            .get(&format!("reslice-{task}-a1"))
            .ok_or_else(|| VivariumError::Message(format!("no reslice job for {task}")))
    }
}

#[test]
fn admitter_and_reslice_use_target_main() -> R {
    // The root need letter targets the primary `default`; the original slice
    // targets `vivi`, which differs from both the primary and the root.
    let mut rig = target_rig(Root::Targeted("default"), Some("vivi"))?;
    rig.planned("PLAN TEXT")?;

    // The shipped admitter falls back to the root need's target and starts
    // from that target's main.
    let admit = rig.admit1()?;
    assert_eq!(admit.target.as_deref(), Some("default"));
    assert_eq!(admit.from_ref.as_deref(), Some("default/main"));

    // Every reslice mode obeys the original slice's own target instead, and
    // starts from that target's main.
    for mode in [Mode::Split, Mode::Rework, Mode::FollowUp] {
        let task = rig.ask(mode, "redo it")?;
        let job = rig.job(&task)?;
        assert!(job.read_only, "{}", mode.name());
        assert_eq!(job.target.as_deref(), Some("vivi"), "{}", mode.name());
        assert_eq!(
            job.from_ref.as_deref(),
            Some("vivi/main"),
            "{}",
            mode.name()
        );
    }

    // Lazy fallback: while the slice names a valid target of its own the root
    // is never read, so an unreadable root still reslices against the slice's
    // target.
    let mut rig = target_rig(Root::Unreadable, Some("vivi"))?;
    let task = rig.ask(Mode::Split, "too big")?;
    let job = rig.job(&task)?;
    assert_eq!(job.target.as_deref(), Some("vivi"));
    assert_eq!(job.from_ref.as_deref(), Some("vivi/main"));

    // An unknown slice target is refused and enqueues nothing.
    let mut rig = target_rig(Root::Targeted("default"), Some("ghost"))?;
    assert!(rig.ask(Mode::Split, "too big").is_err());
    assert!(rig.jobs.jobs.is_empty());

    // Without a slice target the root is read; an unreadable root is refused
    // and enqueues nothing.
    let mut rig = target_rig(Root::Unreadable, None)?;
    assert!(rig.ask(Mode::Rework, "redo it").is_err());
    assert!(rig.jobs.jobs.is_empty());
    Ok(())
}
