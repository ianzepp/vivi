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

impl Rig {
    /// Point the rig's run at an existing run row.
    fn aim_at(&mut self, number: u32, entry: &str) {
        self.run.run = number;
        self.run.entry = entry.into();
    }

    /// Add run `number` entering at `entry` and point the rig at it.
    fn continue_with(&mut self, number: u32, entry: &str) -> R {
        let run = RunRow {
            need: NEED.into(),
            run: number,
            flow: "build".into(),
            flow_version: 2,
            kind: "build".into(),
            entry: entry.into(),
            state: "live".into(),
        };
        self.store.insert_run(&run)?;
        self.aim_at(number, entry);
        Ok(())
    }
}

fn row(node: &str, task: &str, state: StepState) -> StepRow {
    row_in(1, node, task, state)
}

fn row_in(run: u32, node: &str, task: &str, state: StepState) -> StepRow {
    StepRow {
        run,
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

/// A done planner step `node` of `run` whose stored payload carries `plan`.
fn planner_done_in(rig: &mut Rig, run: u32, node: &str, plan: &str) -> R {
    let task = format!("t-r{run}-{node}");
    rig.store.record_step_payload(&StepPayloadRow {
        need: NEED.into(),
        run,
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
            ..row_in(run, node, &task, StepState::Done)
        },
    )
}

/// A done admitter step `node` of `run` answering `verdict`.
fn admitter_done_in(rig: &mut Rig, run: u32, node: &str, verdict: &str) -> R {
    rig.store.insert_step(
        NEED,
        &StepRow {
            verdict: Some(verdict.into()),
            ..row_in(run, node, &format!("t-r{run}-{node}"), StepState::Done)
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
fn slice_continuation_uses_previous_admitted_plan() -> R {
    // The first admission travels to a later slice run.
    {
        let mut rig = rig()?;
        admitted_first_plan(&mut rig)?;
        rig.continue_with(2, "slice")?;

        let spec = start(&rig)?;

        assert!(
            spec.body.contains("admitted plan:\nPLAN-ONE"),
            "{}",
            spec.body
        );
        assert!(spec.body.contains("need: n1\nrun: 2\n"), "{}", spec.body);
    }

    // The revised admission travels instead of the first plan.
    {
        let mut rig = rig()?;
        planner_done(&mut rig, "plan", "PLAN-ONE")?;
        admitter_done(&mut rig, "admit1", "revise")?;
        planner_done(&mut rig, "plan2", "PLAN-TWO")?;
        admitter_done(&mut rig, "admit2", "admitted")?;
        rig.continue_with(2, "slice")?;

        let spec = start(&rig)?;

        assert!(
            spec.body.contains("admitted plan:\nPLAN-TWO"),
            "{}",
            spec.body
        );
        assert!(!spec.body.contains("PLAN-ONE"), "{}", spec.body);
    }
    Ok(())
}

#[test]
fn newer_unadmitted_plan_does_not_replace_admitted_plan() -> R {
    let mut rig = rig()?;
    admitted_first_plan(&mut rig)?;
    // plan2 is planned, but no admitter admitted the revision.
    planner_done(&mut rig, "plan2", "PLAN-TWO")?;
    rig.continue_with(2, "slice")?;

    let spec = start(&rig)?;

    assert!(
        spec.body.contains("admitted plan:\nPLAN-ONE"),
        "{}",
        spec.body
    );
    assert!(!spec.body.contains("PLAN-TWO"), "{}", spec.body);
    Ok(())
}

#[test]
fn plan_inheritance_respects_need_run_and_replan_boundaries() -> R {
    let mut rig = rig()?;
    admitted_first_plan(&mut rig)?;

    // Repeated slice continuations and a rework1 entry inherit run 1's plan.
    for (number, entry) in [(2, "slice"), (3, "slice"), (4, "rework1")] {
        rig.continue_with(number, entry)?;
        let spec = start(&rig)?;
        assert!(
            spec.body.contains("admitted plan:\nPLAN-ONE"),
            "r{number}: {}",
            spec.body
        );
    }

    // A run with its own admission prefers it; the next slice continues there.
    rig.continue_with(5, "plan")?;
    planner_done_in(&mut rig, 5, "plan", "PLAN-FIVE")?;
    admitter_done_in(&mut rig, 5, "admit1", "admitted")?;
    let spec = start(&rig)?;
    assert!(
        spec.body.contains("admitted plan:\nPLAN-FIVE"),
        "{}",
        spec.body
    );
    rig.continue_with(6, "slice")?;
    let spec = start(&rig)?;
    assert!(
        spec.body.contains("admitted plan:\nPLAN-FIVE"),
        "{}",
        spec.body
    );

    // A future run's admission is never read: run 2 predates run 5.
    rig.aim_at(2, "slice");
    let spec = start(&rig)?;
    assert!(
        spec.body.contains("admitted plan:\nPLAN-ONE"),
        "{}",
        spec.body
    );
    assert!(!spec.body.contains("PLAN-FIVE"), "{}", spec.body);

    // Another need's admission is never read, and a fresh unadmitted plan or
    // research run ends the inheritance: the slicer rejects, naming the run.
    seed_other_need(&rig)?;
    for (number, entry) in [(7, "plan"), (8, "slice"), (9, "research")] {
        rig.continue_with(number, entry)?;
        let err = start(&rig).unwrap_err();
        assert!(
            err.to_string().contains("no admitted plan to slice"),
            "r{number} at {entry}: {err}"
        );
        assert!(
            err.to_string().contains(&format!("{NEED} run {number}")),
            "r{number}: {err}"
        );
    }
    Ok(())
}

/// Another need whose run 1 admitted its own plan; it must never be read.
fn seed_other_need(rig: &Rig) -> R {
    let mut run = rig.run.clone();
    run.need = "need-2".into();
    run.run = 1;
    run.entry = "plan".into();
    rig.store.insert_run(&run)?;
    rig.store.record_step_payload(&StepPayloadRow {
        need: run.need.clone(),
        run: 1,
        node: "plan".into(),
        class: "planner".into(),
        task: "other-plan".into(),
        payload: serde_json::json!({
            "front": { "answer": "planned" },
            "body": "OTHER PLAN",
        }),
    })?;
    rig.store.insert_step(
        "need-2",
        &StepRow {
            verdict: Some("planned".into()),
            ..row_in(1, "plan", "other-plan", StepState::Done)
        },
    )?;
    rig.store.insert_step(
        "need-2",
        &StepRow {
            verdict: Some("admitted".into()),
            ..row_in(1, "admit1", "other-admit", StepState::Done)
        },
    )
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
