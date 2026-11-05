//! Tests for the slicer step handler (node `slice`).

use tempfile::{TempDir, tempdir};

use super::SliceStep;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::{FlowDef, FlowKind};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::{Registry, shipped};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::step_inputs::POOL_RESULT_MARKER;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::JobSpec;
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;
use vivi_error::VivariumError;
use vivi_mail::letter::roles::{Slicer, SlicerAnswer, SlicerMode, parse_role_reply};

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

/// A Done planner step `node` whose task holds one pool result with `plan`.
fn planner_done(rig: &mut Rig, node: &str, plan: Option<&str>) -> R {
    let task = rig.mail.send_step("pump", "pump", node, "body", &[])?;
    if let Some(plan) = plan {
        let text = format!(
            "{POOL_RESULT_MARKER}j-{node}: finished\npool p  slot 1  model m\n\n--- receipt\n{plan}"
        );
        rig.mail.reply(&task, &text)?;
    }
    rig.store
        .insert_step(NEED, &row(node, &task, StepState::Done))
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
    planner_done(&mut rig, "plan", Some("PLAN-ONE"))?;

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
    assert_slicer_footer(&spec.body, SlicerMode::Slice, "default");
    assert!(
        spec.body.find("admitted plan:\nPLAN-ONE").unwrap()
            < spec
                .body
                .find("The actual reply starts immediately")
                .unwrap(),
        "footer precedes the admitted plan: {}",
        spec.body
    );
    Ok(())
}

#[test]
fn configured_target_is_used_in_the_slice_example() -> R {
    let mut cfg = PumpConfig::default();
    cfg.targets.insert(
        String::from("sandbox"),
        crate::pump::config::PumpTarget {
            path: String::from("pump-sandbox"),
            main: String::from("vivi/main"),
            ..crate::pump::config::PumpTarget::default()
        },
    );
    let mut rig = rig_with(cfg)?;
    planner_done(&mut rig, "plan", Some("PLAN-ONE"))?;

    let spec = start(&rig)?;

    assert!(spec.body.contains("targets: sandbox"), "{}", spec.body);
    assert_slicer_footer(&spec.body, SlicerMode::Slice, "sandbox");
    Ok(())
}

fn assert_slicer_footer(body: &str, mode: SlicerMode, target: &str) {
    let footer = crate::pump::envelope::format::slicer(mode, target, "slice-n1-example");
    assert!(body.ends_with(&footer), "missing complete footer: {body}");
    let parsed: Vec<_> = envelopes(&footer)
        .iter()
        .filter_map(|example| parse_role_reply::<Slicer>(example).ok())
        .collect();
    assert_eq!(parsed.len(), 2, "{footer}");
    let sliced = &parsed[0].front;
    assert_eq!(sliced.answer, SlicerAnswer::Sliced);
    assert_eq!(sliced.mode, mode);
    assert_eq!(sliced.slice.len(), 1);
    assert_eq!(sliced.slice[0].id, "slice-n1-example");
    assert_eq!(sliced.slice[0].target, target);
    assert!(!sliced.slice[0].title.trim().is_empty());
    assert!(!sliced.slice[0].brief.trim().is_empty());
    assert!(!sliced.slice[0].write_scope.is_empty());
    assert!(sliced.slice[0].deps.is_empty());
    assert_eq!(parsed[1].front.answer, SlicerAnswer::Escalate);
    assert_eq!(parsed[1].front.mode, mode);
}

fn envelopes(text: &str) -> Vec<String> {
    let mut inside = false;
    let mut current = String::new();
    let mut found = Vec::new();
    for line in text.lines() {
        if line == "+++" {
            if inside {
                found.push(format!("+++\n{}\n+++", current.trim_end()));
                current.clear();
            }
            inside = !inside;
        } else if inside {
            current.push_str(line);
            current.push('\n');
        }
    }
    found
}

#[test]
fn configured_minutes_set_the_slice_size() -> R {
    let mut cfg = PumpConfig::default();
    cfg.slice.minutes = Some(6);
    let mut rig = rig_with(cfg)?;
    planner_done(&mut rig, "plan", Some("PLAN-ONE"))?;

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
    planner_done(&mut rig, "plan", Some("PLAN-ONE"))?;

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
    planner_done(&mut rig, "plan", Some("PLAN-ONE"))?;

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
    planner_done(&mut rig, "plan", Some("PLAN-ONE"))?;
    planner_done(&mut rig, "plan2", Some("PLAN-TWO"))?;

    let spec = start(&rig)?;

    assert!(spec.body.contains("PLAN-TWO"), "{}", spec.body);
    assert!(!spec.body.contains("PLAN-ONE"), "{}", spec.body);
    Ok(())
}

#[test]
fn no_done_planner_step_is_an_error() -> R {
    let rig = rig()?;

    let err = start(&rig).unwrap_err();

    assert!(err.to_string().contains(NEED), "{err}");
    Ok(())
}

#[test]
fn a_planner_step_without_a_reply_is_an_error() -> R {
    let mut rig = rig()?;
    planner_done(&mut rig, "plan", None)?;

    let err = start(&rig).unwrap_err();

    assert!(err.to_string().contains("no reply"), "{err}");
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

/// A run that a head-cto `slice` answer started (2026-11-04 local pump, need
/// 4c693111 r4) has no planner step of its own: it slices the earlier plan.
#[test]
fn a_run_restarted_at_slice_slices_the_earlier_runs_plan() -> R {
    let mut rig = rig()?;
    planner_done(&mut rig, "plan", Some("PLAN-FROM-RUN-ONE"))?;
    let restarted = RunRow {
        run: 2,
        entry: "slice".into(),
        ..rig.run.clone()
    };
    rig.store.insert_run(&restarted)?;
    rig.run = restarted;

    let spec = start(&rig)?;

    assert!(spec.body.contains("PLAN-FROM-RUN-ONE"), "{}", spec.body);
    assert!(spec.body.contains("run: 2"), "{}", spec.body);
    Ok(())
}

#[test]
fn this_runs_plan_wins_over_an_earlier_runs_plan() -> R {
    let mut rig = rig()?;
    planner_done(&mut rig, "plan", Some("OLD-PLAN"))?;
    let replanned = RunRow {
        run: 2,
        entry: "plan".into(),
        ..rig.run.clone()
    };
    rig.store.insert_run(&replanned)?;
    rig.run = replanned;
    let task = rig.mail.send_step("pump", "pump", "plan", "body", &[])?;
    rig.mail.reply(
        &task,
        &format!(
            "{POOL_RESULT_MARKER}j: finished\npool p  slot 1  model m\n\n--- receipt\nNEW-PLAN"
        ),
    )?;
    rig.store.insert_step(
        NEED,
        &StepRow {
            run: 2,
            ..row("plan", &task, StepState::Done)
        },
    )?;

    let spec = start(&rig)?;

    assert!(spec.body.contains("NEW-PLAN"), "{}", spec.body);
    assert!(!spec.body.contains("OLD-PLAN"), "{}", spec.body);
    Ok(())
}
