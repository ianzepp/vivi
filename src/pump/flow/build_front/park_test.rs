//! Tests for the park handler.

use std::fs;

use tempfile::{TempDir, tempdir};

use super::{NEED_PARKED, ParkStep};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::{FlowDef, FlowKind};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::store::step_payload::StepPayloadRow;
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";

struct Rig {
    dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    run: RunRow,
    def: FlowDef,
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let run = RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 1,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    };
    store.insert_run(&run)?;
    Ok(Rig {
        dir,
        ctx,
        store,
        mail: FakeMail::new("pump"),
        run,
        def: load(FlowKind::Build)?,
    })
}

fn row(node: &str, task: &str, state: StepState, verdict: Option<&str>) -> StepRow {
    StepRow {
        run: 1,
        node: node.into(),
        task: task.into(),
        state,
        verdict: verdict.map(str::to_owned),
    }
}

fn payload(task: &str, node: &str, front: serde_json::Value) -> StepPayloadRow {
    StepPayloadRow {
        need: NEED.into(),
        run: 1,
        node: node.into(),
        class: "planner".into(),
        task: task.into(),
        payload: serde_json::json!({ "front": front }),
    }
}

fn start(rig: &Rig) -> Result<StartAction, VivariumError> {
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &rig.run,
        def: &rig.def,
    };
    ParkStep.start(&cx, &row("parked", "t-park", StepState::Running, None))
}

fn exceptions(rig: &Rig) -> String {
    fs::read_to_string(rig.dir.path().join("home").join("exceptions.txt")).unwrap_or_default()
}

fn count(text: &str) -> usize {
    text.lines()
        .filter(|line| line.contains(NEED_PARKED))
        .count()
}

#[test]
fn park_after_a_hold_alarms_once_with_the_word_and_reason() -> R {
    let rig = rig()?;
    rig.store
        .insert_step(NEED, &row("plan", "t-plan", StepState::Done, Some("hold")))?;
    rig.store
        .insert_step(NEED, &row("parked", "t-park", StepState::Running, None))?;
    rig.store.record_step_payload(&payload(
        "t-plan",
        "plan",
        serde_json::json!({ "reason": "waiting on the schema decision" }),
    ))?;
    assert_eq!(start(&rig)?, StartAction::OpenAndWait);
    assert_eq!(start(&rig)?, StartAction::OpenAndWait);
    let text = exceptions(&rig);
    assert_eq!(count(&text), 1, "{text}");
    for needle in [
        "need n1 r1 parked at parked",
        "expected the planner to answer planned",
        "actual: hold: waiting on the schema decision",
        "vivi pump answer t-park",
    ] {
        assert!(text.contains(needle), "missing {needle}: {text}");
    }
    Ok(())
}

#[test]
fn park_without_a_reason_says_none_given() -> R {
    let rig = rig()?;
    rig.store.insert_step(
        NEED,
        &row("plan", "t-plan", StepState::Done, Some("operator")),
    )?;
    assert_eq!(start(&rig)?, StartAction::OpenAndWait);
    let text = exceptions(&rig);
    assert!(text.contains("actual: operator: none given"), "{text}");
    Ok(())
}

#[test]
fn park_without_a_planner_step_still_parks_and_says_so() -> R {
    let rig = rig()?;
    assert_eq!(start(&rig)?, StartAction::OpenAndWait);
    let text = exceptions(&rig);
    assert_eq!(count(&text), 1, "{text}");
    assert!(text.contains("actual: no planner step found"), "{text}");
    Ok(())
}
