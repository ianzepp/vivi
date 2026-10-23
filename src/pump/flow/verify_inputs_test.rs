//! Tests for the brief input readers in [`super`].

use std::sync::Arc;

use serde_json::json;
use tempfile::{TempDir, tempdir};

use super::{
    admitted_plan, audit_context, branch_range, capped, cto_ruling, latest_of_class, need_thread,
    requirements, rung_report, whole_message_requirements,
};
use crate::pump::config::PumpConfig;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::store::step_payload::StepPayloadRow;
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};

const NEED: &str = "need-1";
const BASE: &str = "1111111111111111111111111111111111111111";
const TIP: &str = "2222222222222222222222222222222222222222";

struct Fixture {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    store: PumpStore,
}

fn fixture() -> Fixture {
    let dir = tempdir().expect("tempdir");
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    Fixture {
        _dir: dir,
        ctx,
        host,
        store,
    }
}

fn payload(task: &str, run: u32, class: &str, body: serde_json::Value) -> StepPayloadRow {
    StepPayloadRow {
        need: NEED.into(),
        run,
        node: class.into(),
        class: class.into(),
        task: task.into(),
        payload: body,
    }
}

fn step(task: &str, run: u32, verdict: Option<&str>) -> StepRow {
    StepRow {
        run,
        node: "plan".into(),
        task: task.into(),
        state: StepState::Done,
        verdict: verdict.map(str::to_string),
    }
}

fn record_plan(f: &Fixture, task: &str, run: u32, verdict: &str, text: &str) {
    f.store
        .insert_step(NEED, &step(task, run, Some(verdict)))
        .unwrap();
    f.store
        .record_step_payload(&payload(task, run, "planner", json!({ "body": text })))
        .unwrap();
}

#[test]
fn admitted_plan_is_the_planned_plan_not_an_earlier_revision() {
    let f = fixture();
    record_plan(&f, "t1", 1, "revised", "plan one");
    record_plan(&f, "t2", 1, "planned", "plan two");
    record_plan(&f, "t3", 2, "revised", "plan three");
    assert_eq!(
        admitted_plan(&f.store, NEED).unwrap().as_deref(),
        Some("plan two")
    );
    assert_eq!(admitted_plan(&f.store, "other").unwrap(), None);
}

#[test]
fn branch_range_errors_without_a_need_branch_row() {
    let f = fixture();
    let err = branch_range(&f.ctx, &f.store, NEED).unwrap_err();
    assert!(err.to_string().contains(NEED), "{err}");
}

#[test]
fn branch_range_reads_the_row_and_the_pushed_tip() {
    let f = fixture();
    f.store
        .upsert_need_branch(&NeedBranchRow {
            need: NEED.into(),
            branch: "vivi/need/need-1".into(),
            base: Some(BASE.into()),
            status: "live".into(),
            run: 1,
            target: "default".into(),
            priors: vec![],
        })
        .unwrap();
    f.host.reply(
        "ls-remote origin refs/heads/vivi/need/",
        0,
        &format!("{TIP}\trefs/heads/vivi/need/need-1\n"),
    );
    let range = branch_range(&f.ctx, &f.store, NEED).unwrap();
    assert_eq!(range.branch, "vivi/need/need-1");
    assert_eq!(range.base.as_deref(), Some(BASE));
    assert_eq!(range.tip.as_deref(), Some(TIP));
    assert_eq!(range.target, "default");
}

#[test]
fn latest_of_class_picks_the_newest() {
    let rows = vec![
        payload("a", 1, "auditor", json!({"n": 1})),
        payload("b", 1, "rung-need", json!({})),
        payload("c", 2, "auditor", json!({"n": 2})),
    ];
    assert_eq!(latest_of_class(&rows, "auditor").unwrap().task, "c");
    assert_eq!(latest_of_class(&rows, "rung-need").unwrap().task, "b");
    assert!(latest_of_class(&rows, "judge").is_none());
}

#[test]
fn rung_report_lists_exit_sorted_failing_ids_and_tail() {
    let row = payload(
        "r",
        1,
        "rung-need",
        json!({"exit": 101, "failing": ["b::two", "a::one"], "tail": "boom"}),
    );
    assert_eq!(
        rung_report(&row),
        "exit: 101\nfailing: a::one, b::two\ntail:\nboom"
    );
    let bare = payload("r", 1, "rung-need", json!({}));
    assert_eq!(rung_report(&bare), "exit: none\nfailing: none\ntail:\n");
}

#[test]
fn capped_keeps_the_tail_on_a_char_boundary() {
    assert_eq!(capped("short", 10), "short");
    assert_eq!(capped("abcdefghij", 4), "[...]\nghij");
    // "é" is two bytes: a cut inside it moves forward to the next boundary.
    let text = "aaéé";
    let out = capped(text, 3);
    assert_eq!(out, "[...]\né");
}

fn thread_json() -> String {
    serde_json::to_string(&json!([
        {
            "handle": NEED,
            "message_id": "message-1",
            "from": "operator@local",
            "subject": "Need",
            "body": "Build the thing"
        },
        {
            "handle": "reply-1",
            "message_id": "message-2",
            "from": "operator@local",
            "subject": "Need amendment",
            "body": "also keep the CLI shape"
        }
    ]))
    .unwrap()
}

#[test]
fn need_thread_parses_a_need_show_reply() {
    let f = fixture();
    f.host.reply("need show", 0, &thread_json());
    let messages = need_thread(&f.ctx, NEED).unwrap();
    assert_eq!(messages.len(), 2);
    let reqs = requirements(&f.ctx, NEED, &messages);
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].id, "need-1-r0");
}

#[test]
fn a_front_matter_root_gives_one_requirement_per_done_when_entry() {
    let f = fixture();
    let root = crate::letter::fixture::need_body(&["first thing", "second thing"], "prose");
    let messages: Vec<serde_json::Value> = vec![
        json!({"handle": NEED, "from": "operator@local", "body": root}),
        json!({"handle": "reply-1", "from": "operator@local", "body": "also keep the CLI shape"}),
    ];
    let reqs = requirements(&f.ctx, NEED, &messages);
    let ids: Vec<&str> = reqs.iter().map(|req| req.id.as_str()).collect();
    assert_eq!(ids, ["need-1-d1", "need-1-d2", "reply-1-r0"]);
    assert_eq!(reqs[0].text, "first thing");
    assert_eq!(reqs[1].text, "second thing");
}

#[test]
fn a_prose_root_keeps_the_whole_message_requirement() {
    let f = fixture();
    let messages: Vec<serde_json::Value> = vec![
        json!({"handle": NEED, "from": "operator@local", "body": "Build the thing"}),
        json!({"handle": "reply-1", "from": "operator@local", "body": "more"}),
    ];
    let reqs = requirements(&f.ctx, NEED, &messages);
    let ids: Vec<&str> = reqs.iter().map(|req| req.id.as_str()).collect();
    assert_eq!(ids, ["need-1-r0", "reply-1-r0"]);
}

#[test]
fn need_thread_errors_on_an_empty_array_and_on_failure() {
    let f = fixture();
    f.host.reply("need show", 0, "[]");
    let err = need_thread(&f.ctx, NEED).unwrap_err();
    assert!(err.to_string().contains("no root message"), "{err}");
    f.host.reply("need show", 1, "boom");
    let err = need_thread(&f.ctx, NEED).unwrap_err();
    assert!(
        err.to_string().contains("cannot read full need thread"),
        "{err}"
    );
}

fn put(f: &Fixture, task: &str, run: u32, class: &str, body: serde_json::Value) {
    f.store
        .record_step_payload(&payload(task, run, class, body))
        .unwrap();
}

#[test]
fn audit_context_reads_the_earlier_run_when_this_run_has_no_audit() {
    let f = fixture();
    put(
        &f,
        "a1",
        1,
        "auditor",
        json!({ "front": { "answer": "block_range" } }),
    );
    put(&f, "r1", 1, "rung", json!({ "exit": 1 }));
    put(&f, "s2", 2, "slicer", json!({}));
    let (run, rows) = audit_context(&f.store, NEED, 2).unwrap();
    assert_eq!(run, 1);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.run == 1));
}

#[test]
fn audit_context_keeps_a_run_that_audited() {
    let f = fixture();
    put(&f, "a1", 1, "auditor", json!({}));
    put(&f, "a2", 2, "auditor", json!({}));
    let (run, rows) = audit_context(&f.store, NEED, 2).unwrap();
    assert_eq!(run, 2);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].task, "a2");
}

#[test]
fn audit_context_without_any_audit_gives_the_asked_run() {
    let f = fixture();
    put(&f, "s1", 1, "slicer", json!({}));
    put(&f, "s2", 2, "slicer", json!({}));
    let (run, rows) = audit_context(&f.store, NEED, 2).unwrap();
    assert_eq!(run, 2);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].task, "s2");
}

#[test]
fn cto_ruling_is_the_earlier_runs_answer_reason_and_new_fact() {
    let f = fixture();
    put(
        &f,
        "c1",
        1,
        "cto",
        json!({ "front": { "answer": "rework", "reason": "R", "new_fact": "X" } }),
    );
    let ruling = cto_ruling(&f.store, NEED, 2).unwrap().unwrap();
    assert_eq!(ruling, "answer: rework\nreason: R\nnew_fact: X");
    assert_eq!(cto_ruling(&f.store, NEED, 1).unwrap(), None);
}

#[test]
fn a_need_without_done_when_is_audited_against_its_own_messages() {
    let f = fixture();
    let ctx = &f.ctx;
    let thread = serde_json::json!([{
        "handle": "need-1",
        "message_id": "mind-root",
        "from": "operator@local",
        "subject": "Need",
        "body": "Rename the en pack rows and every en source together."
    }, {
        "handle": "need-1-reply",
        "message_id": "pump-reply",
        "from": "mind@local",
        "subject": "Pump plan",
        "body": "pump: planned at revision 1"
    }]);
    let messages = thread.as_array().unwrap().clone();
    let requirements = whole_message_requirements(ctx, "need-1", &messages);
    assert_eq!(requirements.len(), 1, "{requirements:?}");
    assert_eq!(requirements[0].id, "need-1-r0");
    assert_eq!(
        requirements[0].text,
        "Deliver what need message need-1 asks for (full text in the original need thread)."
    );
    assert!(!requirements[0].text.contains("Rename the en pack rows"));
}

#[test]
fn a_long_multi_line_message_still_yields_one_short_single_line_requirement() {
    let f = fixture();
    let ctx = &f.ctx;
    let body = format!("first line\n{}", "long detail\n".repeat(1000));
    let messages = vec![serde_json::json!({
        "handle": "need-9",
        "message_id": "mind-root",
        "from": "operator@local",
        "subject": "Need",
        "body": body,
    })];

    let requirements = whole_message_requirements(ctx, "need-9", &messages);

    assert_eq!(requirements.len(), 1);
    assert!(!requirements[0].text.contains('\n'));
    assert!(requirements[0].text.len() < 200);
}
