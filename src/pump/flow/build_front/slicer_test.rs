//! Tests for the slicer step handler (node `slice`).

use tempfile::{TempDir, tempdir};

use super::SliceStep;
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::{FlowDef, FlowKind};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::{Registry, shipped};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::JobSpec;
use crate::pump::state::State;
use crate::pump::store::step_payload::StepPayloadRow;
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    run: RunRow,
    def: FlowDef,
}

fn rig_with(cfg: PumpConfig) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("slicer"),
        kind: Some(String::from("slicer")),
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
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail: FakeMail::new("pump"),
        run,
        def: load(FlowKind::Build)?,
    })
}

fn rig() -> Result<Rig, VivariumError> {
    rig_with(PumpConfig::default())
}

fn row(node: &str, task: &str, state: StepState) -> StepRow {
    StepRow {
        run: 1,
        node: node.into(),
        task: task.into(),
        state,
        verdict: None,
    }
}

/// A done planner step `node` whose stored payload carries `plan`.
fn planner_done(rig: &mut Rig, node: &str, plan: &str) -> R {
    let task = format!("t-{node}");
    rig.store.record_step_payload(&StepPayloadRow {
        need: NEED.into(),
        run: 1,
        node: node.into(),
        class: "planner".into(),
        task: task.clone(),
        payload: serde_json::json!({
            "front": { "answer": "planned" },
            "body": plan,
        }),
    })?;
    rig.store.insert_step(
        NEED,
        &StepRow {
            verdict: Some("planned".into()),
            ..row(node, &task, StepState::Done)
        },
    )
}

/// A done admitter step `node` answering `verdict`.
fn admitter_done(rig: &mut Rig, node: &str, verdict: &str) -> R {
    rig.store.insert_step(
        NEED,
        &StepRow {
            verdict: Some(verdict.into()),
            ..row(node, &format!("t-{node}"), StepState::Done)
        },
    )
}

/// The plain admission of a first plan: `plan` planned, `admit1` admitted.
fn admitted_first_plan(rig: &mut Rig) -> R {
    planner_done(rig, "plan", "PLAN-ONE")?;
    admitter_done(rig, "admit1", "admitted")
}

fn start(rig: &Rig) -> Result<JobSpec, VivariumError> {
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &rig.run,
        def: &rig.def,
    };
    match SliceStep.start(&cx, &row("slice", "ts", StepState::Waiting))? {
        StartAction::Job(spec) => Ok(*spec),
        other => Err(VivariumError::Other(format!("not a job: {other:?}"))),
    }
}

#[test]
fn start_is_a_read_only_slicer_job_with_the_plan_and_default_minutes() -> R {
    let mut rig = rig()?;
    admitted_first_plan(&mut rig)?;

    let spec = start(&rig)?;

    assert_eq!(spec.role, "slicer");
    assert!(spec.read_only);
    assert_eq!(spec.task.as_deref(), Some("ts"));
    assert!(spec.body.starts_with("mode: slice\n"), "{}", spec.body);
    assert!(spec.body.contains("need: n1\nrun: 1\n"), "{}", spec.body);
    assert!(spec.body.contains("targets: default"), "{}", spec.body);
    assert!(
        spec.body
            .contains("slice size: aim for 5 to 10 minutes per slice"),
        "{}",
        spec.body
    );
    assert!(
        spec.body.contains("admitted plan:\nPLAN-ONE"),
        "{}",
        spec.body
    );
    Ok(())
}

#[test]
fn configured_minutes_set_the_slice_size() -> R {
    let mut cfg = PumpConfig::default();
    cfg.slice.minutes = Some(6);
    let mut rig = rig_with(cfg)?;
    admitted_first_plan(&mut rig)?;

    let spec = start(&rig)?;

    assert!(
        spec.body
            .contains("slice size: aim for 5 to 6 minutes per slice"),
        "{}",
        spec.body
    );
    Ok(())
}

#[test]
fn slice_budgets_follow_a_nondefault_dispatch_fallback() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 222;
    let mut rig = rig_with(cfg)?;
    admitted_first_plan(&mut rig)?;

    let spec = start(&rig)?;

    // No `[pump.dispatch.step_budgets]` key: the slice node takes the changed
    // shared fallback, exactly.
    assert_eq!(spec.budget, 222);
    assert!(spec.read_only);
    assert_eq!(spec.from_ref.as_deref(), Some(rig.ctx.cfg.main.as_str()));
    assert_eq!(spec.extra_targets, vec![String::from("default")]);
    // The sizing guidance stays the ten-minute default; the budget is not a
    // sizing input and the minutes are not a hard stop.
    assert_eq!(rig.ctx.cfg.slice.minutes(), 10);
    assert!(
        spec.body
            .contains("slice size: aim for 5 to 10 minutes per slice"),
        "{}",
        spec.body
    );
    Ok(())
}

#[test]
fn slice_budget_overrides_do_not_touch_the_slice_size_guidance() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 222;
    cfg.dispatch.step_budgets.slicer = Some(77);
    // Other kinds configured: none of them may reach the slice node.
    cfg.dispatch.step_budgets.judge = Some(88);
    cfg.dispatch.step_budgets.planner = Some(66);
    let mut rig = rig_with(cfg)?;
    admitted_first_plan(&mut rig)?;

    let spec = start(&rig)?;

    assert_eq!(spec.budget, 77);
    assert!(spec.read_only);
    assert_eq!(spec.from_ref.as_deref(), Some(rig.ctx.cfg.main.as_str()));
    assert_eq!(spec.extra_targets, vec![String::from("default")]);
    // Execution budgets never move `SliceConfig::minutes()` or the brief's
    // default "aim for 5 to 10 minutes" guidance.
    assert_eq!(rig.ctx.cfg.slice.minutes(), 10);
    assert!(
        spec.body
            .contains("slice size: aim for 5 to 10 minutes per slice"),
        "{}",
        spec.body
    );
    Ok(())
}

#[test]
fn the_revised_plan_wins_over_the_first() -> R {
    let mut rig = rig()?;
    planner_done(&mut rig, "plan", "PLAN-ONE")?;
    admitter_done(&mut rig, "admit1", "revise")?;
    planner_done(&mut rig, "plan2", "PLAN-TWO")?;
    admitter_done(&mut rig, "admit2", "admitted")?;

    let spec = start(&rig)?;

    assert!(spec.body.contains("PLAN-TWO"), "{}", spec.body);
    assert!(!spec.body.contains("PLAN-ONE"), "{}", spec.body);
    Ok(())
}

#[test]
fn no_admitted_plan_is_rejected() -> R {
    let rig = rig()?;

    let err = start(&rig).unwrap_err();

    assert!(
        err.to_string().contains("no admitted plan to slice"),
        "{err}"
    );
    assert!(err.to_string().contains(NEED), "{err}");
    Ok(())
}

#[test]
fn an_admission_without_a_stored_plan_body_is_an_error() -> R {
    let mut rig = rig()?;
    // The planner step and the admission exist, but the planner stored no
    // payload: the referenced material is missing, not merely absent.
    rig.store.insert_step(
        NEED,
        &StepRow {
            verdict: Some("planned".into()),
            ..row("plan", "t-plan", StepState::Done)
        },
    )?;
    admitter_done(&mut rig, "admit1", "admitted")?;

    let err = start(&rig).unwrap_err();

    assert!(err.to_string().contains("no stored payload"), "{err}");
    Ok(())
}

#[test]
fn shipped_registers_slice_and_the_rework_step_override_for_rework1() -> R {
    let rig = rig()?;
    let registry: Registry<State, FakeMail> = shipped(&rig.ctx);
    let node = |id: &str| {
        rig.def
            .nodes
            .iter()
            .find(|n| n.id == id)
            .ok_or_else(|| VivariumError::Other(format!("no node {id}")))
    };

    assert!(
        registry
            .handler_for(FlowKind::Build, node("slice")?)
            .is_some()
    );
    // rework1 has N5's rework_step node override, not this slicer's handler.
    assert!(
        registry
            .handler_for(FlowKind::Build, node("rework1")?)
            .is_some()
    );
    Ok(())
}
