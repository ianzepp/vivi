//! Tests for the cto step handler.

use std::sync::Arc;

use serde_json::json;
use tempfile::{TempDir, tempdir};

use super::CtoStart;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::{FlowKind, FlowNode, NodeClass};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::JobSpec;
use crate::pump::store::flow_links::SliceLink;
use crate::pump::store::step_payload::StepPayloadRow;
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};

const NEED: &str = "need-1";

struct Fixture {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
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
        mail: FakeMail::new("mind"),
    }
}

fn run_row(number: u32, entry: &str, state: &str) -> RunRow {
    RunRow {
        need: NEED.into(),
        run: number,
        flow: "build".into(),
        flow_version: 1,
        kind: "build".into(),
        entry: entry.into(),
        state: state.into(),
    }
}

fn add_step(f: &Fixture, run: u32, node: &str, state: StepState, verdict: Option<&str>) {
    f.store
        .insert_step(
            NEED,
            &StepRow {
                run,
                node: node.into(),
                task: format!("t-{node}-r{run}"),
                state,
                verdict: verdict.map(str::to_string),
            },
        )
        .unwrap();
}

fn payload(f: &Fixture, run: u32, node: &str, class: &str, body: serde_json::Value) {
    f.store
        .record_step_payload(&StepPayloadRow {
            need: NEED.into(),
            run,
            node: node.into(),
            class: class.into(),
            task: format!("t-{node}-r{run}"),
            payload: body,
        })
        .unwrap();
}

/// Two runs: run 1 failed its audit and ended in the cto; run 2 audited again.
fn two_runs(f: &Fixture, newest_answer: &str) -> RunRow {
    let first = run_row(1, "plan", "done");
    let second = run_row(2, "slice", "live");
    f.store.insert_run(&first).unwrap();
    f.store.insert_run(&second).unwrap();
    add_step(f, 1, "sentinel", StepState::Running, None);
    add_step(f, 1, "plan", StepState::Done, Some("planned"));
    add_step(f, 1, "audit1", StepState::Done, Some("defects"));
    add_step(f, 2, "sentinel", StepState::Running, None);
    add_step(f, 2, "slice", StepState::Done, Some("sliced"));
    add_step(f, 2, "audit1", StepState::Done, Some("verdict-two"));
    payload(
        f,
        1,
        "audit1",
        "auditor",
        json!({"front": {"answer": "defects"}, "body": "RUN ONE FINDINGS"}),
    );
    payload(
        f,
        2,
        "audit1",
        "auditor",
        json!({"front": {"answer": newest_answer}, "body": "RUN TWO FULL REPORT"}),
    );
    second
}

fn start(f: &Fixture, run: &RunRow) -> JobSpec {
    let def = load(FlowKind::Build).unwrap();
    let cx = StartCx {
        ctx: &f.ctx,
        store: &f.store,
        mail: &f.mail,
        run,
        def: &def,
    };
    let step = StepRow {
        run: run.run,
        node: "cto".into(),
        task: "t-cto".into(),
        state: StepState::Waiting,
        verdict: None,
    };
    match CtoStart.start(&cx, &step).unwrap() {
        StartAction::Job(spec) => *spec,
        other => panic!("expected a job, got {other:?}"),
    }
}

#[test]
fn the_job_is_head_cto_with_run_count_and_step_verdicts() {
    let f = fixture();
    let run = two_runs(&f, "block_ship");
    let spec = start(&f, &run);
    assert_eq!(spec.role, "head-cto");
    assert_eq!(spec.kind, "flow-cto");
    assert_eq!(spec.task.as_deref(), Some("t-cto"));
    assert!(spec.body.contains("runs: 2"), "{}", spec.body);
    assert!(spec.body.contains("run 1: entry plan, state done"));
    assert!(spec.body.contains("run 2: entry slice, state live"));
    assert!(spec.body.contains("audit1 t-audit1-r1 done defects"));
    assert!(spec.body.contains("audit1 t-audit1-r2 done verdict-two"));
    assert!(spec.body.contains("plan t-plan-r1 done planned"));
    assert!(!spec.body.contains("sentinel"));
    assert!(!spec.body.contains("new_fact"));
    assert!(spec.body.contains("- lock:"));
}

#[test]
fn a_block_ship_audit_brings_its_full_body() {
    let f = fixture();
    let run = two_runs(&f, "block_ship");
    let spec = start(&f, &run);
    assert!(spec.body.contains("## Block-ship audit"));
    assert!(spec.body.contains("RUN TWO FULL REPORT"));
    assert!(spec.body.contains(r#"{"answer":"block_ship"}"#));
}

#[test]
fn another_newest_audit_answer_brings_no_block_ship_body() {
    let f = fixture();
    let run = two_runs(&f, "defects");
    let spec = start(&f, &run);
    assert!(!spec.body.contains("## Block-ship audit"));
    assert!(!spec.body.contains("RUN TWO FULL REPORT"));
}

#[test]
fn from_the_third_run_every_answer_but_lock_needs_a_new_fact() {
    let f = fixture();
    two_runs(&f, "defects");
    let third = run_row(3, "rework1", "live");
    f.store.insert_run(&third).unwrap();
    let spec = start(&f, &third);
    assert!(spec.body.contains("runs: 3"));
    assert!(spec.body.contains("new_fact"));
}

#[test]
fn a_long_front_is_capped() {
    let f = fixture();
    let run = two_runs(&f, "defects");
    payload(
        &f,
        2,
        "judge",
        "judge",
        json!({"front": {"answer": "x".repeat(9000)}}),
    );
    let spec = start(&f, &run);
    assert!(spec.body.contains("[...]"));
    assert!(spec.body.len() < 9000);
}

#[test]
fn the_cto_class_registers_for_build() {
    let mut registry: Registry<crate::pump::state::State, FakeMail> = Registry::new();
    super::register(&mut registry);
    let node = FlowNode {
        id: "cto".into(),
        class: NodeClass::Cto,
        label: String::new(),
    };
    assert!(registry.handler_for(FlowKind::Build, &node).is_some());
}

#[test]
fn a_second_pass_block_names_the_failing_test_and_the_automatic_block_ship() {
    let f = fixture();
    let run = run_row(1, "plan", "live");
    f.store.insert_run(&run).unwrap();
    add_step(&f, 1, "sentinel", StepState::Running, None);
    add_step(&f, 1, "audit1", StepState::Done, Some("defects"));
    add_step(&f, 1, "test2", StepState::Done, Some("fail"));
    payload(
        &f,
        1,
        "test1",
        "rung-need",
        json!({"exit": 101, "failing": ["lib::pump::flow::broken_test"], "tail": "assertion failed"}),
    );
    let spec = start(&f, &run);
    assert!(
        spec.body.contains("## Failing rung output"),
        "{}",
        spec.body
    );
    assert!(spec.body.contains("run 1 test1:"));
    assert!(spec.body.contains("lib::pump::flow::broken_test"));
    assert!(spec.body.contains("## Why the CTO was reached"));
    assert!(spec.body.contains("test2 answered fail"));
    assert!(
        spec.body
            .contains("Two verify passes failed: this is the automatic block-ship of the need.")
    );
}

#[test]
fn an_admit2_escalation_names_the_step_and_has_no_block_ship_line() {
    let f = fixture();
    let run = run_row(1, "plan", "live");
    f.store.insert_run(&run).unwrap();
    add_step(&f, 1, "sentinel", StepState::Running, None);
    add_step(&f, 1, "admit2", StepState::Done, Some("escalate"));
    let spec = start(&f, &run);
    assert!(
        spec.body.contains("admit2 answered escalate"),
        "{}",
        spec.body
    );
    assert!(!spec.body.contains("automatic block-ship"));
    assert!(spec.body.contains("## Failing rung output\n\n(none)"));
}

fn slice_link(f: &Fixture, task: &str, fanout: &str) {
    f.store
        .link_slice(&SliceLink {
            task: task.into(),
            need: NEED.into(),
            run: 1,
            fanout: Some(fanout.into()),
            deps: Vec::new(),
        })
        .unwrap();
}

#[test]
fn an_unchanged_fanout_names_expected_against_actual_and_no_block_ship() {
    let f = fixture();
    let run = run_row(1, "plan", "live");
    f.store.insert_run(&run).unwrap();
    add_step(&f, 1, "sentinel", StepState::Running, None);
    add_step(&f, 1, "build1", StepState::Done, Some("unchanged"));
    add_step(&f, 1, "build2", StepState::Done, Some("unchanged"));
    slice_link(&f, "t-s1", "t-build1-r1");
    slice_link(&f, "t-s2", "t-build1-r1");
    slice_link(&f, "t-s3", "t-build2-r1");
    let start_tip = "a".repeat(40);
    f.store
        .set_fanout_start("t-build2-r1", Some(&start_tip))
        .unwrap();
    f.store
        .upsert_need_branch(&NeedBranchRow {
            need: NEED.into(),
            branch: format!("vivi/need/{NEED}"),
            base: None,
            status: "live".into(),
            run: 1,
            target: "default".into(),
            priors: Vec::new(),
        })
        .unwrap();
    f.host.reply(
        "ls-remote origin refs/heads/vivi/need/",
        0,
        &format!("{start_tip}\trefs/heads/vivi/need/{NEED}\n"),
    );
    let spec = start(&f, &run);
    let line = format!(
        "expected a change on vivi/need/{NEED} in fanout build2 (1 slices); actual: 0 merged, need tip {start_tip} == fanout start {start_tip}"
    );
    assert!(spec.body.contains(&line), "{}", spec.body);
    assert!(
        !spec.body.contains("block-ship of the need"),
        "{}",
        spec.body
    );
}

#[test]
fn an_unchanged_first_fanout_with_no_tips_says_none() {
    let f = fixture();
    let run = run_row(1, "plan", "live");
    f.store.insert_run(&run).unwrap();
    add_step(&f, 1, "sentinel", StepState::Running, None);
    add_step(&f, 1, "build1", StepState::Done, Some("unchanged"));
    slice_link(&f, "t-s1", "t-build1-r1");
    slice_link(&f, "t-s2", "t-build1-r1");
    let spec = start(&f, &run);
    assert!(
        spec.body.contains(&format!(
            "expected a change on vivi/need/{NEED} in fanout build1 (2 slices); actual: 0 merged, no need branch yet (need tip none, fanout start none)"
        )),
        "{}",
        spec.body
    );
}

#[test]
fn an_unchanged_fanout_whose_branch_moved_says_the_tips_differ() {
    let f = fixture();
    let run = run_row(1, "plan", "live");
    f.store.insert_run(&run).unwrap();
    add_step(&f, 1, "sentinel", StepState::Running, None);
    add_step(&f, 1, "build2", StepState::Done, Some("unchanged"));
    slice_link(&f, "t-s1", "t-build2-r1");
    let start_tip = "a".repeat(40);
    let now_tip = "b".repeat(40);
    f.store
        .set_fanout_start("t-build2-r1", Some(&start_tip))
        .unwrap();
    f.store
        .upsert_need_branch(&NeedBranchRow {
            need: NEED.into(),
            branch: format!("vivi/need/{NEED}"),
            base: None,
            status: "live".into(),
            run: 1,
            target: "default".into(),
            priors: Vec::new(),
        })
        .unwrap();
    f.host.reply(
        "ls-remote origin refs/heads/vivi/need/",
        0,
        &format!("{now_tip}\trefs/heads/vivi/need/{NEED}\n"),
    );
    let spec = start(&f, &run);
    let line = format!(
        "actual: 0 merged, need tip {now_tip} != fanout start {start_tip} (the branch moved with no merged slice)"
    );
    assert!(spec.body.contains(&line), "{}", spec.body);
    assert!(!spec.body.contains("=="), "{}", spec.body);
}
