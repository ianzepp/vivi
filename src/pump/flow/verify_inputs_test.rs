//! Tests for the brief input readers in [`super`].

use std::sync::Arc;

use serde_json::json;
use tempfile::{TempDir, tempdir};

use super::{
    admitted_plan, audit_context, branch_range, capped, cto_ruling, latest_of_class, need_thread,
    requirements, rung_report, whole_message_requirements,
};
use crate::pump::config::PumpConfig;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
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
            "body": crate::letter::fixture::need_body(&["Build the thing"], "")
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
    let reqs = requirements(&f.ctx, NEED, &messages).unwrap();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].id, "need-1-d1");
}

#[test]
fn a_front_matter_root_gives_one_requirement_per_done_when_entry() {
    let f = fixture();
    let root = crate::letter::fixture::need_body(&["first thing", "second thing"], "prose");
    let messages: Vec<serde_json::Value> = vec![
        json!({"handle": NEED, "from": "operator@local", "body": root}),
        json!({"handle": "reply-1", "from": "operator@local", "body": "also keep the CLI shape"}),
    ];
    let reqs = requirements(&f.ctx, NEED, &messages).unwrap();
    let ids: Vec<&str> = reqs.iter().map(|req| req.id.as_str()).collect();
    assert_eq!(ids, ["need-1-d1", "need-1-d2", "reply-1-r0"]);
    assert_eq!(reqs[0].text, "first thing");
    assert_eq!(reqs[1].text, "second thing");
}

#[test]
fn a_root_without_front_matter_is_an_error_naming_the_handle() {
    let f = fixture();
    let messages: Vec<serde_json::Value> = vec![
        json!({"handle": NEED, "from": "operator@local", "body": "Build the thing"}),
        json!({"handle": "reply-1", "from": "operator@local", "body": "more"}),
    ];
    let err = requirements(&f.ctx, NEED, &messages).unwrap_err();
    assert!(err.to_string().contains(NEED), "{err}");
    assert!(err.to_string().contains("front matter"), "{err}");
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

// --- admitted plan continuation (the resolver behind the slicer brief) ---

fn run_row(number: u32, entry: &str) -> RunRow {
    RunRow {
        need: NEED.into(),
        run: number,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: entry.into(),
        state: "live".into(),
    }
}

fn seed_run(f: &Fixture, number: u32, entry: &str) {
    f.store.insert_run(&run_row(number, entry)).unwrap();
}

/// A done planner step `node` of `run` whose stored payload body is `plan`.
fn seed_planner(f: &Fixture, run: u32, node: &str, plan: &str) {
    let task = format!("t-r{run}-{node}");
    f.store
        .record_step_payload(&StepPayloadRow {
            need: NEED.into(),
            run,
            node: node.into(),
            class: "planner".into(),
            task: task.clone(),
            payload: json!({
                "front": { "answer": "planned" },
                "body": plan,
            }),
        })
        .unwrap();
    f.store
        .insert_step(
            NEED,
            &StepRow {
                run,
                node: node.into(),
                task,
                state: StepState::Done,
                verdict: Some("planned".into()),
            },
        )
        .unwrap();
}

/// A done admitter step `node` of `run` answering `verdict`.
fn seed_admitter(f: &Fixture, run: u32, node: &str, verdict: &str) {
    f.store
        .insert_step(
            NEED,
            &StepRow {
                run,
                node: node.into(),
                task: format!("t-r{run}-{node}"),
                state: StepState::Done,
                verdict: Some(verdict.into()),
            },
        )
        .unwrap();
}

/// Run 1 at `plan` whose planner `planner` carries `plan` and whose admitter
/// `admitter` admitted it.
fn seed_admitted(f: &Fixture, planner: &str, admitter: &str, plan: &str) {
    seed_run(f, 1, "plan");
    seed_planner(f, 1, planner, plan);
    seed_admitter(f, 1, admitter, "admitted");
}

/// Another need whose run 1 admitted its own plan.
fn seed_other_need(f: &Fixture) {
    f.store
        .insert_run(&RunRow {
            need: "need-2".into(),
            ..run_row(1, "plan")
        })
        .unwrap();
    f.store
        .record_step_payload(&StepPayloadRow {
            need: "need-2".into(),
            run: 1,
            node: "plan".into(),
            class: "planner".into(),
            task: "other-plan".into(),
            payload: json!({
                "front": { "answer": "planned" },
                "body": "OTHER PLAN",
            }),
        })
        .unwrap();
    for (node, task, verdict) in [
        ("plan", "other-plan", "planned"),
        ("admit1", "other-admit", "admitted"),
    ] {
        f.store
            .insert_step(
                "need-2",
                &StepRow {
                    run: 1,
                    node: node.into(),
                    task: task.into(),
                    state: StepState::Done,
                    verdict: Some(verdict.into()),
                },
            )
            .unwrap();
    }
}

#[test]
fn slice_continuation_uses_previous_admitted_plan() {
    // The first admission: a slice run inherits run 1's plan/admit1 plan.
    let f = fixture();
    seed_admitted(&f, "plan", "admit1", "PLAN ONE");
    seed_run(&f, 2, "slice");
    let plan = admitted_plan(&f.store, &run_row(2, "slice")).unwrap();
    assert_eq!(plan.as_deref(), Some("PLAN ONE"));

    // The revised admission: a slice run inherits the plan2/admit2 plan.
    let f = fixture();
    seed_run(&f, 1, "plan");
    seed_planner(&f, 1, "plan", "PLAN ONE");
    seed_admitter(&f, 1, "admit1", "revise");
    seed_planner(&f, 1, "plan2", "PLAN TWO");
    seed_admitter(&f, 1, "admit2", "admitted");
    seed_run(&f, 2, "slice");
    let plan = admitted_plan(&f.store, &run_row(2, "slice")).unwrap();
    assert_eq!(plan.as_deref(), Some("PLAN TWO"));
}

#[test]
fn newer_unadmitted_plan_does_not_replace_admitted_plan() {
    let f = fixture();
    seed_admitted(&f, "plan", "admit1", "PLAN ONE");
    // A newer revision is planned, but no admitter admitted it.
    seed_planner(&f, 1, "plan2", "PLAN TWO");
    seed_run(&f, 2, "slice");

    let plan = admitted_plan(&f.store, &run_row(2, "slice")).unwrap();

    assert_eq!(plan.as_deref(), Some("PLAN ONE"));
}

#[test]
fn plan_inheritance_respects_need_run_and_replan_boundaries() {
    let f = fixture();
    seed_admitted(&f, "plan", "admit1", "PLAN ONE");

    // Repeated slice continuations keep reading the same admission.
    for number in [2, 3] {
        seed_run(&f, number, "slice");
        let plan = admitted_plan(&f.store, &run_row(number, "slice")).unwrap();
        assert_eq!(plan.as_deref(), Some("PLAN ONE"), "run {number}");
    }
    // A rework1 entry inherits it too.
    seed_run(&f, 4, "rework1");
    let plan = admitted_plan(&f.store, &run_row(4, "rework1")).unwrap();
    assert_eq!(plan.as_deref(), Some("PLAN ONE"));

    // A run with its own admission prefers it over every earlier run's.
    seed_run(&f, 5, "plan");
    seed_planner(&f, 5, "plan", "PLAN FIVE");
    seed_admitter(&f, 5, "admit1", "admitted");
    let plan = admitted_plan(&f.store, &run_row(5, "plan")).unwrap();
    assert_eq!(plan.as_deref(), Some("PLAN FIVE"));
    // and the next slice continues from the nearest earlier run, not run 1.
    seed_run(&f, 6, "slice");
    let plan = admitted_plan(&f.store, &run_row(6, "slice")).unwrap();
    assert_eq!(plan.as_deref(), Some("PLAN FIVE"));

    // A future run's admission is never read: run 2 predates run 5.
    let plan = admitted_plan(&f.store, &run_row(2, "slice")).unwrap();
    assert_eq!(plan.as_deref(), Some("PLAN ONE"));

    // Another need's admission is never read, and a fresh unadmitted plan or
    // research run ends the inheritance: the walk stops at the boundary.
    seed_other_need(&f);
    for (number, entry) in [(7, "plan"), (8, "slice"), (9, "research")] {
        seed_run(&f, number, entry);
        let plan = admitted_plan(&f.store, &run_row(number, entry)).unwrap();
        assert_eq!(plan, None, "run {number} at {entry}");
        assert_ne!(plan.as_deref(), Some("OTHER PLAN"), "run {number}");
    }
}

#[test]
fn a_blank_wrapped_prose_message_gets_no_requirement() {
    let f = fixture();
    let ctx = &f.ctx;
    let messages = vec![
        serde_json::json!({"handle": "need-5", "message_id": "m1", "from": "operator@local",
            "subject": "Need", "body": "+++\n+++\n\n"}),
        serde_json::json!({"handle": "need-5-a", "message_id": "m2", "from": "operator@local",
            "subject": "Need", "body": "+++\n+++\n\nAlso rename the rows."}),
    ];

    let requirements = whole_message_requirements(ctx, "need-5", &messages);

    assert_eq!(requirements.len(), 1, "{requirements:?}");
    assert_eq!(requirements[0].id, "need-5-a-r0");
}
