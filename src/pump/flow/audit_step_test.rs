//! Tests for the audit step handler.

use std::sync::Arc;

use serde_json::json;
use tempfile::{TempDir, tempdir};

use super::AuditStart;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::JobSpec;
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
    mail: FakeMail,
    run: RunRow,
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
        run: RunRow {
            need: NEED.into(),
            run: 1,
            flow: "build".into(),
            flow_version: 1,
            kind: "build".into(),
            entry: "plan".into(),
            state: "live".into(),
        },
    }
}

fn with_branch(f: &Fixture) {
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
    f.host.reply(
        "need show",
        0,
        &serde_json::to_string(&json!([
            {"handle": NEED, "message_id": "m1", "from": "operator@local",
             "subject": "Need", "body": "Build the thing"},
            {"handle": "reply-1", "message_id": "m2", "from": "operator@local",
             "subject": "Need amendment", "body": "also keep the CLI shape"}
        ]))
        .unwrap(),
    );
}

fn plan(f: &Fixture) {
    f.store
        .insert_step(
            NEED,
            &StepRow {
                run: 1,
                node: "plan".into(),
                task: "t-plan".into(),
                state: StepState::Done,
                verdict: Some("planned".into()),
            },
        )
        .unwrap();
    payload(f, "t-plan", "plan", "planner", json!({"body": "PLAN TEXT"}));
}

fn payload(f: &Fixture, task: &str, node: &str, class: &str, body: serde_json::Value) {
    payload_in(f, 1, task, node, class, body);
}

fn payload_in(f: &Fixture, run: u32, task: &str, node: &str, class: &str, body: serde_json::Value) {
    f.store
        .record_step_payload(&StepPayloadRow {
            need: NEED.into(),
            run,
            node: node.into(),
            class: class.into(),
            task: task.into(),
            payload: body,
        })
        .unwrap();
}

fn start(f: &Fixture, node: &str) -> Result<JobSpec, crate::error::VivariumError> {
    start_in(f, 1, node)
}

fn start_in(f: &Fixture, run: u32, node: &str) -> Result<JobSpec, crate::error::VivariumError> {
    let def = load(FlowKind::Build)?;
    let run_row = RunRow {
        run,
        ..f.run.clone()
    };
    let cx = StartCx {
        ctx: &f.ctx,
        store: &f.store,
        mail: &f.mail,
        run: &run_row,
        def: &def,
    };
    let step = StepRow {
        run,
        node: node.into(),
        task: format!("t-{node}"),
        state: StepState::Waiting,
        verdict: None,
    };
    match AuditStart.start(&cx, &step)? {
        StartAction::Job(spec) => Ok(*spec),
        other => panic!("expected a job, got {other:?}"),
    }
}

#[test]
fn a_first_pass_is_a_read_only_auditor_job_with_range_and_plan() {
    let f = fixture();
    with_branch(&f);
    plan(&f);
    let spec = start(&f, "audit1").unwrap();
    assert_eq!(spec.role, "auditor");
    assert_eq!(spec.kind, "flow-audit");
    assert!(spec.read_only);
    assert_eq!(spec.builtin_role.as_deref(), Some("auditor"));
    assert_eq!(spec.target.as_deref(), Some("default"));
    assert_eq!(spec.task.as_deref(), Some("t-audit1"));
    assert!(spec.body.starts_with("Pump verify mode"));
    assert!(spec.body.contains(&format!("{BASE}..{TIP}")));
    assert!(spec.body.contains("vivi/need/need-1"));
    assert!(spec.body.contains("PLAN TEXT"));
    assert!(spec.body.contains("reply-1: also keep the CLI shape"));
    assert!(spec.body.contains("need-1-r0"));
    assert!(spec.body.contains("in parallel"));
    assert!(!spec.body.contains("Second pass"));
}

#[test]
fn a_second_pass_carries_the_prior_audit_and_the_rework_slices() {
    let f = fixture();
    with_branch(&f);
    plan(&f);
    payload(
        &f,
        "t-audit1",
        "audit1",
        "auditor",
        json!({"front": {"answer": "defects"}, "body": "PRIOR FINDINGS"}),
    );
    payload(
        &f,
        "t-rework1",
        "rework1",
        "planner",
        json!({"front": {"slice": [{"id": "fix-1", "title": "Fix the thing"}]}}),
    );
    let spec = start(&f, "audit2").unwrap();
    assert!(spec.body.contains("Second pass"));
    assert!(spec.body.contains("PRIOR FINDINGS"));
    assert!(spec.body.contains("defects"));
    assert!(spec.body.contains("fix-1"));
}

fn run_one_audit(f: &Fixture) {
    payload_in(
        f,
        1,
        "t-audit1",
        "audit1",
        "auditor",
        json!({"front": {"finding": [{"id": "F-1"}]}, "body": "RUN ONE BODY"}),
    );
}

#[test]
fn a_second_pass_after_a_rework_restart_finds_the_audit_of_the_earlier_run() {
    let f = fixture();
    with_branch(&f);
    plan(&f);
    run_one_audit(&f);
    payload_in(
        &f,
        2,
        "t-rework1-r2",
        "rework1",
        "slicer",
        json!({"front": {"slice": [{"id": "rw-1", "title": "Rework it"}]}}),
    );
    let spec = start_in(&f, 2, "audit2").unwrap();
    assert!(
        spec.body.contains("## Second pass: prior audit (run 1)"),
        "{}",
        spec.body
    );
    assert!(spec.body.contains("F-1"));
    assert!(spec.body.contains("RUN ONE BODY"));
    assert!(spec.body.contains("## Rework slices"));
    assert!(spec.body.contains("rw-1"));
}

#[test]
fn a_first_pass_never_falls_back_to_an_earlier_run() {
    let f = fixture();
    with_branch(&f);
    plan(&f);
    run_one_audit(&f);
    let spec = start_in(&f, 2, "audit1").unwrap();
    assert!(!spec.body.contains("Second pass"), "{}", spec.body);
    assert!(!spec.body.contains("F-1"));
}

#[test]
fn no_need_branch_row_is_an_error_naming_the_need() {
    let f = fixture();
    let err = start(&f, "audit1").unwrap_err();
    assert!(err.to_string().contains(NEED), "{err}");
}

#[test]
fn the_auditor_class_registers_for_build() {
    let mut registry: Registry<crate::pump::state::State, FakeMail> = Registry::new();
    super::register(&mut registry);
    let node = crate::pump::flow::def::FlowNode {
        id: "audit1".into(),
        class: NodeClass::Auditor,
        label: String::new(),
    };
    assert!(registry.handler_for(FlowKind::Build, &node).is_some());
}
