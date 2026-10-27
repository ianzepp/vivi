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
use crate::pump::store::recovery::pause::{PauseBoundaryRow, pause_state};
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

/// A recorded pause on `origin`'s own coordinates, the shape routing records.
/// `node = "parked"` and `origin_task = <gate task>` records it for the gate
/// itself, the shape that names a gate's own origin verbatim.
fn record_pause(rig: &Rig, node: &str, origin_task: &str) -> R {
    let boundary = PauseBoundaryRow {
        need: NEED.into(),
        run: 1,
        node: node.into(),
        origin_task: origin_task.into(),
        gate: "parked".into(),
        pause_generation: 1,
        input_generation: 1,
        role_request: format!("step-{origin_task}-a1"),
        boundary_message: None,
        paused_at: 1_700,
        state: pause_state::OPEN.into(),
    };
    rig.store.record_pause(&boundary)?;
    Ok(())
}

#[test]
fn park_after_a_hold_alarms_once_with_the_origin_and_reason() -> R {
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
        "plan (t-plan) answered hold: waiting on the schema decision",
        "a reply on the need thread continues it on a new thread",
        "vivi pump answer t-park",
    ] {
        assert!(text.contains(needle), "missing {needle}: {text}");
    }
    assert!(!text.contains("restarts it at plan"), "{text}");
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
    assert!(text.contains("answered operator: none given"), "{text}");
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
fn an_unresolved_origin_stays_parked_with_the_lookup_diagnostic() -> R {
    let rig = rig()?;
    assert_eq!(poll(&rig)?, None);
    let text = exceptions(&rig);
    assert_eq!(count(&text), 1, "{text}");
    assert!(
        text.contains(
            "need n1 r1 parked at parked: no completed step routed parked \
             through a decision on its answer; the park stays"
        ),
        "{text}"
    );
    assert!(text.contains("vivi pump answer t-park"), "{text}");
    Ok(())
}

#[test]
fn a_plan2_origin_is_named_with_its_own_answer() -> R {
    let rig = rig()?;
    rig.store.insert_step(
        NEED,
        &row("plan2", "t-plan2", StepState::Done, Some("operator")),
    )?;
    rig.store.record_step_payload(&payload(
        "t-plan2",
        "plan2",
        serde_json::json!({ "reason": "second plan wants a narrower slice" }),
    ))?;
    assert_eq!(poll(&rig)?, None);
    let text = exceptions(&rig);
    assert_eq!(count(&text), 1, "{text}");
    assert!(
        text.contains("plan2 (t-plan2) answered operator: second plan wants a narrower slice"),
        "{text}"
    );
    Ok(())
}

#[test]
fn a_recorded_origin_beats_an_ambiguous_legacy_topology() -> R {
    let rig = rig()?;
    // Both planners routed the park; only t-plan's pause is recorded, so the
    // record decides and the topology is never guessed at.
    rig.store.insert_step(
        NEED,
        &row("plan", "t-plan", StepState::Done, Some("operator")),
    )?;
    rig.store.insert_step(
        NEED,
        &row("plan2", "t-plan2", StepState::Done, Some("operator")),
    )?;
    rig.store.record_step_payload(&payload(
        "t-plan",
        "plan",
        serde_json::json!({ "reason": "hold for the operator" }),
    ))?;
    record_pause(&rig, "plan", "t-plan")?;
    assert_eq!(poll(&rig)?, None);
    assert_eq!(poll(&rig)?, None);
    let text = exceptions(&rig);
    assert_eq!(count(&text), 1, "{text}");
    assert!(
        text.contains("plan (t-plan) answered operator: hold for the operator"),
        "{text}"
    );
    assert!(!text.contains("t-plan2"), "{text}");
    Ok(())
}

#[test]
fn a_nonplanner_origin_recorded_on_the_gate_is_trusted_verbatim() -> R {
    let rig = rig()?;
    // A park-node record names the gate itself, so the recorded origin is the
    // park step, a nonplanner class: no planner is assumed.
    rig.store
        .insert_step(NEED, &row("parked", "t-park", StepState::Waiting, None))?;
    record_pause(&rig, "parked", "t-park")?;
    assert_eq!(poll(&rig)?, None);
    let text = exceptions(&rig);
    assert_eq!(count(&text), 1, "{text}");
    assert!(
        text.contains("parked (t-park) answered none: none given"),
        "{text}"
    );
    assert!(text.contains("continues it on a new thread"), "{text}");
    Ok(())
}

#[test]
fn an_ambiguous_legacy_park_names_both_candidates_and_stays() -> R {
    let rig = rig()?;
    rig.store.insert_step(
        NEED,
        &row("plan", "t-plan", StepState::Done, Some("operator")),
    )?;
    rig.store.insert_step(
        NEED,
        &row("plan2", "t-plan2", StepState::Done, Some("operator")),
    )?;
    assert_eq!(poll(&rig)?, None);
    assert_eq!(poll(&rig)?, None);
    let text = exceptions(&rig);
    assert_eq!(count(&text), 1, "{text}");
    for needle in [
        "t-plan",
        "t-plan2",
        "both routed parked through a decision",
        "the park stays",
    ] {
        assert!(text.contains(needle), "missing {needle}: {text}");
    }
    Ok(())
}
