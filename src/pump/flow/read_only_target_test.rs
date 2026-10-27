//! Cross-handler acceptance for the read-only seats: every shipped handler
//! starts on the root need letter's own target and that target's main.
//!
//! Handlers resolve through the shipped registry, never mocked, so the
//! pairing the pump will actually enqueue is what these tests assert. The
//! focused per-seat regressions keep their own companion files; this file is
//! the integrated view across research, planner, slicer, judge, rework and
//! the CTO.

use std::collections::BTreeMap;

use serde_json::json;
use tempfile::{TempDir, tempdir};

use crate::error::VivariumError;
use crate::letter::fixture::need_body_with;
use crate::letter::work::NeedFront;
use crate::mailspace::shipped::{RESEARCHER_ROLE, SLICER_ROLE};
use crate::pump::config::{PumpConfig, PumpTarget};
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::{FakeMail, FakeTask};
use crate::pump::flow::handlers::{Registry, shipped};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailWriter, TaskState, TaskStatus};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::step_inputs::POOL_RESULT_MARKER;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobSpec, JobStore, PoolJob};
use crate::pump::store::step_payload::StepPayloadRow;
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED_A: &str = "n1";
const NEED_B: &str = "n2";
const VIVI: &str = "vivi";
const VIVI_MAIN: &str = "vivi/main";
const BASE: &str = "0123456789abcdef0123456789abcdef01234567";

/// The shipped seats and the role each seat's job names.
const SEATS: [(&str, &str); 6] = [
    ("research", "researcher"),
    ("plan", "pump-planner"),
    ("slice", "slicer"),
    ("judge1", "auditor"),
    ("rework1", "slicer"),
    ("cto", "head-cto"),
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

/// Two named targets: the explicit primary `default` at the workspace main
/// and the non-primary `vivi` at its own main.
fn two_target_cfg() -> PumpConfig {
    let mut cfg = PumpConfig::default();
    cfg.targets.insert(
        String::from("default"),
        PumpTarget {
            main: cfg.main.clone(),
            ..PumpTarget::default()
        },
    );
    cfg.targets.insert(
        String::from(VIVI),
        PumpTarget {
            main: String::from(VIVI_MAIN),
            ..PumpTarget::default()
        },
    );
    cfg
}

/// Two named targets with distinct mains and no `default` among them.
fn no_default_cfg() -> PumpConfig {
    let mut cfg = PumpConfig::default();
    for (id, main) in [(VIVI, VIVI_MAIN), ("zinc", "zinc/main")] {
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

fn rig() -> Result<Rig, VivariumError> {
    rig_with(two_target_cfg())
}

/// A rig over `cfg`: routed researcher and slicer roles beside the test
/// planner, and two root need letters, `n1` targeting `vivi` and `n2` the
/// primary `default`.
fn rig_with(cfg: PumpConfig) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(role(RESEARCHER_ROLE, "researcher"));
    routing.roles.push(role(SLICER_ROLE, "slicer"));
    let ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    seed_root(&mut mail, NEED_A, Some(VIVI));
    seed_root(&mut mail, NEED_B, Some("default"));
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs: Jobs::default(),
    })
}

/// An active routing role named `name` of kind `kind` on the test pool.
fn role(name: &str, kind: &str) -> RoutedRole {
    RoutedRole {
        name: name.into(),
        kind: Some(kind.into()),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: None,
        model: None,
        models: Vec::new(),
    }
}

/// A valid readable root letter under `need`; `target` names its `target:`.
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

/// Everything the six seats read beside the root of `need`: a planned
/// planner reply, audit, judge and rung rows with payloads, a live need
/// branch on `branch_target`, and the run row itself.
fn seed_run(rig: &mut Rig, need: &str, branch_target: &str) -> Result<(), VivariumError> {
    rig.store.insert_run(&run_row(need))?;
    let plan = rig.mail.send_step("pump", "pump", "plan", "body", &[])?;
    rig.mail.reply(
        &plan,
        &format!(
            "{POOL_RESULT_MARKER}j: finished\npool p  slot 1  model m\n\n--- receipt\nPLAN TEXT"
        ),
    )?;
    rig.store
        .insert_step(need, &done("plan", &plan, Some("planned")))?;
    let audit = format!("{need}-audit");
    let judge = format!("{need}-judge");
    let rung = format!("{need}-rung");
    for (node, task, verdict) in [
        ("audit1", audit.as_str(), Some("block")),
        ("judge1", judge.as_str(), Some("block")),
        ("test1", rung.as_str(), Some("fail")),
    ] {
        rig.store.insert_step(need, &done(node, task, verdict))?;
    }
    seed_payloads(rig, need, &plan, &audit, &judge, &rung)?;
    rig.store.upsert_need_branch(&NeedBranchRow {
        need: need.into(),
        branch: format!("vivi/need/{need}"),
        base: Some(BASE.into()),
        status: "live".into(),
        run: 1,
        target: branch_target.into(),
        priors: Vec::new(),
    })
}

/// The planner, auditor, judge and need-rung payloads the judge and rework
/// briefs read; `plan`, `audit`, `judge` and `rung` are the task handles the
/// step rows carry.
fn seed_payloads(
    rig: &Rig,
    need: &str,
    plan: &str,
    audit: &str,
    judge: &str,
    rung: &str,
) -> Result<(), VivariumError> {
    let rows = [
        ("planner", "plan", plan, json!({ "body": "THE PLAN" })),
        (
            "auditor",
            "audit1",
            audit,
            json!({ "front": {
                "answer": "block_range",
                "finding": [{
                    "id": "F-1", "content": "the cache is never cleared",
                    "paths": ["src/cache.rs"], "recheck_trigger": "cache edits",
                }],
                "requirement": [
                    { "id": "R1", "text": "clears the cache", "status": "MISSING", "evidence": [] },
                ],
            }, "body": "AUDIT TEXT" }),
        ),
        (
            "judge",
            "judge1",
            judge,
            json!({ "front": { "answer": "block", "reason": "JUDGE SAYS BLOCK" } }),
        ),
        (
            "rung-need",
            "test1",
            rung,
            json!({ "exit": 101, "tail": "boom", "failing": ["vivi::pump::a_test"] }),
        ),
    ];
    for (class, node, task, payload) in rows {
        rig.store.record_step_payload(&StepPayloadRow {
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

/// A Done step row of run 1.
fn done(node: &str, task: &str, verdict: Option<&str>) -> StepRow {
    StepRow {
        run: 1,
        node: node.into(),
        task: task.into(),
        state: StepState::Done,
        verdict: verdict.map(str::to_string),
    }
}

fn run_row(need: &str) -> RunRow {
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

/// The job the shipped handler of `node` starts for a running step of `need`.
fn start(rig: &Rig, need: &str, node: &str, task: &str) -> Result<JobSpec, VivariumError> {
    let registry: Registry<Jobs, FakeMail> = shipped(&rig.ctx);
    let def = load(FlowKind::Build)?;
    let run = run_row(need);
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &run,
        def: &def,
    };
    let flow_node = def
        .nodes
        .iter()
        .find(|n| n.id == node)
        .ok_or_else(|| VivariumError::Message(format!("no node {node}")))?;
    let handler = registry
        .handler_for(FlowKind::Build, flow_node)
        .ok_or_else(|| VivariumError::Message(format!("no handler for {node}")))?;
    let step = StepRow {
        run: 1,
        node: node.into(),
        task: task.into(),
        state: StepState::Running,
        verdict: None,
    };
    match handler.start(&cx, &step)? {
        StartAction::Job(spec) => Ok(*spec),
        other => Err(VivariumError::Message(format!(
            "{node} did not start a job: {other:?}"
        ))),
    }
}

/// The pairing every seat must keep: the need's own target, that target's
/// main as the start ref, and read-only mode.
fn assert_on_target(spec: &JobSpec, label: &str, target: &str, main: &str) {
    assert_eq!(spec.target.as_deref(), Some(target), "{label}");
    assert_eq!(spec.from_ref.as_deref(), Some(main), "{label}");
    assert!(spec.read_only, "{label}");
}

/// Every shipped seat refuses `need` on `rig` with a config error and
/// enqueues nothing.
fn assert_all_refused(rig: &Rig, need: &str) {
    for (node, _) in SEATS {
        let task = format!("t-{node}");
        let err = start(rig, need, node, &task).unwrap_err();
        assert!(matches!(err, VivariumError::Config(_)), "{node}: {err}");
    }
    assert!(rig.jobs.jobs.is_empty(), "a refused start enqueued a job");
}

#[test]
fn read_only_flow_seats_start_on_need_target_main() -> R {
    let mut rig = rig()?;
    seed_run(&mut rig, NEED_A, VIVI)?;
    for (node, role) in SEATS {
        let task = format!("t-{node}");
        let spec = start(&rig, NEED_A, node, &task)?;
        // The workspace main is `main`; the root need letter targets the
        // non-primary `vivi`, so every seat starts from vivi/main.
        assert_on_target(&spec, node, VIVI, VIVI_MAIN);
        assert_eq!(spec.task.as_deref(), Some(task.as_str()), "{node}");
        assert_eq!(spec.role, role, "{node}");
        assert_eq!(
            spec.extra_targets,
            vec![String::from("default"), String::from(VIVI)],
            "{node}"
        );
    }
    Ok(())
}

#[test]
fn read_only_flow_jobs_use_each_targets_main() -> R {
    let mut rig = rig()?;
    seed_run(&mut rig, NEED_A, VIVI)?;
    seed_run(&mut rig, NEED_B, "default")?;
    // Two needs, one per target: each produced specification keeps its own
    // target/main pairing, never the other target's main.
    for (need, target, main) in [
        (NEED_A, VIVI, VIVI_MAIN),
        (NEED_B, "default", rig.ctx.cfg.main.as_str()),
    ] {
        for (node, _) in SEATS {
            let task = format!("t-{need}-{node}");
            let spec = start(&rig, need, node, &task)?;
            assert_on_target(&spec, &format!("{need} {node}"), target, main);
            assert_eq!(spec.task.as_deref(), Some(task.as_str()), "{need} {node}");
        }
    }
    Ok(())
}

#[test]
fn read_only_flow_refuses_unknown_target_and_defaultless_pair() -> R {
    // A readable root whose `target:` names no configured target: every
    // shipped seat refuses with a config error and enqueues nothing.
    let mut rig = rig()?;
    seed_run(&mut rig, NEED_A, VIVI)?;
    seed_root(&mut rig.mail, NEED_A, Some("ghost"));
    assert_all_refused(&rig, NEED_A);

    // Two named targets without a `default` and a readable root with no
    // target: the resolution refuses instead of guessing a repository.
    let mut rig = rig_with(no_default_cfg())?;
    seed_run(&mut rig, NEED_A, VIVI)?;
    seed_root(&mut rig.mail, NEED_A, None);
    assert_all_refused(&rig, NEED_A);
    Ok(())
}
