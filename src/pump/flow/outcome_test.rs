//! Tests for `role_job`: the typed execution kind decides the budget, never
//! the durable kind label or the role seat.

use tempfile::{TempDir, tempdir};

use super::role_job;
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::envelope::roles::RoleKind;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::host::{Ctx, test_ctx};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    run: RunRow,
}

fn rig_with(cfg: PumpConfig) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    let run = RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    };
    Ok(Rig {
        _dir: dir,
        ctx,
        run,
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

#[test]
fn a_judge_through_the_auditor_seat_budgets_as_a_judge() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.step_budgets.judge = Some(45);
    cfg.dispatch.step_budgets.auditor = Some(999);
    let rig = rig_with(cfg)?;
    let step = row("judge1", "t1", StepState::Running);

    let spec = role_job(
        &rig.ctx,
        &rig.run,
        &step,
        "flow-judge",
        RoleKind::Judge,
        "auditor",
        "b",
    );

    assert_eq!(spec.budget, 45);
    Ok(())
}

#[test]
fn rework_budgets_as_a_slicer_despite_its_label() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.step_budgets.slicer = Some(61);
    let rig = rig_with(cfg)?;
    let step = row("rework1", "t2", StepState::Running);

    let spec = role_job(
        &rig.ctx,
        &rig.run,
        &step,
        "flow-rework",
        RoleKind::Slicer,
        "slicer",
        "b",
    );

    assert_eq!(spec.budget, 61);
    Ok(())
}

#[test]
fn an_omitted_override_uses_the_changed_shared_budget() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 777;
    let rig = rig_with(cfg)?;
    let step = row("audit1", "t3", StepState::Running);

    // A thinking kind whose override is absent and a kind with no thinking
    // key both land on the shared budget, changed from the default.
    let auditor = role_job(
        &rig.ctx,
        &rig.run,
        &step,
        "flow-audit",
        RoleKind::Auditor,
        "auditor",
        "b",
    );
    let merge = role_job(
        &rig.ctx,
        &rig.run,
        &step,
        "sync",
        RoleKind::Merge,
        "merge",
        "b",
    );

    assert_eq!(auditor.budget, 777);
    assert_eq!(merge.budget, 777);
    Ok(())
}

#[test]
fn merge_stays_outside_the_overrides() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 777;
    cfg.dispatch.step_budgets.planner = Some(90);
    cfg.dispatch.step_budgets.slicer = Some(61);
    cfg.dispatch.step_budgets.judge = Some(45);
    let rig = rig_with(cfg)?;
    let step = row("sync1", "t4", StepState::Running);

    let merge = role_job(
        &rig.ctx,
        &rig.run,
        &step,
        "sync",
        RoleKind::Merge,
        "merge",
        "b",
    );

    assert_eq!(merge.budget, 777);
    Ok(())
}

#[test]
fn role_job_keeps_the_label_task_identity_and_step_id() -> R {
    let rig = rig()?;
    let step = row("plan", "t9", StepState::Running);

    let spec = role_job(
        &rig.ctx,
        &rig.run,
        &step,
        "flow-rework",
        RoleKind::Slicer,
        "slicer",
        "the brief",
    );

    assert_eq!(spec.id, "step-t9-a1");
    assert_eq!(spec.kind, "flow-rework");
    assert_eq!(spec.role, "slicer");
    assert_eq!(spec.task.as_deref(), Some("t9"));
    assert_eq!(spec.budget, rig.ctx.cfg.dispatch.budget);
    Ok(())
}
