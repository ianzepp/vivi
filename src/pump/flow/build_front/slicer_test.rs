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

/// A completed planner step whose stored payload body is `document`, and a
/// completed admitter that admitted it, both in run `run`.
fn admission(
    rig: &Rig,
    run: u32,
    plan_node: &str,
    admit_node: &str,
    plan_task: &str,
    admit_task: &str,
    document: &str,
) -> R {
    rig.store.insert_step(
        NEED,
        &StepRow {
            run,
            node: plan_node.into(),
            task: plan_task.into(),
            state: StepState::Done,
            verdict: Some(String::from("planned")),
        },
    )?;
    rig.store.insert_step(
        NEED,
        &StepRow {
            run,
            node: admit_node.into(),
            task: admit_task.into(),
            state: StepState::Done,
            verdict: Some(String::from("admitted")),
        },
    )?;
    rig.store.record_step_payload(&StepPayloadRow {
        need: NEED.into(),
        run,
        node: plan_node.into(),
        class: String::from("planner"),
        task: plan_task.into(),
        payload: serde_json::json!({ "body": document }),
    })?;
    Ok(())
}

/// The admitted plan of run 1, sliced by a run 2 that entered at `slice`.
fn restarted_rig() -> Result<Rig, VivariumError> {
    let rig = rig()?;
    admission(&rig, 1, "plan", "admit1", "t-plan", "t-admit", "PLAN-ONE")?;
    let mut run = rig.run.clone();
    run.run = 2;
    run.entry = String::from("slice");
    Ok(Rig { run, ..rig })
}

fn start(rig: &Rig) -> Result<JobSpec, VivariumError> {
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &rig.run,
        def: &rig.def,
    };
    let step = if rig.run.run == 1 {
        row("slice", "ts", StepState::Waiting)
    } else {
        StepRow {
            run: rig.run.run,
            ..row("slice", "ts", StepState::Waiting)
        }
    };
    match SliceStep.start(&cx, &step)? {
        StartAction::Job(spec) => Ok(*spec),
        other => Err(VivariumError::Other(format!("not a job: {other:?}"))),
    }
}

#[test]
fn start_is_a_read_only_slicer_job_with_the_plan_and_default_minutes() -> R {
    let rig = rig()?;
    admission(&rig, 1, "plan", "admit1", "t-plan", "t-admit", "PLAN-ONE")?;

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
    // The provenance is frozen into the brief next to the document.
    assert!(
        spec.body
            .contains("admitted plan source: run 1 planner task t-plan admitter task t-admit"),
        "{}",
        spec.body
    );
    Ok(())
}

#[test]
fn the_job_payload_freezes_the_admitted_plan_provenance() -> R {
    let rig = rig()?;
    admission(&rig, 1, "plan", "admit1", "t-plan", "t-admit", "PLAN-ONE")?;

    let spec = start(&rig)?;

    assert_eq!(
        spec.payload,
        serde_json::json!({ "admitted_plan": {
            "run": 1, "planner": "t-plan", "admitter": "t-admit",
        } })
    );
    Ok(())
}

#[test]
fn configured_minutes_set_the_slice_size() -> R {
    let mut cfg = PumpConfig::default();
    cfg.slice.minutes = Some(6);
    let rig = rig_with(cfg)?;
    admission(&rig, 1, "plan", "admit1", "t-plan", "t-admit", "PLAN-ONE")?;

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
    let rig = rig_with(cfg)?;
    admission(&rig, 1, "plan", "admit1", "t-plan", "t-admit", "PLAN-ONE")?;

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
    let rig = rig_with(cfg)?;
    admission(&rig, 1, "plan", "admit1", "t-plan", "t-admit", "PLAN-ONE")?;

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
    let rig = rig()?;
    admission(&rig, 1, "plan", "admit1", "t-plan", "t-admit", "PLAN-ONE")?;
    admission(
        &rig, 1, "plan2", "admit2", "t-plan2", "t-admit2", "PLAN-TWO",
    )?;

    let spec = start(&rig)?;

    assert!(spec.body.contains("PLAN-TWO"), "{}", spec.body);
    assert!(!spec.body.contains("PLAN-ONE"), "{}", spec.body);
    assert!(
        spec.body
            .contains("planner task t-plan2 admitter task t-admit2"),
        "{}",
        spec.body
    );
    Ok(())
}

#[test]
fn unadmitted_newer_plan_does_not_replace_admitted_source() -> R {
    let rig = rig()?;
    admission(&rig, 1, "plan", "admit1", "t-plan", "t-admit", "PLAN-ONE")?;
    // A later revision that planned but was never admitted.
    rig.store.insert_step(
        NEED,
        &StepRow {
            node: "plan2".into(),
            task: "t-plan2".into(),
            state: StepState::Done,
            verdict: Some(String::from("planned")),
            ..row("plan2", "t-plan2", StepState::Done)
        },
    )?;
    rig.store.record_step_payload(&StepPayloadRow {
        need: NEED.into(),
        run: 1,
        node: "plan2".into(),
        class: "planner".into(),
        task: "t-plan2".into(),
        payload: serde_json::json!({ "body": "PLAN-TWO" }),
    })?;

    let spec = start(&rig)?;

    assert!(spec.body.contains("PLAN-ONE"), "{}", spec.body);
    assert!(!spec.body.contains("PLAN-TWO"), "{}", spec.body);
    Ok(())
}

#[test]
fn slice_restart_uses_admitted_revision() -> R {
    // Run 1 ended admitting its plan revision; run 2 entered at slice and has
    // no planner or admitter of its own.
    let rig = restarted_rig()?;

    let spec = start(&rig)?;

    assert!(spec.body.contains("PLAN-ONE"), "{}", spec.body);
    assert!(
        spec.body
            .contains("admitted plan source: run 1 planner task t-plan admitter task t-admit"),
        "{}",
        spec.body
    );
    assert_eq!(spec.payload["admitted_plan"]["run"], 1, "{}", spec.payload);
    Ok(())
}

#[test]
fn no_admitted_plan_is_an_error_naming_the_need() -> R {
    let rig = rig()?;

    let err = start(&rig).unwrap_err();

    assert!(err.to_string().contains(NEED), "{err}");
    Ok(())
}

#[test]
fn a_planner_without_an_admitter_is_not_a_sliceable_source() -> R {
    let rig = rig()?;
    admission(&rig, 1, "plan", "admit1", "t-plan", "t-admit", "PLAN-ONE")?;
    // A fresh plan that planned after the admission has no admitter yet.
    rig.store.insert_step(
        NEED,
        &StepRow {
            node: "plan2".into(),
            task: "t-plan2".into(),
            state: StepState::Done,
            verdict: Some(String::from("planned")),
            ..row("plan2", "t-plan2", StepState::Done)
        },
    )?;

    let spec = start(&rig)?;

    assert!(spec.body.contains("PLAN-ONE"), "{}", spec.body);
    Ok(())
}

#[test]
fn an_admission_without_a_stored_planner_payload_is_an_error() -> R {
    let rig = rig()?;
    rig.store.insert_step(
        NEED,
        &StepRow {
            node: "plan".into(),
            task: "t-plan".into(),
            state: StepState::Done,
            verdict: Some(String::from("planned")),
            ..row("plan", "t-plan", StepState::Done)
        },
    )?;
    rig.store.insert_step(
        NEED,
        &StepRow {
            node: "admit1".into(),
            task: "t-admit".into(),
            state: StepState::Done,
            verdict: Some(String::from("admitted")),
            ..row("admit1", "t-admit", StepState::Done)
        },
    )?;

    let err = start(&rig).unwrap_err();

    assert!(err.to_string().contains("t-plan"), "{err}");
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
