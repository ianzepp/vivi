//! Tests for the cto step handler.

use std::sync::Arc;

use serde_json::json;
use tempfile::{TempDir, tempdir};

use super::CtoStart;
use crate::letter::records::NoteWord;
use crate::mailspace::pump_note::note_body;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::{FlowKind, FlowNode, NodeClass};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
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
    fixture_with(PumpConfig::default())
}

/// A fixture over `cfg`, so tests can shape the dispatch budgets.
fn fixture_with(cfg: PumpConfig) -> Fixture {
    let dir = tempdir().expect("tempdir");
    let (ctx, host) = test_ctx(dir.path(), cfg);
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
        flow_version: 2,
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

/// A readable slice task in the fake mailspace: its handle.
fn slice_task(f: &mut Fixture) -> String {
    f.mail
        .send_step("pump", "hand", "slice alpha", "slice body", &[])
        .unwrap()
}

/// Index `handle` as the step row of a slice node, as a fanout would, so a
/// thread read through the mail port sees the actual handle.
fn add_slice_step(f: &Fixture, run: u32, node: &str, handle: &str, verdict: Option<&str>) {
    f.store
        .insert_step(
            NEED,
            &StepRow {
                run,
                node: node.into(),
                task: handle.to_string(),
                state: StepState::Done,
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
    assert!(spec.read_only);
    assert_eq!(spec.from_ref.as_deref(), Some(f.ctx.cfg.main.as_str()));
    assert_eq!(
        spec.extra_targets,
        crate::pump::flow::step_inputs::targets(&f.ctx)
    );
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
fn a_configured_head_cto_override_does_not_reach_the_cto_job() {
    // The flow cto step is seated at head-cto but budgets by its execution
    // kind, RoleKind::Cto, which carries no step_budgets key: the job keeps
    // the shared budget, so resolving from the head-cto seat would fail.
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 611;
    cfg.dispatch.step_budgets.head_cto = Some(888);
    cfg.dispatch.step_budgets.auditor = Some(44);
    cfg.dispatch.step_budgets.judge = Some(55);
    let f = fixture_with(cfg);
    let run = two_runs(&f, "defects");
    let spec = start(&f, &run);
    assert_eq!(spec.budget, 611);
}

#[test]
fn the_cto_job_follows_a_changed_dispatch_fallback_despite_unrelated_overrides() {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 4242;
    cfg.dispatch.step_budgets.planner = Some(11);
    cfg.dispatch.step_budgets.auditor = Some(44);
    cfg.dispatch.step_budgets.judge = Some(55);
    let f = fixture_with(cfg);
    let run = two_runs(&f, "defects");
    let spec = start(&f, &run);
    assert_eq!(spec.budget, 4242);
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

#[test]
fn missing_attempt_notes_do_not_prevent_cto_dispatch() {
    let mut f = fixture();
    let run = two_runs(&f, "defects");
    let task = slice_task(&mut f);
    add_slice_step(&f, 2, "slice:alpha", &task, Some("merged"));
    let spec = start(&f, &run);
    assert_eq!(spec.role, "head-cto");
    assert!(spec.read_only);
    assert_eq!(spec.kind, "flow-cto");
    assert!(
        spec.body.contains("## Slice attempt records"),
        "{}",
        spec.body
    );
    assert!(
        spec.body
            .contains("## Slice attempt records\n\n(none recorded)"),
        "{}",
        spec.body
    );
}

#[test]
fn a_typed_attempt_note_reaches_the_brief_with_source_and_outcome() {
    let mut f = fixture();
    let run = two_runs(&f, "defects");
    let task = slice_task(&mut f);
    f.mail
        .reply(
            &task,
            &note_body(
                NoteWord::Attempt,
                Some("agent failed: proof-failed"),
                "pump: attempt record\nfailing command:\n  cargo test --lib\nexcerpt:\n  error: test failed",
            )
            .unwrap(),
        )
        .unwrap();
    f.mail
        .reply(
            &task,
            &note_body(
                NoteWord::Attempt,
                Some("delivered"),
                "second attempt landed",
            )
            .unwrap(),
        )
        .unwrap();
    add_slice_step(&f, 1, "slice:alpha", &task, Some("merged"));
    let spec = start(&f, &run);
    let first = format!("run 1 slice:alpha (task {task}, outcome agent failed: proof-failed):");
    assert!(spec.body.contains(&first), "{}", spec.body);
    assert!(
        spec.body.contains("failing command:\n  cargo test --lib"),
        "{}",
        spec.body
    );
    assert!(spec.body.contains("error: test failed"));
    let second =
        format!("run 1 slice:alpha (task {task}, outcome delivered):\nsecond attempt landed");
    assert!(spec.body.contains(&second), "{}", spec.body);
    let head = spec.body.find(&first).expect("first record");
    let tail = spec.body.find(&second).expect("second record");
    assert!(head < tail, "notes must keep their thread order");
    assert!(!spec.body.contains("[...]"), "{}", spec.body);
}

#[test]
fn an_oversized_attempt_note_keeps_its_tail_and_the_marker_per_record() {
    let mut f = fixture();
    let run = two_runs(&f, "defects");
    for (node, tail) in [("slice:alpha", "ALPHA-TAIL"), ("slice:beta", "BETA-TAIL")] {
        let task = slice_task(&mut f);
        f.mail
            .reply(
                &task,
                &note_body(
                    NoteWord::Attempt,
                    Some("agent failed: timeout"),
                    &format!("{}{tail}", "x".repeat(12_500)),
                )
                .unwrap(),
            )
            .unwrap();
        add_slice_step(&f, 1, node, &task, Some("exhausted"));
    }
    let spec = start(&f, &run);
    assert_eq!(spec.body.matches("[...]").count(), 2, "{}", spec.body);
    assert!(spec.body.contains("ALPHA-TAIL"));
    assert!(spec.body.contains("BETA-TAIL"));
    assert!(!spec.body.contains(&"x".repeat(12_500)));
}

fn slice_link(f: &Fixture, task: &str, fanout: &str) {
    slice_link_on(f, 1, task, fanout);
}

/// Link `task` to the fanout task `fanout` of `run`, as a fanout would.
fn slice_link_on(f: &Fixture, run: u32, task: &str, fanout: &str) {
    f.store
        .link_slice(&SliceLink {
            task: task.into(),
            need: NEED.into(),
            run,
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

#[test]
fn with_no_need_branch_row_the_brief_says_rework_is_unavailable() {
    let f = fixture();
    let run = two_runs(&f, "defects");
    let spec = start(&f, &run);
    assert!(
        spec.body.contains("## Rework availability"),
        "{}",
        spec.body
    );
    assert!(
        spec.body.contains("No need-branch row exists"),
        "{}",
        spec.body
    );
    assert!(spec.body.contains("rework is unavailable"), "{}", spec.body);
    assert!(
        spec.body
            .contains("Research, plan, slice and lock remain the choices"),
        "{}",
        spec.body
    );
}

#[test]
fn a_stored_need_branch_row_names_the_branch_and_target_for_rework() {
    let f = fixture();
    let run = two_runs(&f, "defects");
    f.store
        .upsert_need_branch(&NeedBranchRow {
            need: NEED.into(),
            branch: "vivi/need/need-1-reworkable".into(),
            base: None,
            status: "live".into(),
            run: run.run,
            target: "gate-distinctive".into(),
            priors: Vec::new(),
        })
        .unwrap();
    let spec = start(&f, &run);
    assert!(
        spec.body.contains("## Rework availability"),
        "{}",
        spec.body
    );
    assert!(
        spec.body.contains("vivi/need/need-1-reworkable"),
        "{}",
        spec.body
    );
    assert!(spec.body.contains("gate-distinctive"), "{}", spec.body);
    assert!(
        spec.body.contains("rework restarts the need at rework1"),
        "{}",
        spec.body
    );
    assert!(
        !spec.body.contains("rework is unavailable"),
        "{}",
        spec.body
    );
}

/// Post one typed note of `word` with `reference` and `prose` on `handle`.
fn post_note(f: &mut Fixture, word: NoteWord, handle: &str, reference: Option<&str>, prose: &str) {
    f.mail
        .reply(handle, &note_body(word, reference, prose).unwrap())
        .unwrap();
}

/// Index `handle` as the step row of `node` under `need` with an explicit
/// state, so any row can carry a thread the mail port can read back.
fn index_step(
    f: &Fixture,
    need: &str,
    run: u32,
    node: &str,
    handle: &str,
    state: StepState,
    verdict: Option<&str>,
) {
    f.store
        .insert_step(
            need,
            &StepRow {
                run,
                node: node.into(),
                task: handle.to_string(),
                state,
                verdict: verdict.map(str::to_string),
            },
        )
        .unwrap();
}

/// The `## Slice attempt records` section of `body`, cut before the next
/// heading, so ordinary run history cannot contaminate section assertions.
fn attempt_section(body: &str) -> &str {
    let head = body
        .find("\n## Slice attempt records\n")
        .expect("attempt-record section");
    let rest = &body[head..];
    let end = rest
        .find("\n## Rework availability")
        .expect("rework section");
    &rest[..end]
}

/// The prose block of the record headed `header` in `section`: everything
/// between the header line and the blank line before the next record.
fn record_prose<'a>(section: &'a str, header: &str) -> &'a str {
    let start = section.find(header).expect("record header") + header.len();
    let rest = section[start..].strip_prefix('\n').expect("prose line");
    let end = rest.find("\n\n").unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn all_attempts_survive_across_slices_and_runs() {
    let mut f = fixture();
    let run = two_runs(&f, "defects");
    // Run 1, a slice replaced before it finished: its row stays failed, yet
    // its attempt note must travel.
    let zeta = slice_task(&mut f);
    post_note(
        &mut f,
        NoteWord::Attempt,
        &zeta,
        Some("agent failed: proof"),
        "ATTEMPT-ZETA-RUN1",
    );
    index_step(&f, NEED, 1, "slice:zeta", &zeta, StepState::Failed, None);
    // Run 1, a slice whose first attempt failed and whose row now holds the
    // current merged verdict: both thread attempts survive the verdict.
    let alpha = slice_task(&mut f);
    post_note(
        &mut f,
        NoteWord::Attempt,
        &alpha,
        Some("agent failed: proof"),
        "ATTEMPT-ALPHA-FAIL",
    );
    post_note(
        &mut f,
        NoteWord::Attempt,
        &alpha,
        Some("delivered"),
        "ATTEMPT-ALPHA-LANDED",
    );
    index_step(
        &f,
        NEED,
        1,
        "slice:alpha",
        &alpha,
        StepState::Done,
        Some("merged"),
    );
    // Run 2, the current run: slices under both fanouts, inserted in an order
    // that differs from lexical node order, so a node sort cannot pass.
    add_step(&f, 2, "build1", StepState::Done, Some("built"));
    add_step(&f, 2, "build2", StepState::Done, Some("built"));
    let two = slice_task(&mut f);
    post_note(
        &mut f,
        NoteWord::Attempt,
        &two,
        Some("delivered"),
        "ATTEMPT-TWO-RUN2",
    );
    index_step(
        &f,
        NEED,
        run.run,
        "slice:two",
        &two,
        StepState::Done,
        Some("merged"),
    );
    slice_link_on(&f, run.run, &two, "t-build2-r2");
    let one = slice_task(&mut f);
    post_note(
        &mut f,
        NoteWord::Attempt,
        &one,
        Some("agent failed: timeout"),
        "ATTEMPT-ONE-RUN2",
    );
    index_step(
        &f,
        NEED,
        run.run,
        "slice:one",
        &one,
        StepState::Done,
        Some("merged"),
    );
    slice_link_on(&f, run.run, &one, "t-build1-r2");
    let spec = start(&f, &run);
    let section = attempt_section(&spec.body);
    let order = [
        format!("run 1 slice:zeta (task {zeta}, outcome agent failed: proof):"),
        format!("run 1 slice:alpha (task {alpha}, outcome agent failed: proof):"),
        format!("run 1 slice:alpha (task {alpha}, outcome delivered):"),
        format!("run {} slice:two (task {two}, outcome delivered):", run.run),
        format!(
            "run {} slice:one (task {one}, outcome agent failed: timeout):",
            run.run
        ),
    ];
    for header in &order {
        assert_eq!(section.matches(header.as_str()).count(), 1, "{section}");
    }
    for (before, after) in order.iter().zip(order.iter().skip(1)) {
        assert!(
            section.find(before.as_str()) < section.find(after.as_str()),
            "{section}"
        );
    }
    for marker in [
        "ATTEMPT-ZETA-RUN1",
        "ATTEMPT-ALPHA-FAIL",
        "ATTEMPT-ALPHA-LANDED",
        "ATTEMPT-TWO-RUN2",
        "ATTEMPT-ONE-RUN2",
    ] {
        assert_eq!(section.matches(marker).count(), 1, "{section}");
    }
}

#[test]
fn only_slice_attempt_notes_are_carried() {
    let mut f = fixture();
    let run = two_runs(&f, "defects");
    // The intended record with an unfamiliar outcome reference and no record
    // heading in its prose: still carried, reference and all.
    let gamma = slice_task(&mut f);
    post_note(
        &mut f,
        NoteWord::Attempt,
        &gamma,
        Some("weird outcome: unheard-of"),
        "KEEP-GAMMA",
    );
    index_step(
        &f,
        NEED,
        run.run,
        "slice:gamma",
        &gamma,
        StepState::Done,
        Some("merged"),
    );
    // A second intended record whose thread mixes other typed notes and an
    // ordinary reply citing the section heading itself.
    let delta = slice_task(&mut f);
    post_note(&mut f, NoteWord::Attempt, &delta, None, "KEEP-DELTA");
    post_note(
        &mut f,
        NoteWord::Deviations,
        &delta,
        Some("deviated"),
        "SKIP-DEVIATIONS",
    );
    post_note(
        &mut f,
        NoteWord::SliceMerge,
        &delta,
        Some("merged"),
        "SKIP-SLICEMERGE",
    );
    f.mail
        .reply(
            &delta,
            "an ordinary reply citing ## Slice attempt records in its prose",
        )
        .unwrap();
    index_step(
        &f,
        NEED,
        run.run,
        "slice:delta",
        &delta,
        StepState::Done,
        Some("merged"),
    );
    // Attempt notes off this need's slice rows: a non-slice step of this need
    // and a slice row of another need.
    let built = slice_task(&mut f);
    post_note(
        &mut f,
        NoteWord::Attempt,
        &built,
        Some("agent failed: proof"),
        "SKIP-NONSLICE",
    );
    index_step(
        &f,
        NEED,
        1,
        "build1",
        &built,
        StepState::Done,
        Some("built"),
    );
    let other = slice_task(&mut f);
    post_note(
        &mut f,
        NoteWord::Attempt,
        &other,
        Some("delivered"),
        "SKIP-OTHER-NEED",
    );
    index_step(
        &f,
        "need-other",
        1,
        "slice:other",
        &other,
        StepState::Done,
        Some("merged"),
    );
    let spec = start(&f, &run);
    let section = attempt_section(&spec.body);
    let kept = [
        format!(
            "run {} slice:gamma (task {gamma}, outcome weird outcome: unheard-of):",
            run.run
        ),
        format!("run {} slice:delta (task {delta}, outcome none):", run.run),
    ];
    for header in &kept {
        assert_eq!(section.matches(header.as_str()).count(), 1, "{section}");
    }
    assert_eq!(section.matches("(task ").count(), 2, "{section}");
    assert_eq!(section.matches("KEEP-GAMMA").count(), 1, "{section}");
    assert_eq!(section.matches("KEEP-DELTA").count(), 1, "{section}");
    for absent in [
        "SKIP-DEVIATIONS",
        "SKIP-SLICEMERGE",
        "SKIP-NONSLICE",
        "SKIP-OTHER-NEED",
        "an ordinary reply citing",
    ] {
        assert!(!spec.body.contains(absent), "{}", spec.body);
    }
    // The one heading is the section's own: the searched prose never entered.
    assert_eq!(spec.body.matches("## Slice attempt records").count(), 1);
}

#[test]
fn attempt_records_are_capped_independently_and_utf8_safely() {
    let mut f = fixture();
    let run = two_runs(&f, "defects");
    // Two oversized multibyte proses whose naive 12,000-byte cut lands inside
    // one code point, an exactly-at-cap record, and a later short record.
    let one = slice_task(&mut f);
    post_note(
        &mut f,
        NoteWord::Attempt,
        &one,
        Some("agent failed: big"),
        &format!("DROP!{}\u{65e5}", "\u{65e5}".repeat(4000)),
    );
    index_step(
        &f,
        NEED,
        1,
        "slice:big-one",
        &one,
        StepState::Done,
        Some("exhausted"),
    );
    let two = slice_task(&mut f);
    post_note(
        &mut f,
        NoteWord::Attempt,
        &two,
        Some("agent failed: big"),
        &format!("DROP!{}\u{1f980}", "\u{1f980}".repeat(3000)),
    );
    index_step(
        &f,
        NEED,
        run.run,
        "slice:big-two",
        &two,
        StepState::Done,
        Some("exhausted"),
    );
    let whole = "b".repeat(12_000);
    let at_cap = slice_task(&mut f);
    post_note(
        &mut f,
        NoteWord::Attempt,
        &at_cap,
        Some("delivered"),
        &whole,
    );
    index_step(
        &f,
        NEED,
        run.run,
        "slice:at-cap",
        &at_cap,
        StepState::Done,
        Some("merged"),
    );
    let short = slice_task(&mut f);
    post_note(
        &mut f,
        NoteWord::Attempt,
        &short,
        Some("delivered"),
        "TAIL-NOTE",
    );
    index_step(
        &f,
        NEED,
        run.run,
        "slice:short",
        &short,
        StepState::Done,
        Some("merged"),
    );
    let spec = start(&f, &run);
    assert_eq!(spec.body.matches("[...]").count(), 2, "{}", spec.body);
    assert!(!spec.body.contains("DROP"), "{}", spec.body);
    let section = attempt_section(&spec.body);
    let one_header = format!("run 1 slice:big-one (task {one}, outcome agent failed: big):");
    let two_header = format!(
        "run {} slice:big-two (task {two}, outcome agent failed: big):",
        run.run
    );
    // Each oversized record keeps its exact multibyte tail: the equality
    // proves the cut landed on a code point boundary. Task identity and typed
    // outcome stay visible in the header, outside the capped prose.
    let one_prose = record_prose(section, &one_header);
    assert_eq!(
        one_prose,
        format!("[...]\n{}\u{65e5}", "\u{65e5}".repeat(3999))
    );
    let two_prose = record_prose(section, &two_header);
    assert_eq!(
        two_prose,
        format!("[...]\n{}\u{1f980}", "\u{1f980}".repeat(2999))
    );
    // Each rendered prose entry fits its own cap plus the marker, measured
    // separately, never against the source label or the whole brief.
    for prose in [one_prose, two_prose] {
        assert!(prose.len() <= 12_000 + "[...]\n".len(), "{}", prose.len());
        assert!(prose.strip_prefix("[...]\n").unwrap_or("").len() <= 12_000);
    }
    // The below-cap and exact-boundary records pass through byte-identical,
    // and the short record stays complete after the oversized ones.
    let cap_header = format!(
        "run {} slice:at-cap (task {at_cap}, outcome delivered):",
        run.run
    );
    assert_eq!(record_prose(section, &cap_header), whole);
    let short_header = format!(
        "run {} slice:short (task {short}, outcome delivered):",
        run.run
    );
    assert_eq!(record_prose(section, &short_header), "TAIL-NOTE");
    assert!(
        section.find(&short_header) > section.find(&two_header),
        "{section}"
    );
}
