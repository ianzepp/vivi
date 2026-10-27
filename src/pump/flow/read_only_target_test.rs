//! Read-only target regressions over the shipped registry: every shipped
//! flow seat that starts a read-only job starts on the root need's own
//! target and that target's main, and unresolvable targets are refused
//! before anything is enqueued.

use std::collections::BTreeMap;

use serde_json::json;
use tempfile::{TempDir, tempdir};

use crate::error::VivariumError;
use crate::letter::fixture::need_body_with;
use crate::letter::work::NeedFront;
use crate::pump::config::{PumpConfig, PumpTarget};
use crate::pump::flow::def::{FlowDef, FlowKind};
use crate::pump::flow::fake_mail::{FakeMail, FakeTask};
use crate::pump::flow::handlers::{Registry, shipped};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailWriter, TaskState, TaskStatus};
use crate::pump::flow::outcome::{StartAction, StartCx};
use crate::pump::flow::step_inputs::POOL_RESULT_MARKER;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{self, JobSpec, JobStore, PoolJob};
use crate::pump::store::step_payload::StepPayloadRow;
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::{RoleRouting, RoutedPool, RoutedRole};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const SECOND_NEED: &str = "n2";
const BASE: &str = "0123456789abcdef0123456789abcdef01234567";

/// The shipped seats and their build-flow nodes, in flow order.
const SEATS: [(&str, &str); 6] = [
    ("research", "research"),
    ("plan", "planner"),
    ("slice", "slicer"),
    ("judge1", "judge"),
    ("rework1", "rework"),
    ("cto", "cto"),
];

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

/// The workspace `main` plus one named target `vivi` with its own main.
fn vivi_only_cfg() -> PumpConfig {
    let mut cfg = PumpConfig::default();
    cfg.targets.insert(
        String::from("vivi"),
        PumpTarget {
            main: String::from("vivi/main"),
            ..PumpTarget::default()
        },
    );
    cfg
}

/// The explicit primary `default` and the non-primary `vivi`, distinct mains.
fn default_and_vivi_cfg() -> PumpConfig {
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
    cfg
}

/// Two named targets with no `default`: nothing is primary.
fn defaultless_cfg() -> PumpConfig {
    let mut cfg = PumpConfig::default();
    for (id, main) in [("alpha", "alpha/main"), ("vivi", "vivi/main")] {
        cfg.targets.insert(
            id.to_owned(),
            PumpTarget {
                main: main.to_owned(),
                ..PumpTarget::default()
            },
        );
    }
    cfg
}

/// Every shipped read-only seat routed active over the test pool.
fn seat_routing() -> RoleRouting {
    let role = |name: &str, kind: &str| RoutedRole {
        name: name.into(),
        kind: Some(kind.into()),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: None,
        model: None,
        models: Vec::new(),
    };
    RoleRouting {
        roles: vec![
            role("researcher", "researcher"),
            role("pump-planner", "planner"),
            role("admitter", "admitter"),
            role("slicer", "slicer"),
            role("auditor", "auditor"),
            role("head-cto", "cto"),
        ],
        pools: BTreeMap::from([(
            String::from("test-planner-pool"),
            RoutedPool {
                slots: 4,
                provider: String::from("local"),
                fallback: None,
                caps: BTreeMap::new(),
                models: Vec::new(),
                has_bands: false,
            },
        )]),
    }
}

fn rig_with(cfg: PumpConfig) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), cfg);
    host.reply("need show", 0, "the need thread");
    let ctx = ctx.with_routing(seat_routing());
    let reply_from = ctx.cfg.reply_from.clone();
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    store.insert_run(&run_of(NEED))?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail: FakeMail::new(&reply_from),
        jobs: Jobs::default(),
    })
}

fn run_of(need: &str) -> RunRow {
    RunRow {
        need: need.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    }
}

/// A valid readable need root under `need`, naming `target`.
fn seed_root(mail: &mut FakeMail, need: &str, target: Option<&str>) {
    let mut front = NeedFront::new(vec![String::from("it works")]);
    front.target = target.map(str::to_owned);
    mail.tasks.insert(
        need.into(),
        FakeTask {
            from: "mind".into(),
            to: "mind".into(),
            subject: "Need".into(),
            body: need_body_with(&front, "Build the thing"),
            depends_on: Vec::new(),
            replies: Vec::new(),
            state: TaskState {
                status: TaskStatus::Open,
                closer: None,
                verdict: None,
            },
            branch: None,
            commit: None,
        },
    );
}

impl Rig {
    /// A finished planner pool reply stored the way the slicer and the rework
    /// brief read it: the Done `plan` row, the marked reply, the payload.
    fn planned(&mut self, need: &str) -> R {
        let task = self
            .mail
            .send_step("pump", "pump", "plan", "plan body", &[])?;
        self.mail.reply(
            &task,
            &format!(
                "{POOL_RESULT_MARKER}j: finished\npool p  slot 1  model m\n\n--- receipt\nPLAN TEXT"
            ),
        )?;
        self.store.insert_step(
            need,
            &StepRow {
                run: 1,
                node: String::from("plan"),
                task: task.clone(),
                state: StepState::Done,
                verdict: Some(String::from("planned")),
            },
        )?;
        self.store.record_step_payload(&StepPayloadRow {
            need: need.into(),
            run: 1,
            node: String::from("plan"),
            class: String::from("planner"),
            task,
            payload: json!({ "body": "PLAN TEXT" }),
        })
    }

    /// The auditor and need-rung rows a judge start reads, keyed per need.
    fn judge_rows(&self, need: &str) -> R {
        for (node, class, task, payload) in [
            (
                "audit1",
                "auditor",
                format!("{need}-audit"),
                json!({"front": {"answer": "defects"}, "body": "AUDIT-ANSWER-TEXT"}),
            ),
            (
                "test1",
                "rung-need",
                format!("{need}-rung"),
                json!({"exit": 0, "failing": []}),
            ),
        ] {
            self.store.record_step_payload(&StepPayloadRow {
                need: need.into(),
                run: 1,
                node: node.into(),
                class: class.into(),
                task: task.into(),
                payload,
            })?;
        }
        Ok(())
    }

    /// A live need-branch row on `target`, the rework seat's own branch.
    fn need_branch_row(&self, need: &str, target: &str) -> R {
        self.store.upsert_need_branch(&NeedBranchRow {
            need: need.into(),
            branch: format!("{target}/need/{need}"),
            base: Some(BASE.into()),
            status: "live".into(),
            run: 1,
            target: target.into(),
            priors: Vec::new(),
        })
    }

    /// Start `node` through the shipped registry and enqueue the job it
    /// returns, the handler-to-enqueue path the tick runs.
    fn start_seat(
        &mut self,
        need: &str,
        node: &str,
        seat_task: &str,
    ) -> Result<JobSpec, VivariumError> {
        let registry: Registry<Jobs, FakeMail> = shipped(&self.ctx);
        let def: FlowDef = load(FlowKind::Build)?;
        let flow_node = def
            .nodes
            .iter()
            .find(|n| n.id == node)
            .ok_or_else(|| VivariumError::Message(format!("no node {node}")))?;
        let handler = registry
            .handler_for(FlowKind::Build, flow_node)
            .ok_or_else(|| VivariumError::Message(format!("no shipped handler for {node}")))?;
        let run = run_of(need);
        let step = StepRow {
            run: 1,
            node: node.into(),
            task: seat_task.into(),
            state: StepState::Pending,
            verdict: None,
        };
        let cx = StartCx {
            ctx: &self.ctx,
            store: &self.store,
            mail: &self.mail,
            run: &run,
            def: &def,
        };
        match handler.start(&cx, &step)? {
            StartAction::Job(spec) => {
                let job = *spec;
                jobs::enqueue(&mut self.jobs, job.clone())?;
                Ok(job)
            }
            other => Err(VivariumError::Message(format!(
                "{node} did not start a job: {other:?}"
            ))),
        }
    }
}

#[test]
fn read_only_flow_seats_start_on_need_target_main() -> R {
    let mut rig = rig_with(vivi_only_cfg())?;
    seed_root(&mut rig.mail, NEED, Some("vivi"));
    rig.planned(NEED)?;
    rig.judge_rows(NEED)?;
    rig.need_branch_row(NEED, "vivi")?;

    for (node, seat) in SEATS {
        let task = format!("t-{seat}");
        let job = rig.start_seat(NEED, node, &task)?;
        assert_eq!(job.target.as_deref(), Some("vivi"), "{node}");
        assert_eq!(job.from_ref.as_deref(), Some("vivi/main"), "{node}");
        assert!(job.read_only, "{node} is not read-only");
        assert_eq!(job.task.as_deref(), Some(task.as_str()), "{node}");
        assert_eq!(
            job.extra_targets,
            vec![String::from("vivi")],
            "{node} extra targets"
        );
    }
    Ok(())
}

#[test]
fn read_only_flow_jobs_use_each_targets_main() -> R {
    let mut rig = rig_with(default_and_vivi_cfg())?;
    for (need, target) in [(NEED, "default"), (SECOND_NEED, "vivi")] {
        seed_root(&mut rig.mail, need, Some(target));
        rig.planned(need)?;
        rig.judge_rows(need)?;
        rig.need_branch_row(need, target)?;
    }

    let mut started = 0;
    for (need, target, main) in [
        (NEED, "default", "default/main"),
        (SECOND_NEED, "vivi", "vivi/main"),
    ] {
        for (node, seat) in SEATS {
            let task = format!("{need}-{seat}");
            let job = rig.start_seat(need, node, &task)?;
            assert_eq!(job.target.as_deref(), Some(target), "{node} of {need}");
            assert_eq!(job.from_ref.as_deref(), Some(main), "{node} of {need}");
            assert!(job.read_only, "{node} of {need}");
            assert_eq!(job.task.as_deref(), Some(task.as_str()), "{node}");
            started += 1;
        }
    }
    // Every seat of both needs sits in one store, each under its own id.
    assert_eq!(started, 12);
    assert_eq!(rig.jobs.jobs.len(), 12);
    Ok(())
}

#[test]
fn read_only_flow_rejects_unresolvable_targets_with_an_empty_store() -> R {
    // A valid root letter naming an unconfigured target: refused by the
    // shipped handler, nothing enqueued.
    let mut rig = rig_with(default_and_vivi_cfg())?;
    seed_root(&mut rig.mail, NEED, Some("ghost"));
    let error = rig
        .start_seat(NEED, "research", "t-ghost")
        .expect_err("unknown target must be refused");
    assert!(matches!(error, VivariumError::Config(_)), "{error}");
    assert!(rig.jobs.jobs.is_empty());

    // Two named targets with no `default` and no root target: nothing is
    // primary, so the start is refused too.
    let mut rig = rig_with(defaultless_cfg())?;
    seed_root(&mut rig.mail, NEED, None);
    let error = rig
        .start_seat(NEED, "research", "t-nodefault")
        .expect_err("a defaultless pair must be refused");
    assert!(matches!(error, VivariumError::Config(_)), "{error}");
    assert!(rig.jobs.jobs.is_empty());
    Ok(())
}
