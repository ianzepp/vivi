//! Tests for the judge step handler.

use serde_json::json;
use tempfile::{TempDir, tempdir};

use super::JudgeStart;
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::store::step_payload::StepPayloadRow;
use crate::pump::store::{DB_FILENAME, PumpStore};

const NEED: &str = "n1";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    run: RunRow,
}

fn rig() -> Result<Rig, VivariumError> {
    rig_with(PumpConfig::default())
}

/// A rig over `cfg`, so tests can shape the dispatch budgets.
fn rig_with(cfg: PumpConfig) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail: FakeMail::new("mind"),
        run: RunRow {
            need: NEED.into(),
            run: 1,
            flow: "build".into(),
            flow_version: 2,
            kind: "build".into(),
            entry: "plan".into(),
            state: "live".into(),
        },
    })
}

fn store_row(rig: &Rig, class: &str, payload: serde_json::Value) -> Result<(), VivariumError> {
    rig.store.record_step_payload(&StepPayloadRow {
        need: NEED.into(),
        run: 1,
        node: class.into(),
        class: class.into(),
        task: format!("t-{class}"),
        payload,
    })
}

fn start(rig: &Rig) -> Result<StartAction, VivariumError> {
    let def = load(FlowKind::Build)?;
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &rig.run,
        def: &def,
    };
    let step = StepRow {
        run: 1,
        node: "judge".into(),
        task: "t-judge".into(),
        state: StepState::Waiting,
        verdict: None,
    };
    JudgeStart.start(&cx, &step)
}

#[test]
fn judge_job_is_a_read_only_auditor_with_audit_and_rung() -> Result<(), VivariumError> {
    let rig = rig()?;
    store_row(
        &rig,
        "auditor",
        json!({"front": {"answer": "defects"}, "body": "AUDIT-ANSWER-TEXT"}),
    )?;
    store_row(
        &rig,
        "rung-need",
        json!({"exit": 101, "tail": "boom", "failing": ["vivi::pump::flow::a_test"]}),
    )?;
    let StartAction::Job(spec) = start(&rig)? else {
        return Err(VivariumError::Other("not a job".into()));
    };
    assert_eq!(spec.role, "auditor");
    assert!(spec.read_only);
    assert_eq!(spec.from_ref.as_deref(), Some(rig.ctx.cfg.main.as_str()));
    assert_eq!(
        spec.extra_targets,
        crate::pump::flow::step_inputs::targets(&rig.ctx)
    );
    assert_eq!(spec.builtin_role.as_deref(), Some("auditor"));
    assert!(spec.body.starts_with("Judge mode\n"), "{}", spec.body);
    assert!(spec.body.contains(NEED));
    assert!(spec.body.contains("AUDIT-ANSWER-TEXT"));
    assert!(spec.body.contains("vivi::pump::flow::a_test"));
    assert!(spec.body.contains("exit: 101"));
    Ok(())
}

/// An auditor row and a need-rung row, the two rows a judge start reads.
fn store_audit_and_rung(rig: &Rig) -> Result<(), VivariumError> {
    store_row(
        rig,
        "auditor",
        json!({"front": {"answer": "defects"}, "body": "AUDIT-ANSWER-TEXT"}),
    )?;
    store_row(rig, "rung-need", json!({"exit": 0, "failing": []}))
}

#[test]
fn a_judge_budget_override_reaches_the_judge_job_not_the_auditor_seat() -> Result<(), VivariumError>
{
    let mut cfg = PumpConfig::default();
    cfg.dispatch.step_budgets.judge = Some(333);
    // The auditor seat carries a different override: budgeting from the seat
    // would put 444 on the job.
    cfg.dispatch.step_budgets.auditor = Some(444);
    cfg.dispatch.step_budgets.planner = Some(11);
    let rig = rig_with(cfg)?;
    store_audit_and_rung(&rig)?;
    let StartAction::Job(spec) = start(&rig)? else {
        return Err(VivariumError::Other("not a job".into()));
    };
    assert_eq!(spec.budget, 333);
    Ok(())
}

#[test]
fn an_absent_judge_override_uses_the_dispatch_fallback_despite_an_auditor_override()
-> Result<(), VivariumError> {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 4242;
    cfg.dispatch.step_budgets.auditor = Some(444);
    cfg.dispatch.step_budgets.planner = Some(11);
    let rig = rig_with(cfg)?;
    store_audit_and_rung(&rig)?;
    let StartAction::Job(spec) = start(&rig)? else {
        return Err(VivariumError::Other("not a job".into()));
    };
    assert_eq!(spec.budget, 4242);
    Ok(())
}

#[test]
fn judge_without_a_rung_row_errors() -> Result<(), VivariumError> {
    let rig = rig()?;
    store_row(&rig, "auditor", json!({"front": {}, "body": "audit"}))?;
    let Err(error) = start(&rig) else {
        return Err(VivariumError::Other("start succeeded".into()));
    };
    assert!(error.to_string().contains(NEED), "{error}");
    Ok(())
}

#[test]
fn judge_without_an_audit_row_errors() -> Result<(), VivariumError> {
    let rig = rig()?;
    store_row(&rig, "rung-need", json!({"exit": 1, "failing": []}))?;
    assert!(start(&rig).is_err());
    Ok(())
}
