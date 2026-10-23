//! A step task sent before a crash but never indexed is adopted by the retry
//! (start and route), not sent a second time.

use serde_json::json;
use tempfile::tempdir;

use super::{SENTINEL_NODE, resume, start};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::fixture::project;
use crate::pump::flow::advance::route::{JoinInput, route};
use crate::pump::flow::def::{FlowKind, FlowNode, SENTINEL_MARKER, STEP_MARKER};
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::index::run_state;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailReader, MailWriter};
use crate::pump::flow::outcome::Outcome;
use crate::pump::host::test_ctx;
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "need1";

fn with_first_line(mail: &FakeMail, line: &str) -> usize {
    mail.tasks
        .values()
        .filter(|t| t.body.lines().next() == Some(line))
        .count()
}

fn sends(mail: &FakeMail) -> usize {
    mail.calls
        .iter()
        .filter(|c| matches!(c, Call::SendStep { .. }))
        .count()
}

fn run_row(state: &str) -> RunRow {
    RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: state.into(),
    }
}

/// Send the sentinel and entry tasks the way start would, with no step rows.
fn hand_send_start(mail: &mut FakeMail) -> Result<(String, String), VivariumError> {
    let sentinel = mail.send_step(
        "pump",
        "pump",
        &format!("{NEED} r1 {SENTINEL_NODE}"),
        &format!("{SENTINEL_MARKER} {NEED} r1"),
        &[],
    )?;
    let entry = mail.send_step(
        "pump",
        "pump",
        &format!("{NEED} r1 plan"),
        &format!("{STEP_MARKER} {NEED} r1 plan"),
        &[],
    )?;
    mail.calls.clear();
    Ok((sentinel, entry))
}

fn assert_adopted(store: &PumpStore, mail: &FakeMail, hand: &(String, String)) -> R {
    assert_eq!(sends(mail), 0, "nothing was sent a second time");
    let sentinel_line = format!("{SENTINEL_MARKER} {NEED} r1");
    assert_eq!(with_first_line(mail, &sentinel_line), 1);
    let entry_line = format!("{STEP_MARKER} {NEED} r1 plan");
    assert_eq!(with_first_line(mail, &entry_line), 1);
    let rows = store.steps_of(NEED, 1)?;
    let task_of = |node: &str| rows.iter().find(|r| r.node == node).map(|r| r.task.clone());
    assert_eq!(task_of(SENTINEL_NODE), Some(hand.0.clone()));
    assert_eq!(task_of("plan"), Some(hand.1.clone()));
    let bound = mail.bound_units(NEED)?;
    assert!(bound.contains(&hand.0) && bound.contains(&hand.1));
    Ok(())
}

#[test]
fn start_adopts_tasks_sent_without_step_rows() -> R {
    let project = project();
    let ctx = project.pump_ctx();
    let store = PumpStore::open(&project.home().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("pump");
    let hand = hand_send_start(&mut mail)?;

    let row = start(&ctx, &store, &mut mail, NEED, None)?;
    assert_eq!(row.state, "live");
    assert_adopted(&store, &mail, &hand)
}

#[test]
fn resume_adopts_tasks_sent_without_step_rows() -> R {
    let project = project();
    let ctx = project.pump_ctx();
    let store = PumpStore::open(&project.home().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("pump");
    let hand = hand_send_start(&mut mail)?;
    let starting = run_row(run_state::STARTING);
    store.insert_run(&starting)?;

    let row = resume(&ctx, &store, &mut mail, &starting)?;
    assert_eq!(row.state, "live");
    assert_adopted(&store, &mail, &hand)
}

/// A step task for `node`, as route would send it; `indexed` writes its row.
fn step_task(
    store: &PumpStore,
    mail: &mut FakeMail,
    (node, label): (&str, &str),
    indexed: bool,
) -> Result<String, VivariumError> {
    let task = mail.send_step(
        "pump",
        "pump",
        &format!("{label} ({NEED} r1)"),
        &format!("{STEP_MARKER} {NEED} r1 {node}\n\n{label}"),
        &[],
    )?;
    if indexed {
        store.insert_step(
            NEED,
            &StepRow {
                run: 1,
                node: node.into(),
                task: task.clone(),
                state: StepState::Running,
                verdict: None,
            },
        )?;
    }
    Ok(task)
}

#[test]
fn route_adopts_a_successor_sent_without_a_step_row() -> R {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    store.insert_run(&run_row("live"))?;
    let def = load(FlowKind::Build)?;
    let label = |node: &str| {
        def.nodes
            .iter()
            .find(|n| n.id == node)
            .map(|n| n.label.clone())
            .unwrap_or_default()
    };
    let mut mail = FakeMail::new("pump");
    let sentinel = mail.send_step(
        "pump",
        "pump",
        "run",
        &format!("{SENTINEL_MARKER} {NEED} r1"),
        &[],
    )?;
    store.insert_step(
        NEED,
        &StepRow {
            run: 1,
            node: SENTINEL_NODE.into(),
            task: sentinel,
            state: StepState::Running,
            verdict: None,
        },
    )?;
    let build = step_task(&store, &mut mail, ("build1", &label("build1")), true)?;
    // The crash window: audit1 was sent, but its step row was never written.
    let hand = step_task(&store, &mut mail, ("audit1", &label("audit1")), false)?;
    mail.calls.clear();

    let out = Outcome {
        need: NEED.into(),
        run: 1,
        node: "build1".into(),
        task: build,
        word: "built".into(),
        payload: json!({}),
    };
    let decide = |_: &FlowNode, _: &[JoinInput]| Ok(String::new());
    let routed = route(&ctx, &store, &mut mail, &def, &out, &decide)?;

    let audit: Vec<_> = routed.sent.iter().filter(|s| s.node == "audit1").collect();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].task, hand, "the hand-sent task is used");
    let audit_line = format!("{STEP_MARKER} {NEED} r1 audit1");
    assert_eq!(with_first_line(&mail, &audit_line), 1);
    assert_eq!(
        store.step_by_task(&hand)?.map(|r| (r.node, r.state)),
        Some(("audit1".to_string(), StepState::Pending))
    );
    assert!(mail.bound_units(NEED)?.contains(&hand));
    assert_eq!(
        sends(&mail),
        routed.sent.len() - 1,
        "only the others were sent"
    );
    Ok(())
}
