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
use crate::pump::store::{ContinuationLink, DB_FILENAME, PumpStore};

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
        flow_version: 2,
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

fn cx(rig: &Rig) -> StartCx<'_> {
    StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &rig.run,
        def: &rig.def,
    }
}

/// The router opens a park row Waiting, so the tick reaches it through `poll`.
fn poll(rig: &Rig) -> Result<Option<String>, VivariumError> {
    ParkStep.poll(&cx(rig), &row("parked", "t-park", StepState::Waiting, None))
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
        .insert_step(NEED, &row("parked", "t-park", StepState::Waiting, None))?;
    rig.store.record_step_payload(&payload(
        "t-plan",
        "plan",
        serde_json::json!({ "reason": "waiting on the schema decision" }),
    ))?;
    assert_eq!(poll(&rig)?, None);
    assert_eq!(poll(&rig)?, None);
    let text = exceptions(&rig);
    assert_eq!(count(&text), 1, "{text}");
    for needle in [
        "need n1 r1 parked at parked",
        "actual: t-plan answered hold: waiting on the schema decision",
        "supplies new input to that same step",
        "vivi pump answer t-park",
    ] {
        assert!(text.contains(needle), "missing {needle}: {text}");
    }
    assert!(!text.contains("restart"), "no restart claim: {text}");
    assert!(
        !text.contains("at plan"),
        "no restart-at-plan claim: {text}"
    );
    Ok(())
}

#[test]
fn park_without_a_reason_says_none_given() -> R {
    let rig = rig()?;
    rig.store.insert_step(
        NEED,
        &row("plan", "t-plan", StepState::Done, Some("operator")),
    )?;
    assert_eq!(poll(&rig)?, None);
    let text = exceptions(&rig);
    assert!(
        text.contains("actual: t-plan answered operator: none given"),
        "{text}"
    );
    Ok(())
}

#[test]
fn polling_twice_raises_the_alarm_once() -> R {
    let rig = rig()?;
    rig.store
        .insert_step(NEED, &row("plan", "t-plan", StepState::Done, Some("hold")))?;
    for _ in 0..2 {
        assert_eq!(poll(&rig)?, None);
    }
    let text = exceptions(&rig);
    assert_eq!(count(&text), 1, "{text}");
    Ok(())
}

#[test]
fn start_opens_the_row_and_waits_without_an_alarm() -> R {
    let rig = rig()?;
    let step = row("parked", "t-park", StepState::Pending, None);
    assert_eq!(ParkStep.start(&cx(&rig), &step)?, StartAction::OpenAndWait);
    assert_eq!(count(&exceptions(&rig)), 0);
    Ok(())
}

#[test]
fn park_without_resolvable_evidence_still_parks_and_says_so() -> R {
    let rig = rig()?;
    assert_eq!(poll(&rig)?, None);
    let text = exceptions(&rig);
    assert_eq!(count(&text), 1, "{text}");
    assert!(
        text.contains("actual: the stopped step is unresolved"),
        "{text}"
    );
    Ok(())
}

#[test]
fn a_recorded_link_names_its_source_and_wins_over_inference() -> R {
    let rig = rig()?;
    // Two completed planners with the same verdict: inference alone cannot
    // decide, so this link must be the origin the alarm names.
    rig.store
        .insert_step(NEED, &row("plan", "t-plan", StepState::Done, Some("hold")))?;
    rig.store.insert_step(
        NEED,
        &row("plan2", "t-plan2", StepState::Done, Some("hold")),
    )?;
    rig.store
        .insert_step(NEED, &row("parked", "t-park", StepState::Waiting, None))?;
    rig.store.link_continuation(&ContinuationLink {
        task: "t-park".into(),
        need: NEED.into(),
        run: 1,
        node: "parked".into(),
        source: "t-plan".into(),
    })?;
    rig.store.record_step_payload(&payload(
        "t-plan",
        "plan",
        serde_json::json!({ "reason": "waiting on the schema decision" }),
    ))?;

    assert_eq!(poll(&rig)?, None);
    let text = exceptions(&rig);
    assert_eq!(count(&text), 1, "{text}");
    assert!(text.contains("actual: t-plan answered hold"), "{text}");
    assert!(!text.contains("t-plan2"), "{text}");
    assert!(!text.contains("unresolved"), "{text}");
    Ok(())
}

#[test]
fn ambiguous_evidence_is_reported_and_the_park_keeps_waiting() -> R {
    let rig = rig()?;
    rig.store
        .insert_step(NEED, &row("plan", "t-plan", StepState::Done, Some("hold")))?;
    rig.store.insert_step(
        NEED,
        &row("plan2", "t-plan2", StepState::Done, Some("hold")),
    )?;
    rig.store
        .insert_step(NEED, &row("parked", "t-park", StepState::Waiting, None))?;

    assert_eq!(poll(&rig)?, None);
    let text = exceptions(&rig);
    assert_eq!(count(&text), 1, "{text}");
    assert!(
        text.contains("actual: the stopped step is unresolved"),
        "{text}"
    );
    assert!(text.contains("ambiguous"), "{text}");
    Ok(())
}
