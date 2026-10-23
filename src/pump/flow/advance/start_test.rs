//! Run-start tests over the fake mail port and a real pump store.

use std::fs;

use super::{SENTINEL_NODE, start};
use crate::pump::fixture::{Project, project};
use crate::pump::flow::def::{SENTINEL_MARKER, STEP_MARKER};
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::index::StepState;
use crate::pump::flow::mail::MailWriter;
use crate::pump::host::Ctx;
use crate::pump::store::step_payload::StepPayloadRow;
use crate::pump::store::{DB_FILENAME, PumpStore};

const NEED: &str = "need1";

fn setup() -> (Project, Ctx, PumpStore, FakeMail) {
    let project = project();
    let ctx = project.pump_ctx();
    let store = PumpStore::open(&project.home().join(DB_FILENAME)).unwrap();
    let mail = FakeMail::new("pump");
    (project, ctx, store, mail)
}

fn exceptions(ctx: &Ctx) -> String {
    fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap_or_default()
}

#[test]
fn the_sentinel_is_sent_before_the_step_with_one_bind() {
    let (_project, ctx, store, mut mail) = setup();
    let row = start(&ctx, &store, &mut mail, NEED, None).unwrap();
    assert_eq!(row.run, 1);
    assert_eq!(row.kind, "build");
    assert_eq!(row.entry, "plan");

    let sends: Vec<&str> = mail
        .calls
        .iter()
        .filter_map(|call| match call {
            Call::SendStep { handle, .. } => Some(handle.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(sends.len(), 2);
    let sentinel = sends[0].to_string();
    let step = sends[1].to_string();
    assert!(
        mail.tasks[&sentinel]
            .body
            .starts_with(&format!("{SENTINEL_MARKER} {NEED} r1"))
    );
    assert!(
        mail.tasks[&step]
            .body
            .starts_with(&format!("{STEP_MARKER} {NEED} r1 plan"))
    );

    let binds: Vec<usize> = mail
        .calls
        .iter()
        .enumerate()
        .filter(|(_, call)| matches!(call, Call::Bind { .. }))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(binds.len(), 1);
    let bind_at = binds[0];
    assert!(
        mail.position(|c| matches!(c, Call::SendStep { handle, .. } if *handle == step))
            < Some(bind_at)
    );
    match &mail.calls[bind_at] {
        Call::Bind { need, units } => {
            assert_eq!(need, NEED);
            assert_eq!(units, &vec![sentinel.clone(), step.clone()]);
        }
        other => panic!("not a bind: {other:?}"),
    }

    let steps = store.steps_of(NEED, 1).unwrap();
    assert_eq!(steps.len(), 2);
    assert!(
        steps
            .iter()
            .any(|s| s.node == SENTINEL_NODE && s.task == sentinel)
    );
    let entry = steps.iter().find(|s| s.node == "plan").unwrap();
    assert_eq!(entry.task, step);
    assert_eq!(entry.state, StepState::Pending);
    assert_eq!(store.live_runs().unwrap(), vec![row]);
}

#[test]
fn the_second_run_has_number_two() {
    let (_project, ctx, store, mut mail) = setup();
    start(&ctx, &store, &mut mail, NEED, None).unwrap();
    let second = start(&ctx, &store, &mut mail, NEED, Some("research")).unwrap();
    assert_eq!(second.run, 2);
    assert_eq!(second.entry, "research");
    assert_eq!(store.runs_of(NEED).unwrap().len(), 2);
    assert!(exceptions(&ctx).is_empty());
}

#[test]
fn an_entry_the_flow_does_not_have_is_refused() {
    let (_project, ctx, store, mut mail) = setup();
    let err = start(&ctx, &store, &mut mail, NEED, Some("wait")).unwrap_err();
    assert!(err.to_string().contains("no entry `wait`"));
    assert!(mail.calls.is_empty());
}

#[test]
fn the_fourth_run_raises_the_alarm_and_the_third_does_not() {
    let (_project, ctx, store, mut mail) = setup();
    for _ in 0..3 {
        start(&ctx, &store, &mut mail, NEED, None).unwrap();
    }
    assert!(!exceptions(&ctx).contains("flow-run-4"));
    let fourth = start(&ctx, &store, &mut mail, NEED, None).unwrap();
    assert_eq!(fourth.run, 4);
    assert!(exceptions(&ctx).contains("flow-run-4"));
}

fn cto_payload(store: &PumpStore, run: u32, body: serde_json::Value) {
    store
        .record_step_payload(&StepPayloadRow {
            need: NEED.into(),
            run,
            node: "cto".into(),
            class: "cto".into(),
            task: format!("t-cto-r{run}"),
            payload: body,
        })
        .unwrap();
}

#[test]
fn the_run_4_alarm_names_each_earlier_cto_answer_and_new_fact() {
    let (_project, ctx, store, mut mail) = setup();
    for _ in 0..3 {
        start(&ctx, &store, &mut mail, NEED, None).unwrap();
    }
    cto_payload(
        &store,
        2,
        serde_json::json!({"front": {"answer": "rework", "new_fact": "F2"}}),
    );
    cto_payload(&store, 3, serde_json::json!({"note": "no front"}));
    start(&ctx, &store, &mut mail, NEED, None).unwrap();
    let text = exceptions(&ctx);
    let line = text
        .lines()
        .find(|line| line.contains("flow-run-4"))
        .unwrap();
    assert!(line.contains("run 4"), "{line}");
    assert!(line.contains("r1 no CTO answer"), "{line}");
    assert!(line.contains("r2 rework (F2)"), "{line}");
    assert!(line.contains("r3 failed (no new fact)"), "{line}");
}

#[test]
fn marks_pick_the_kind_and_the_record_body_overrides_them() {
    let (_project, ctx, store, mut mail) = setup();
    mail.set_marks("n-research", &["research"]);
    let row = start(&ctx, &store, &mut mail, "n-research", None).unwrap();
    assert_eq!(
        (row.kind.as_str(), row.entry.as_str()),
        ("research", "research")
    );

    mail.set_marks("n-release", &["release"]);
    assert_eq!(
        start(&ctx, &store, &mut mail, "n-release", None)
            .unwrap()
            .kind,
        "release"
    );

    mail.set_marks("n-fix", &["fix"]);
    assert_eq!(
        start(&ctx, &store, &mut mail, "n-fix", None).unwrap().kind,
        "build"
    );

    mail.set_marks("n-body", &["research"]);
    MailWriter::open_record(&mut mail, "n-body", "intake\nkind: release\n").unwrap();
    assert_eq!(
        start(&ctx, &store, &mut mail, "n-body", None).unwrap().kind,
        "release"
    );
}

#[test]
fn a_mark_added_after_start_leaves_the_runs_kind_unchanged() {
    let (_project, ctx, store, mut mail) = setup();
    let row = start(&ctx, &store, &mut mail, NEED, None).unwrap();
    assert_eq!(row.kind, "build");
    mail.set_marks(NEED, &["research"]);
    let stored = store.runs_of(NEED).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].kind, "build");
    assert_eq!(stored[0].flow, "build");
}

#[test]
fn the_run_start_post_follows_the_bind_and_precedes_any_close() {
    let (_project, ctx, store, mut mail) = setup();
    start(&ctx, &store, &mut mail, NEED, None).unwrap();
    let steps = store.steps_of(NEED, 1).unwrap();
    let entry = steps.iter().find(|s| s.node == "plan").unwrap();
    // A step finishing after the start closes its task; the post is already out.
    MailWriter::close_task(&mut mail, &entry.task, "ok", None, None).unwrap();
    MailWriter::close_need(&mut mail, NEED, "done").unwrap();

    let post = mail
        .position(|c| matches!(c, Call::PostToNeed { .. }))
        .expect("a run-start post");
    let bind = mail.position(|c| matches!(c, Call::Bind { .. })).unwrap();
    let close_step = mail
        .position(|c| matches!(c, Call::CloseTask { .. }))
        .unwrap();
    let close_need = mail
        .position(|c| matches!(c, Call::CloseNeed { .. }))
        .unwrap();
    assert!(bind < post && post < close_step && post < close_need);
    let posts: Vec<&Call> = mail
        .calls
        .iter()
        .filter(|c| matches!(c, Call::PostToNeed { .. }))
        .collect();
    assert_eq!(
        posts,
        vec![&Call::PostToNeed {
            need: NEED.into(),
            body: "pump: run r1 started (build, entry plan)".into(),
        }]
    );
}
