//! Tests for the planner step handler.

use tempfile::{TempDir, tempdir};

use crate::error::VivariumError;
use crate::mailspace::shipped::PUMP_PLANNER_ROLE;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::{FlowKind, FlowNode, NodeClass};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::shipped;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::step_inputs::POOL_RESULT_MARKER;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const THREAD: &str = "need thread: build the widget";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    run: RunRow,
}

fn rig(with_role: bool, run_no: u32) -> Result<Rig, VivariumError> {
    rig_with(PumpConfig::default(), with_role, run_no)
}

fn rig_with(cfg: PumpConfig, with_role: bool, run_no: u32) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (mut ctx, host) = test_ctx(dir.path(), cfg);
    let mut routing = (*ctx.routing).clone();
    routing.roles.retain(|role| role.name != PUMP_PLANNER_ROLE);
    if with_role {
        routing.roles.push(RoutedRole {
            name: PUMP_PLANNER_ROLE.into(),
            kind: Some(String::from("planner")),
            status: String::from("active"),
            pool: Some(String::from("test-planner-pool")),
            read_only: None,
            model: None,
            models: Vec::new(),
        });
    }
    ctx = ctx.with_routing(routing);
    host.reply("need show", 0, THREAD);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let run = RunRow {
        need: NEED.into(),
        run: run_no,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    };
    store.insert_run(&run)?;
    let mut mail = FakeMail::new("pump");
    let body = crate::letter::fixture::need_body(&["build the widget"], "prose");
    let sent = mail.send_step("pump", "pump", "the need", &body, &[])?;
    if let Some(task) = mail.tasks.remove(&sent) {
        mail.tasks.insert(NEED.to_string(), task);
    }
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        run,
    })
}

fn row(run: u32, node: &str, task: &str, state: StepState) -> StepRow {
    StepRow {
        run,
        node: node.into(),
        task: task.into(),
        state,
        verdict: None,
    }
}

fn result(message: &str) -> String {
    format!("{POOL_RESULT_MARKER}j1: finished\npool p  slot 1  model m\n\n--- receipt\n{message}")
}

/// Starts the planner handler on `node` and returns the job's spec.
fn start(rig: &Rig, node: &str) -> Result<crate::pump::jobs::JobSpec, VivariumError> {
    let def = load(FlowKind::Build)?;
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &rig.run,
        def: &def,
    };
    let step = row(rig.run.run, node, "tp", StepState::Running);
    match super::PlannerStep.start(&cx, &step)? {
        StartAction::Job(spec) => Ok(*spec),
        other => Err(VivariumError::Other(format!("not a job: {other:?}"))),
    }
}

#[test]
fn plan_is_a_read_only_planner_job_with_the_thread() -> R {
    let rig = rig(true, 1)?;

    let spec = start(&rig, "plan")?;

    assert_eq!(spec.role, PUMP_PLANNER_ROLE);
    assert!(spec.read_only);
    assert!(spec.body.contains("need: n1\nrun: r1\n"), "{}", spec.body);
    assert!(spec.body.contains("targets: default"), "{}", spec.body);
    assert!(spec.body.contains(THREAD), "{}", spec.body);
    assert!(!spec.body.contains("first plan:"), "{}", spec.body);
    assert!(!spec.body.contains("prior need branch"), "{}", spec.body);
    Ok(())
}

#[test]
fn plan_brief_shows_done_when_target_and_depends_on_above_the_thread() -> R {
    let mut rig = rig(true, 1)?;
    let mut front = crate::letter::work::NeedFront::new(vec![
        String::from("first thing"),
        String::from("second thing"),
    ]);
    front.target = Some(String::from("default"));
    front.depends_on = vec![String::from("a1"), String::from("b2")];
    let body = crate::letter::fixture::need_body_with(&front, "prose");
    let need = rig.mail.send_step("pump", "pump", "the need", &body, &[])?;
    rig.run.need = need;

    let spec = start(&rig, "plan")?;

    let lines = "done_when: first thing\ndone_when: second thing\ntarget: default\ndepends_on: a1, b2\nthread:";
    assert!(spec.body.contains(lines), "{}", spec.body);
    Ok(())
}

#[test]
fn a_need_root_without_front_matter_fails_the_brief_naming_the_handle() -> R {
    let mut rig = rig(true, 1)?;
    let need = rig
        .mail
        .send_step("pump", "pump", "the need", "Build the widget", &[])?;
    rig.run.need = need.clone();

    let Err(err) = start(&rig, "plan") else {
        return Err(VivariumError::Other(String::from("brief did not fail")));
    };

    assert!(err.to_string().contains(&need), "{err}");
    assert!(err.to_string().contains("front matter"), "{err}");
    Ok(())
}

#[test]
fn plan2_carries_the_first_plan_and_the_admitter_reasons() -> R {
    let mut rig = rig(true, 1)?;
    let plan = rig.mail.send_step("pump", "pump", "plan", "b", &[])?;
    let admit = rig.mail.send_step("pump", "pump", "admit1", "b", &[])?;
    rig.store
        .insert_step(NEED, &row(1, "plan", &plan, StepState::Done))?;
    rig.store
        .insert_step(NEED, &row(1, "admit1", &admit, StepState::Done))?;
    rig.mail.reply(&plan, &result("the first plan text"))?;
    rig.mail.reply(&admit, &result("the admitter reasons"))?;

    let spec = start(&rig, "plan2")?;

    assert!(
        spec.body
            .contains("first plan:\nthe first plan text\nadmitter reasons:\nthe admitter reasons"),
        "{}",
        spec.body
    );
    Ok(())
}

#[test]
fn plan2_without_admitter_reasons_says_none() -> R {
    let rig = rig(true, 1)?;

    let spec = start(&rig, "plan2")?;

    assert!(
        spec.body
            .contains("first plan:\n(none)\nadmitter reasons:\n(none)"),
        "{}",
        spec.body
    );
    Ok(())
}

#[test]
fn a_later_run_names_the_last_prior_need_branch() -> R {
    let rig = rig(true, 2)?;
    rig.store.upsert_need_branch(&NeedBranchRow {
        need: NEED.into(),
        branch: "vivi/need/n1".into(),
        base: None,
        status: "live".into(),
        run: 2,
        target: "default".into(),
        priors: vec!["vivi/need-prior/n1-r1".into()],
    })?;

    let spec = start(&rig, "plan")?;

    assert!(
        spec.body
            .contains("prior need branch: vivi/need-prior/n1-r1"),
        "{}",
        spec.body
    );
    assert!(
        spec.body.contains("readable for reference"),
        "{}",
        spec.body
    );
    Ok(())
}

#[test]
fn a_missing_planner_role_is_an_error() -> R {
    let rig = rig(false, 1)?;

    let err = start(&rig, "plan").unwrap_err();

    assert!(err.to_string().contains(PUMP_PLANNER_ROLE), "{err}");
    Ok(())
}

#[test]
fn plan_and_plan2_budgets_follow_a_nondefault_dispatch_fallback() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 222;
    let rig = rig_with(cfg, true, 1)?;

    let plan = start(&rig, "plan")?;
    let plan2 = start(&rig, "plan2")?;

    // No `[pump.dispatch.step_budgets]` key: both planner nodes take the
    // changed shared fallback, exactly.
    assert_eq!(plan.budget, 222);
    assert_eq!(plan2.budget, 222);
    for spec in [&plan, &plan2] {
        assert!(spec.read_only);
        assert_eq!(spec.from_ref.as_deref(), Some(rig.ctx.cfg.main.as_str()));
        assert_eq!(spec.extra_targets, vec![String::from("default")]);
    }
    assert!(plan.body.contains("need: n1\nrun: r1\n"), "{}", plan.body);
    Ok(())
}

#[test]
fn plan_and_plan2_budgets_take_the_planner_override_not_another_kind() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 222;
    cfg.dispatch.step_budgets.planner = Some(333);
    // Another kind configured: it must not reach the planner nodes.
    cfg.dispatch.step_budgets.judge = Some(444);
    let rig = rig_with(cfg, true, 1)?;

    let plan = start(&rig, "plan")?;
    let plan2 = start(&rig, "plan2")?;

    assert_eq!(plan.budget, 333);
    assert_eq!(plan2.budget, 333);
    assert!(plan.read_only);
    assert_eq!(plan.from_ref.as_deref(), Some(rig.ctx.cfg.main.as_str()));
    assert_eq!(plan.extra_targets, vec![String::from("default")]);
    Ok(())
}

#[test]
fn shipped_registry_has_a_handler_for_plan_and_plan2() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let registry = shipped::<State, FakeMail>(&ctx);
    for id in ["plan", "plan2"] {
        let node = FlowNode {
            id: id.into(),
            class: NodeClass::Planner,
            label: String::new(),
        };
        assert!(
            registry.handler_for(FlowKind::Build, &node).is_some(),
            "{id}"
        );
    }
}
