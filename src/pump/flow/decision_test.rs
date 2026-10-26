//! Tests for the decision-seat resolver, the wait inventory, and the
//! delivery of Mind decision notices from the tick.

use std::sync::Arc;

use super::delivery::{DECISION_UNROUTABLE, REQUEST_MARK, deliver, deliver_to};
use super::{Wait, WaitKind, inventory, recipient};
use crate::error::VivariumError;
use crate::mailspace::{Mailspace, MailspaceThreadMessage, RoleUpdate, SendRequest};
use crate::pump::config::PumpConfig;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState};
use crate::pump::host::Ctx;
use crate::pump::store::DB_FILENAME;
use crate::pump::store::PumpStore;
use crate::pump::store::step_payload::StepPayloadRow;

/// A project whose pump store is open: rows go in, [`inventory`] reads them
/// back, and `ctx.exception` records issues the way the tick does.
struct Rig {
    project: Project,
    store: PumpStore,
}

impl Rig {
    fn open() -> Rig {
        let project = project();
        project.pump_ctx();
        let store = PumpStore::open(&project.home().join(DB_FILENAME)).unwrap();
        Rig { project, store }
    }

    /// A need handle to key runs by.
    fn need(&self, subject: &str) -> String {
        self.project.need(subject)
    }

    /// Record an issue the way `raise_once` does.
    fn issue(&self, kind: &str, unit: &str, text: &str) {
        self.project.pump_ctx().exception(kind, unit, text);
    }

    /// A live run of `kind`.
    fn live(&self, need: &str, run: u32, kind: &str) {
        self.run(need, run, kind, "live");
    }

    /// A run in any state.
    fn run(&self, need: &str, run: u32, kind: &str, state: &str) {
        self.store
            .insert_run(&RunRow {
                need: need.into(),
                run,
                flow: kind.into(),
                flow_version: 2,
                kind: kind.into(),
                entry: "plan".into(),
                state: state.into(),
            })
            .unwrap();
    }

    /// A waiting step with `verdict`.
    fn waiting(&self, need: &str, run: u32, node: &str, task: &str, verdict: Option<&str>) {
        self.store
            .insert_step(
                need,
                &StepRow {
                    run,
                    node: node.into(),
                    task: task.into(),
                    state: StepState::Waiting,
                    verdict: verdict.map(str::to_owned),
                },
            )
            .unwrap();
    }

    /// A stored payload with a `front` object.
    fn payload(
        &self,
        need: &str,
        run: u32,
        task: &str,
        node: &str,
        class: &str,
        front: serde_json::Value,
    ) {
        self.store
            .record_step_payload(&StepPayloadRow {
                need: need.into(),
                run,
                node: node.into(),
                class: class.into(),
                task: task.into(),
                payload: serde_json::json!({ "front": front }),
            })
            .unwrap();
    }

    /// A job attempt row.
    fn job(&self, need: &str, job: &str, task: &str, attempt: u32) {
        self.store
            .insert_step_job(
                need,
                &StepJobRow {
                    job: job.into(),
                    run: 1,
                    node: "slice".into(),
                    task: task.into(),
                    attempt,
                },
            )
            .unwrap();
    }
}

/// A config whose decision seat is `seat`.
fn config(seat: &str) -> PumpConfig {
    PumpConfig {
        decision_to: seat.into(),
        ..PumpConfig::default()
    }
}

fn err_of(result: Result<String, VivariumError>) -> String {
    result.err().expect("resolution should fail").to_string()
}

/// A role of kind `kind`.
fn role_of_kind(kind: &str) -> RoleUpdate {
    RoleUpdate {
        kind: Some(Some(kind.into())),
        ..RoleUpdate::default()
    }
}

#[test]
fn the_default_seat_resolves_the_mind_role() {
    let project = project();
    assert_eq!(
        recipient(&project.mailspace, &PumpConfig::default()).unwrap(),
        "mind",
        "the default seat is the mind role"
    );
}

#[test]
fn a_seat_may_name_an_alias_or_the_role_address() {
    let mut project = project();
    project
        .mailspace
        .add_role("chief", role_of_kind("mind"))
        .unwrap();
    project
        .mailspace
        .rename_identity("chief", "deputy")
        .unwrap();
    let mailspace = &project.mailspace;
    assert_eq!(
        recipient(mailspace, &config("chief")).unwrap(),
        "deputy",
        "an alias resolves to the canonical role name"
    );
    assert_eq!(
        recipient(mailspace, &config(&mailspace.address_for("deputy"))).unwrap(),
        "deputy",
        "the role's own address resolves too"
    );
}

#[test]
fn a_blank_seat_fails_without_falling_back() {
    let project = project();
    let mut pump = config("   ");
    pump.from = String::from("operator");
    pump.board.reply_to = Some(String::from("operator@bd371d96.local"));
    let err = err_of(recipient(&project.mailspace, &pump));
    assert!(err.contains("blank"), "{err}");
}

#[test]
fn an_unknown_seat_fails() {
    let project = project();
    let err = err_of(recipient(&project.mailspace, &config("nobody")));
    assert!(err.contains("unknown local role 'nobody'"), "{err}");
}

#[test]
fn a_wrong_kind_seat_fails_even_when_fallbacks_are_configured() {
    let mut project = project();
    project
        .mailspace
        .add_role("chief", role_of_kind("planner"))
        .unwrap();
    let mut pump = config("chief");
    pump.from = String::from("operator");
    pump.board.reply_to = Some(String::from("operator@bd371d96.local"));
    let err = err_of(recipient(&project.mailspace, &pump));
    assert!(err.contains("'chief'"), "{err}");
    assert!(err.contains("planner"), "{err}");
    assert!(err.contains("mind"), "{err}");
}

#[test]
fn a_kindless_seat_fails() {
    let project = project();
    let err = err_of(recipient(&project.mailspace, &config("elsewhere")));
    assert!(err.contains("'elsewhere'"), "{err}");
    assert!(err.contains("none"), "{err}");
}

/// The one wait of a one-run rig, for the common single-wait assertions.
fn one(rig: &Rig) -> Wait {
    let waits = inventory(&rig.store).unwrap();
    assert_eq!(waits.len(), 1, "{waits:#?}");
    waits.into_iter().next().unwrap()
}

#[test]
fn a_waiting_build_lock_reports_the_cto_answer() {
    let rig = Rig::open();
    let need = rig.need("ship it");
    rig.live(&need, 1, "build");
    rig.waiting(&need, 1, "locked", "t-lock", None);
    rig.payload(
        &need,
        1,
        "t-cto",
        "cto",
        "cto",
        serde_json::json!({ "answer": "lock", "reason": "ship is blocked" }),
    );

    let wait = one(&rig);

    assert_eq!(wait.kind, WaitKind::Lock);
    assert_eq!(wait.need, need);
    assert_eq!(wait.run, 1);
    assert_eq!(wait.flow, "build");
    assert_eq!(wait.node, "locked");
    assert_eq!(wait.task, "t-lock");
    assert_eq!(wait.id, format!("lock {need} r1 locked t-lock a0"));
    assert_eq!(wait.job, None);
    assert_eq!(wait.issue, None);
    assert_eq!(wait.reason, "the CTO answered `lock`: ship is blocked");
    assert_eq!(wait.evidence, vec!["payload of cto step t-cto"]);
    assert_eq!(
        wait.controls,
        vec![format!("vivi pump unlock {need} [entry]")]
    );
}

#[test]
fn a_build_lock_without_a_cto_payload_says_so() {
    let rig = Rig::open();
    let need = rig.need("ship it");
    rig.live(&need, 1, "build");
    rig.waiting(&need, 1, "locked", "t-lock", None);

    let wait = one(&rig);

    assert_eq!(wait.reason, "reached without a CTO answer");
    assert_eq!(
        wait.evidence,
        vec![format!("no cto step payload in {need} r1")]
    );
}

#[test]
fn a_waiting_park_reports_the_planner_word_and_reason() {
    let rig = Rig::open();
    let need = rig.need("park me");
    rig.live(&need, 1, "build");
    rig.store
        .insert_step(
            &need,
            &StepRow {
                run: 1,
                node: "plan".into(),
                task: "t-plan".into(),
                state: StepState::Done,
                verdict: Some("hold".into()),
            },
        )
        .unwrap();
    rig.payload(
        &need,
        1,
        "t-plan",
        "plan",
        "planner",
        serde_json::json!({ "reason": "waiting on the schema" }),
    );
    rig.waiting(&need, 1, "parked", "t-park", None);

    let wait = one(&rig);

    assert_eq!(wait.kind, WaitKind::Park);
    assert_eq!(wait.node, "parked");
    assert_eq!(wait.reason, "hold: waiting on the schema");
    assert_eq!(wait.evidence[0], "planner step t-plan verdict hold");
    assert_eq!(
        wait.controls,
        vec![
            "reply on the need thread to restart the run at plan".to_string(),
            "vivi pump answer t-park".to_string()
        ]
    );
}

#[test]
fn a_park_without_a_planner_step_reports_the_absence() {
    let rig = Rig::open();
    let need = rig.need("park me");
    rig.live(&need, 1, "build");
    rig.waiting(&need, 1, "parked", "t-park", None);

    let wait = one(&rig);

    assert_eq!(wait.reason, "no planner step found");
    assert_eq!(wait.evidence, vec!["no planner step found"]);
}

#[test]
fn a_held_slice_row_keeps_the_issue_raised_on_its_task() {
    let rig = Rig::open();
    let need = rig.need("slice it");
    rig.live(&need, 1, "build");
    rig.waiting(&need, 1, "slice:abc", "t-slice", Some("operator-hold"));
    rig.issue(
        "flow-step-error-held",
        "t-slice",
        "held after 3 failed ticks",
    );

    let wait = one(&rig);

    assert_eq!(wait.kind, WaitKind::Hold);
    assert_eq!(wait.node, "slice:abc", "a node the shipped flow has not");
    assert_eq!(wait.issue.as_deref(), Some("flow-step-error-held"));
    assert_eq!(wait.reason, "held after 3 failed ticks");
    assert_eq!(
        wait.evidence,
        vec!["raised flow-step-error-held on t-slice"]
    );
    assert_eq!(wait.controls, vec!["vivi pump answer t-slice"]);
}

#[test]
fn a_hold_without_an_issue_reports_the_absence() {
    let rig = Rig::open();
    let need = rig.need("slice it");
    rig.live(&need, 1, "build");
    rig.waiting(&need, 1, "slice:abc", "t-slice", Some("operator-hold"));

    let wait = one(&rig);

    assert_eq!(wait.issue, None);
    assert_eq!(
        wait.reason,
        "no issue was raised about task t-slice; the hold carries no recorded diagnostic"
    );
    assert_eq!(
        wait.evidence,
        vec!["no raised issue found for task t-slice"]
    );
}

#[test]
fn a_hold_keeps_the_issue_raised_on_its_latest_job() {
    let rig = Rig::open();
    let need = rig.need("slice it");
    rig.live(&need, 1, "build");
    rig.waiting(&need, 1, "slice:abc", "t-slice", Some("operator-hold"));
    rig.job(&need, "j1", "t-slice", 1);
    rig.issue(
        "flow-reslice-failed",
        "j1",
        "the reslice came back unusable",
    );

    let wait = one(&rig);

    let job = wait.job.as_ref().unwrap();
    assert_eq!(job.job, "j1");
    assert_eq!(job.attempt, 1);
    assert_eq!(wait.issue.as_deref(), Some("flow-reslice-failed"));
    assert_eq!(wait.reason, "the reslice came back unusable");
    assert_eq!(wait.id, format!("hold {need} r1 slice:abc t-slice a1"));
}

#[test]
fn a_changed_attempt_changes_the_identity() {
    let rig = Rig::open();
    let need = rig.need("slice it");
    rig.live(&need, 1, "build");
    rig.waiting(&need, 1, "slice:abc", "t-slice", Some("operator-hold"));
    rig.job(&need, "j1", "t-slice", 1);
    let first = one(&rig);

    rig.job(&need, "j2", "t-slice", 2);
    let second = one(&rig);

    assert_ne!(first.id, second.id, "a changed attempt must differ");
    assert!(first.id.ends_with("a1"), "{}", first.id);
    assert!(second.id.ends_with("a2"), "{}", second.id);
    assert_eq!(second.job.unwrap().job, "j2");
}

#[test]
fn a_hold_falls_back_to_the_issue_raised_on_the_run() {
    let rig = Rig::open();
    let need = rig.need("slice it");
    rig.live(&need, 1, "build");
    rig.waiting(&need, 1, "slice:abc", "t-slice", Some("operator-hold"));
    rig.issue(
        "flow-run-error",
        &format!("{need} r1"),
        "the run tick failed",
    );

    let wait = one(&rig);

    assert_eq!(wait.issue.as_deref(), Some("flow-run-error"));
    assert_eq!(wait.reason, "the run tick failed");
    assert_eq!(
        wait.evidence,
        vec![format!("raised flow-run-error on {need} r1")]
    );
}

#[test]
fn a_repair_lock_keeps_the_repair_standing() {
    let rig = Rig::open();
    let need = rig.need("fix main");
    rig.live(&need, 1, "repair");
    rig.waiting(&need, 1, "unrepaired", "t-unrep", None);

    let wait = one(&rig);

    assert_eq!(wait.kind, WaitKind::Lock);
    assert!(wait.reason.contains("no raised alarm"), "{}", wait.reason);
    assert!(wait.reason.contains("unrepaired"), "{}", wait.reason);
    assert!(!wait.reason.contains("CTO"), "{}", wait.reason);
    assert!(
        wait.evidence[0].contains("main stays red"),
        "{}",
        wait.evidence[0]
    );
    assert_eq!(
        wait.controls,
        vec![
            format!("vivi pump unlock {need} [entry]"),
            "make main green at a tip holding the tested tip; the run releases itself".to_string()
        ]
    );
}

#[test]
fn a_repair_lock_keeps_the_raised_alarm() {
    let rig = Rig::open();
    let need = rig.need("fix main");
    rig.live(&need, 1, "repair");
    rig.waiting(&need, 1, "unrepaired", "t-unrep", None);
    let unit = format!("{need} r1");
    rig.issue("flow-main-unrepaired", &unit, "main stays red at tip ab12");

    let wait = one(&rig);

    assert_eq!(wait.issue.as_deref(), Some("flow-main-unrepaired"));
    assert_eq!(wait.reason, "main stays red at tip ab12");
    assert_eq!(
        wait.evidence[1],
        format!("raised flow-main-unrepaired on {need} r1")
    );
}

#[test]
fn a_held_flow_node_reports_as_a_hold_before_its_class() {
    let rig = Rig::open();
    let need = rig.need("park me");
    rig.live(&need, 1, "build");
    rig.waiting(&need, 1, "parked", "t-park", Some("operator-hold"));

    assert_eq!(one(&rig).kind, WaitKind::Hold);
}

#[test]
fn only_live_runs_and_waiting_rows_are_inventoried() {
    let rig = Rig::open();
    let need = rig.need("ship it");
    rig.run(&need, 1, "build", "superseded");
    rig.waiting(&need, 1, "locked", "t-old", None);
    rig.live(&need, 2, "build");
    rig.store
        .insert_step(
            &need,
            &StepRow {
                run: 2,
                node: "locked".into(),
                task: "t-done".into(),
                state: StepState::Done,
                verdict: Some("unlocked".into()),
            },
        )
        .unwrap();
    rig.waiting(&need, 2, "judge", "t-judge", None);
    rig.waiting(&need, 2, "locked", "t-live", None);

    let waits = inventory(&rig.store).unwrap();

    assert_eq!(
        waits.iter().map(|w| w.task.as_str()).collect::<Vec<_>>(),
        vec!["t-live"],
        "a superseded run, a done step and a plain waiting step stay out"
    );
}

#[test]
fn waits_come_back_ordered_by_need_run_and_task() {
    let rig = Rig::open();
    let late = rig.need("second");
    let early = rig.need("first");
    rig.live(&late, 2, "build");
    rig.waiting(&late, 2, "locked", "t-z", None);
    rig.waiting(&late, 2, "parked", "t-a", None);
    rig.live(&early, 1, "build");
    rig.waiting(&early, 1, "locked", "t-m", None);

    let waits = inventory(&rig.store).unwrap();

    let order: Vec<_> = waits
        .iter()
        .map(|w| (w.need.as_str(), w.run, w.task.as_str()))
        .collect();
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(order, sorted);
    assert_eq!(order.len(), 3);
}

#[test]
fn a_run_without_a_shipped_flow_is_refused() {
    let rig = Rig::open();
    let need = rig.need("mystery");
    rig.live(&need, 1, "mystery");

    let err = inventory(&rig.store).unwrap_err().to_string();

    assert!(err.contains("no shipped flow of kind `mystery`"), "{err}");
}

/// Send a step task on the fixture mailspace and return the recipient copy's
/// handle: the thread the pump would reply on.
fn task_handle(project: &Project, subject: &str) -> String {
    project
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: crate::letter::fixture::task_body("the work"),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

/// A pump context over `project` whose config is `cfg`.
fn ctx_of(project: &Project, cfg: PumpConfig) -> Ctx {
    let mut ctx = project.pump_ctx();
    ctx.cfg = Arc::new(cfg);
    ctx
}

/// The one notice the pump posted on `task`'s thread.
fn notice_on(mailspace: &Mailspace, task: &str) -> MailspaceThreadMessage {
    let thread = mailspace
        .thread(task, false, usize::MAX, usize::MAX)
        .unwrap();
    let notices: Vec<_> = thread.iter().filter(|m| m.handle != task).collect();
    assert_eq!(
        notices.len(),
        1,
        "expected one notice on {task}: {thread:?}"
    );
    notices[0].clone()
}

/// A CTO payload that locks the build.
fn cto_lock(rig: &Rig, need: &str) {
    rig.payload(
        need,
        1,
        "t-cto",
        "cto",
        "cto",
        serde_json::json!({ "answer": "lock", "reason": "ship is blocked" }),
    );
}

/// A live build run waiting at its lock on `task`, with the CTO answer read.
fn locked_rig() -> (Rig, String, String) {
    let rig = Rig::open();
    let need = rig.need("ship it");
    let task = task_handle(&rig.project, "lock the build");
    rig.live(&need, 1, "build");
    rig.waiting(&need, 1, "locked", &task, None);
    cto_lock(&rig, &need);
    (rig, need, task)
}

#[test]
fn cto_lock_is_addressed_to_the_mind() {
    let (rig, need, task) = locked_rig();
    let ctx = ctx_of(&rig.project, PumpConfig::default());
    let mind = rig.project.mailspace.address_for("mind");
    let operator = rig.project.mailspace.address_for("operator");

    deliver_to(&rig.project.mailspace, &ctx, &rig.store).unwrap();

    let notice = notice_on(&rig.project.mailspace, &task);
    assert_eq!(notice.from, mind, "the pump posts as its own identity");
    assert_eq!(notice.to, mind, "addressed to the decision seat only");
    assert!(notice.cc.is_empty(), "no operator copy: {}", notice.cc);
    assert!(!notice.to.contains(&operator), "no operator copy");
    let body = &notice.body;
    let request = format!("{REQUEST_MARK}lock {need} r1 locked {task} a0");
    assert!(body.contains(&request), "{body}");
    assert!(
        body.contains("the CTO answered `lock`: ship is blocked"),
        "{body}"
    );
    assert!(
        body.contains("evidence:\n- payload of cto step t-cto"),
        "{body}"
    );
    assert!(body.contains("issue: none raised"), "{body}");
    assert!(body.contains(&format!("task {task}")), "{body}");
    assert!(
        body.contains(&format!("vivi pump unlock {need} [entry]")),
        "{body}"
    );
}

#[test]
fn park_and_held_step_are_addressed_to_the_mind() {
    let rig = Rig::open();
    let need = rig.need("park me");
    let park_task = task_handle(&rig.project, "parked");
    let held_task = task_handle(&rig.project, "held slice");
    rig.live(&need, 1, "build");
    rig.waiting(&need, 1, "parked", &park_task, None);
    rig.waiting(&need, 1, "slice:abc", &held_task, Some("operator-hold"));
    rig.issue(
        "flow-step-error-held",
        &held_task,
        "held after 3 failed ticks",
    );
    let ctx = ctx_of(&rig.project, PumpConfig::default());
    let mind = rig.project.mailspace.address_for("mind");
    let operator = rig.project.mailspace.address_for("operator");

    deliver_to(&rig.project.mailspace, &ctx, &rig.store).unwrap();

    let park = notice_on(&rig.project.mailspace, &park_task);
    let held = notice_on(&rig.project.mailspace, &held_task);
    for notice in [&park, &held] {
        assert_eq!(notice.from, mind, "the pump posts as its own identity");
        assert_eq!(notice.to, mind, "addressed to the decision seat only");
        assert!(notice.cc.is_empty(), "no operator copy: {}", notice.cc);
        assert!(!notice.to.contains(&operator), "no operator copy");
    }
    let park_request = format!("{REQUEST_MARK}park {need} r1 parked {park_task} a0");
    assert!(park.body.contains(&park_request), "{}", park.body);
    assert!(park.body.contains("control choices:"), "{}", park.body);
    let held_request = format!("{REQUEST_MARK}hold {need} r1 slice:abc {held_task} a0");
    assert!(held.body.contains(&held_request), "{}", held.body);
    assert!(
        held.body.contains("issue: flow-step-error-held"),
        "{}",
        held.body
    );
    assert!(
        held.body.contains("raised flow-step-error-held"),
        "{}",
        held.body
    );
    assert!(
        held.body.contains(&format!("vivi pump answer {held_task}")),
        "{}",
        held.body
    );
}

#[test]
fn decision_delivery_is_retryable_and_restart_safe() {
    let (rig, need, task) = locked_rig();
    let ctx = ctx_of(&rig.project, PumpConfig::default());

    // A send that fails is recorded nowhere: an unknown posting identity
    // refuses the reply outright, and the thread keeps no receipt.
    let bad = ctx_of(
        &rig.project,
        PumpConfig {
            reply_from: String::from("ghost"),
            ..PumpConfig::default()
        },
    );
    let error = deliver_to(&rig.project.mailspace, &bad, &rig.store)
        .expect_err("a failed send must be an error");
    assert!(error.to_string().contains("ghost"), "{error}");
    let thread = rig
        .project
        .mailspace
        .thread(&task, false, usize::MAX, usize::MAX)
        .unwrap();
    assert_eq!(thread.len(), 1, "a failed send leaves no receipt");
    assert!(
        !rig.store
            .exceptions()
            .unwrap()
            .iter()
            .any(|(_, kind, _, _)| kind == DECISION_UNROUTABLE),
        "the seat was fine; only the send failed"
    );

    // The retry delivers exactly once.
    deliver_to(&rig.project.mailspace, &ctx, &rig.store).unwrap();
    assert!(
        notice_on(&rig.project.mailspace, &task)
            .body
            .contains(&format!("{REQUEST_MARK}lock {need} r1 locked {task} a0"))
    );

    // A repeated tick is a no-op.
    deliver_to(&rig.project.mailspace, &ctx, &rig.store).unwrap();

    // A later attempt is a new request and gets its own notice.
    rig.job(&need, "j1", &task, 1);
    deliver_to(&rig.project.mailspace, &ctx, &rig.store).unwrap();
    let thread = rig
        .project
        .mailspace
        .thread(&task, false, usize::MAX, usize::MAX)
        .unwrap();
    let notices: Vec<_> = thread.iter().filter(|m| m.handle != task).collect();
    assert_eq!(notices.len(), 2, "the later attempt got its own notice");
    assert!(notices[0].body.contains("a0"), "{}", notices[0].body);
    assert!(notices[1].body.contains("a1"), "{}", notices[1].body);

    // Reopening the state from disk and ticking again sends nothing: the
    // receipts live in the mail, not in memory.
    let store = PumpStore::open(&rig.project.home().join(DB_FILENAME)).unwrap();
    let mailspace = Mailspace::discover(Some(rig.project.root())).unwrap();
    deliver_to(&mailspace, &ctx, &store).unwrap();
    let thread = mailspace
        .thread(&task, false, usize::MAX, usize::MAX)
        .unwrap();
    let notices: Vec<_> = thread.iter().filter(|m| m.handle != task).collect();
    assert_eq!(notices.len(), 2, "no duplicate after reopen");

    // Nothing else moved: the run still waits, the step is untouched, and
    // the thread holds notices only, never a close or a reopen.
    let runs = rig.store.runs_of(&need).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, "live");
    let steps = rig.store.steps_of(&need, 1).unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].state, StepState::Waiting);
    assert_eq!(steps[0].verdict, None);
    assert!(thread.iter().all(|m| m.event.is_none()), "{thread:?}");
}

#[test]
fn invalid_decision_seat_never_falls_back_to_operator() {
    let (rig, need, task) = locked_rig();
    let cfg = PumpConfig {
        decision_to: String::from("elsewhere"),
        ..PumpConfig::default()
    };
    let ctx = ctx_of(&rig.project, cfg);

    deliver_to(&rig.project.mailspace, &ctx, &rig.store).unwrap();
    deliver_to(&rig.project.mailspace, &ctx, &rig.store).unwrap();

    let rows = rig.store.exceptions().unwrap();
    let unroutable: Vec<_> = rows
        .iter()
        .filter(|(_, kind, _, _)| kind == DECISION_UNROUTABLE)
        .collect();
    assert_eq!(unroutable.len(), 1, "{rows:?}");
    assert_eq!(unroutable[0].2, "elsewhere");
    let text = &unroutable[0].3;
    assert!(text.contains("elsewhere"), "{text}");
    assert!(text.contains("keep waiting"), "{text}");
    let thread = rig
        .project
        .mailspace
        .thread(&task, false, usize::MAX, usize::MAX)
        .unwrap();
    assert_eq!(thread.len(), 1, "only the step task is on the thread");
    assert!(!thread[0].to.contains("operator"), "no operator mail");
    let steps = rig.store.steps_of(&need, 1).unwrap();
    assert_eq!(steps[0].state, StepState::Waiting, "the work keeps waiting");
}

#[test]
fn the_tick_delivers_through_the_project_mailspace() {
    let (rig, need, task) = locked_rig();
    let ctx = ctx_of(&rig.project, PumpConfig::default());

    deliver(&ctx, &rig.store).unwrap();

    let notice = notice_on(&rig.project.mailspace, &task);
    assert!(
        notice
            .body
            .contains(&format!("{REQUEST_MARK}lock {need} r1 locked {task} a0"))
    );
}
