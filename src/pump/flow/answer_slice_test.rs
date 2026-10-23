//! Tests for answers on slice tasks and for transient routing errors: a slice
//! answer settles through the fanout and does not route, and a failed write
//! leaves the letter unapplied for the next tick.

use std::fs;

use super::{ANSWER_REFUSED, ANSWER_RETRYING, apply_pending};
use crate::error::VivariumError;
use crate::mailspace::SendRequest;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::advance::route::JoinInput;
use crate::pump::flow::def::{FlowKind, FlowNode, STEP_MARKER};
use crate::pump::flow::fake_mail::{Call, FakeMail, FakeTask};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailWriter, TaskState, TaskStatus};
use crate::pump::host::Ctx;
use crate::pump::store::{DB_FILENAME, PumpStore};

struct Rig {
    project: Project,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    slice: String,
    build: String,
}

/// A real need with an open pump record, a running `slice:s1` step and a
/// running `build1` step, each with a real mailspace task mirrored in the
/// fake port and the step index.
fn rig() -> Rig {
    let project = project();
    let ctx = project.pump_ctx();
    let store = PumpStore::open(&project.home().join(DB_FILENAME)).unwrap();
    let need = project.need("fix auth");
    project.mailspace.pump_send(None, &need, None).unwrap();
    let mut mail = FakeMail::new("pump");
    let sentinel = mail
        .send_step("pump", "pump", "run", "pump-run-sentinel: x r1", &[])
        .unwrap();
    store
        .insert_run(&RunRow {
            need: need.clone(),
            run: 1,
            flow: "build".into(),
            flow_version: 1,
            kind: "build".into(),
            entry: "plan".into(),
            state: "live".into(),
        })
        .unwrap();
    insert_row(&store, &need, "sentinel", &sentinel);
    let slice = step_task(&project, &mut mail, &store, &need, "slice:s1");
    let build = step_task(&project, &mut mail, &store, &need, "build1");
    mail.calls.clear();
    Rig {
        project,
        ctx,
        store,
        mail,
        slice,
        build,
    }
}

fn insert_row(store: &PumpStore, need: &str, node: &str, task: &str) {
    store
        .insert_step(
            need,
            &StepRow {
                run: 1,
                node: node.into(),
                task: task.into(),
                state: StepState::Running,
                verdict: None,
            },
        )
        .unwrap();
}

/// A real open step task of `node`, mirrored in the fake port and the index.
fn step_task(
    project: &Project,
    mail: &mut FakeMail,
    store: &PumpStore,
    need: &str,
    node: &str,
) -> String {
    let body = format!("{STEP_MARKER} {need} r1 {node}");
    let task = project
        .mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: node.into(),
            body: body.clone(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;
    mail.tasks.insert(
        task.clone(),
        FakeTask {
            from: "pump".into(),
            to: "pump".into(),
            subject: node.into(),
            body,
            depends_on: Vec::new(),
            replies: Vec::new(),
            state: TaskState {
                status: TaskStatus::Open,
                closer: None,
                verdict: None,
            },
            branch: None,
            commit: None,
        },
    );
    insert_row(store, need, node, &task);
    task
}

impl Rig {
    fn post(&self, task: &str, word: &str) -> String {
        self.project
            .mailspace
            .pump_post_answer(None, task, word, "operator ruled")
            .unwrap()
            .id
    }

    fn apply(&mut self) -> Result<(), VivariumError> {
        let def_for = |_: &RunRow| load(FlowKind::Build);
        let decide = |_: &FlowNode, _: &[JoinInput]| Ok(String::new());
        apply_pending(&self.ctx, &self.store, &mut self.mail, &def_for, &decide)
    }

    fn row(&self, task: &str) -> StepRow {
        self.store.step_by_task(task).unwrap().unwrap()
    }

    fn applied(&self, id: &str) -> bool {
        self.store.applied_action_ids().unwrap().contains(id)
    }

    fn exceptions(&self) -> String {
        fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }
}

#[test]
fn merged_on_a_slice_closes_the_task_and_marks_the_row_done() {
    let mut rig = rig();
    let id = rig.post(&rig.slice.clone(), "merged");

    rig.apply().unwrap();

    let row = rig.row(&rig.slice);
    assert_eq!(row.state, StepState::Done);
    assert_eq!(row.verdict.as_deref(), Some("merged"));
    assert_eq!(
        rig.mail.calls,
        vec![Call::CloseTask {
            handle: rig.slice.clone(),
            verdict: "merged".into()
        }],
        "the slice closes and nothing routes"
    );
    assert!(rig.applied(&id));
    assert_eq!(rig.exceptions(), "");
}

#[test]
fn no_defect_on_a_slice_closes_it_the_same_way() {
    let mut rig = rig();
    rig.post(&rig.slice.clone(), "no-defect");

    rig.apply().unwrap();

    let row = rig.row(&rig.slice);
    assert_eq!(row.state, StepState::Done);
    assert_eq!(row.verdict.as_deref(), Some("no-defect"));
}

#[test]
fn failed_on_a_slice_marks_the_row_failed_without_a_close() {
    let mut rig = rig();
    let id = rig.post(&rig.slice.clone(), "failed");

    rig.apply().unwrap();

    let row = rig.row(&rig.slice);
    assert_eq!(row.state, StepState::Failed);
    assert_eq!(row.verdict.as_deref(), Some("failed"));
    assert!(rig.mail.calls.is_empty(), "{:?}", rig.mail.calls);
    assert!(rig.applied(&id));
}

#[test]
fn built_on_a_slice_is_refused_naming_the_three_words() {
    let mut rig = rig();
    let id = rig.post(&rig.slice.clone(), "built");

    rig.apply().unwrap();

    let row = rig.row(&rig.slice);
    assert_eq!(row.state, StepState::Running);
    assert!(rig.mail.calls.is_empty(), "{:?}", rig.mail.calls);
    let log = rig.exceptions();
    assert_eq!(log.matches(ANSWER_REFUSED).count(), 1, "{log}");
    assert!(log.contains("merged, no-defect, failed"), "{log}");
    assert!(rig.applied(&id));
}

#[test]
fn a_failed_slice_close_leaves_the_letter_for_the_next_apply() {
    let mut rig = rig();
    let id = rig.post(&rig.slice.clone(), "merged");
    rig.mail.fail_at = Some(0);

    rig.apply().unwrap();

    assert!(!rig.applied(&id), "a retry is not marked applied");
    assert_eq!(rig.row(&rig.slice).state, StepState::Running);
    let log = rig.exceptions();
    assert!(
        !log.contains(ANSWER_REFUSED),
        "a retry is not a refusal: {log}"
    );
    assert_eq!(log.matches(ANSWER_RETRYING).count(), 1, "{log}");

    rig.apply().unwrap();

    assert!(rig.applied(&id));
    let row = rig.row(&rig.slice);
    assert_eq!(row.state, StepState::Done);
    assert_eq!(row.verdict.as_deref(), Some("merged"));
}

#[test]
fn a_routing_write_error_on_a_step_is_retried_not_refused() {
    let mut rig = rig();
    let id = rig.post(&rig.build.clone(), "built");
    rig.mail.fail_at = Some(0);

    rig.apply().unwrap();

    assert!(!rig.applied(&id));
    let log = rig.exceptions();
    assert!(!log.contains(ANSWER_REFUSED), "{log}");
    assert_eq!(log.matches(ANSWER_RETRYING).count(), 1, "{log}");

    rig.apply().unwrap();

    assert!(rig.applied(&id));
    let row = rig.row(&rig.build);
    assert_eq!(row.state, StepState::Done);
    assert_eq!(row.verdict.as_deref(), Some("built"));
}

#[test]
fn a_routing_failure_alarms_once_over_two_ticks_and_stays_unapplied() {
    let mut rig = rig();
    let id = rig.post(&rig.build.clone(), "built");

    for _ in 0..2 {
        rig.mail.fail_at = Some(0);
        rig.apply().unwrap();
    }

    assert!(!rig.applied(&id));
    let log = rig.exceptions();
    assert_eq!(log.matches(ANSWER_RETRYING).count(), 1, "{log}");
    assert!(log.contains(&rig.build), "names the step task: {log}");
    assert!(log.contains("`built`"), "names the word: {log}");
    assert!(
        log.contains("routing the answer failed"),
        "names the error: {log}"
    );
    assert!(!log.contains(ANSWER_REFUSED), "{log}");
}

#[test]
fn the_fourth_failing_tick_refuses_the_letter_and_marks_it_applied() {
    let mut rig = rig();
    let id = rig.post(&rig.build.clone(), "built");

    for tick in 1..=3 {
        rig.mail.fail_at = Some(0);
        rig.apply().unwrap();
        assert!(!rig.applied(&id), "tick {tick} leaves it unapplied");
    }
    assert!(!rig.exceptions().contains(ANSWER_REFUSED));

    rig.mail.fail_at = Some(0);
    rig.apply().unwrap();

    assert!(rig.applied(&id));
    let log = rig.exceptions();
    assert_eq!(log.matches(ANSWER_RETRYING).count(), 1, "{log}");
    assert_eq!(log.matches(ANSWER_REFUSED).count(), 1, "{log}");
    assert!(log.contains("failed 4 times, more than 3 retries"), "{log}");
    assert_eq!(rig.row(&rig.build).state, StepState::Running);
}

#[test]
fn a_transient_failure_that_clears_on_tick_two_applies_normally() {
    let mut rig = rig();
    let id = rig.post(&rig.build.clone(), "built");

    rig.mail.fail_at = Some(0);
    rig.apply().unwrap();
    assert!(!rig.applied(&id));

    rig.apply().unwrap();

    assert!(rig.applied(&id));
    let row = rig.row(&rig.build);
    assert_eq!(row.state, StepState::Done);
    assert_eq!(row.verdict.as_deref(), Some("built"));
    assert!(!rig.exceptions().contains(ANSWER_REFUSED));
}
