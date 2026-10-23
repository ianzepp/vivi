//! Tests for [`super::apply_pending`]: an operator answer routes once, and a
//! wrong word, a closed step or an unknown task is refused with an alarm.

use std::fs;

use super::{ANSWER_REFUSED, apply_pending};
use crate::error::VivariumError;
use crate::letter::fixture::{sentinel_body, step_body};
use crate::mailspace::SendRequest;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::advance::route::JoinInput;
use crate::pump::flow::def::{FlowKind, FlowNode};
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
    need: String,
    step: String,
}

/// A real need with an open pump record and a real step task of node `build1`
/// (state `state`), mirrored in the fake port and the step index.
fn rig(state: StepState) -> Rig {
    let project = project();
    let ctx = project.pump_ctx();
    let store = PumpStore::open(&project.home().join(DB_FILENAME)).unwrap();
    let need = project.need("fix auth");
    project.mailspace.pump_send(None, &need, None).unwrap();
    let step = project
        .mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "build1".into(),
            body: step_body(&need, 1, "build1", ""),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;
    let mut mail = FakeMail::new("pump");
    let sentinel = mail
        .send_step("pump", "pump", "run", &sentinel_body("x", 1, ""), &[])
        .unwrap();
    mail.tasks.insert(
        step.clone(),
        FakeTask {
            from: "pump".into(),
            to: "pump".into(),
            subject: "build1".into(),
            body: step_body(&need, 1, "build1", ""),
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
    store
        .insert_run(&RunRow {
            need: need.clone(),
            run: 1,
            flow: "build".into(),
            flow_version: 2,
            kind: "build".into(),
            entry: "plan".into(),
            state: "live".into(),
        })
        .unwrap();
    for (node, task, row_state) in [
        ("sentinel", sentinel, StepState::Running),
        ("build1", step.clone(), state),
    ] {
        store
            .insert_step(
                &need,
                &StepRow {
                    run: 1,
                    node: node.into(),
                    task,
                    state: row_state,
                    verdict: None,
                },
            )
            .unwrap();
    }
    mail.calls.clear();
    Rig {
        project,
        ctx,
        store,
        mail,
        need,
        step,
    }
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

    fn closes(&self) -> usize {
        self.mail
            .calls
            .iter()
            .filter(|call| matches!(call, Call::CloseTask { .. }))
            .count()
    }

    fn exceptions(&self) -> String {
        fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }
}

#[test]
fn an_answer_routes_once_and_closes_the_step_with_that_verdict() {
    let mut rig = rig(StepState::Running);
    let id = rig.post(&rig.step.clone(), "built");

    rig.apply().unwrap();

    let row = rig.store.step_by_task(&rig.step).unwrap().unwrap();
    assert_eq!(row.state, StepState::Done);
    assert_eq!(row.verdict.as_deref(), Some("built"));
    let sent = rig
        .mail
        .calls
        .iter()
        .filter(|call| matches!(call, Call::SendStep { .. }))
        .count();
    assert_eq!(sent, 2, "audit1 and test1 follow `built`");
    assert_eq!(rig.closes(), 1);
    assert!(rig.store.applied_action_ids().unwrap().contains(&id));
    assert_eq!(rig.exceptions(), "");

    let calls = rig.mail.calls.len();
    rig.apply().unwrap();
    assert_eq!(rig.mail.calls.len(), calls, "applied exactly once");
    assert_eq!(rig.exceptions(), "");
}

#[test]
fn a_word_outside_the_nodes_set_is_refused_and_recorded_once() {
    let mut rig = rig(StepState::Running);
    let id = rig.post(&rig.step.clone(), "merged");

    rig.apply().unwrap();
    rig.apply().unwrap();

    assert!(rig.mail.calls.is_empty(), "{:?}", rig.mail.calls);
    let row = rig.store.step_by_task(&rig.step).unwrap().unwrap();
    assert_eq!(row.state, StepState::Running);
    let log = rig.exceptions();
    assert_eq!(log.matches(ANSWER_REFUSED).count(), 1, "{log}");
    assert!(
        log.contains("`merged` is not an answer of node `build1`"),
        "{log}"
    );
    assert!(rig.store.applied_action_ids().unwrap().contains(&id));
}

#[test]
fn a_closed_step_is_refused() {
    let mut rig = rig(StepState::Running);
    rig.store
        .set_step_state(&rig.step, StepState::Done, Some("built"))
        .unwrap();
    rig.post(&rig.step.clone(), "failed");

    rig.apply().unwrap();

    assert!(rig.mail.calls.is_empty(), "{:?}", rig.mail.calls);
    let log = rig.exceptions();
    assert!(log.contains(ANSWER_REFUSED), "{log}");
    assert!(log.contains("already closed with `built`"), "{log}");
}

#[test]
fn a_held_step_is_released_by_an_answer() {
    let mut rig = rig(StepState::Waiting);
    rig.post(&rig.step.clone(), "failed");

    rig.apply().unwrap();

    let row = rig.store.step_by_task(&rig.step).unwrap().unwrap();
    assert_eq!(row.state, StepState::Done);
    assert_eq!(row.verdict.as_deref(), Some("failed"));
    assert_eq!(rig.exceptions(), "");
}

#[test]
fn the_run_sentinel_and_an_unindexed_task_are_refused() {
    let mut rig = rig(StepState::Running);
    let sentinel = rig
        .store
        .steps_of(&rig.need, 1)
        .unwrap()
        .into_iter()
        .find(|row| row.node == "sentinel")
        .unwrap()
        .task;
    // The sentinel is a fake-port task; give the mailspace an indexed look-alike.
    let ghost = rig
        .project
        .mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "ghost".into(),
            body: step_body(&rig.need, 1, "plan", ""),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;
    rig.post(&ghost, "planned");
    rig.store
        .insert_step(
            &rig.need,
            &StepRow {
                run: 1,
                node: "sentinel".into(),
                task: ghost.clone(),
                state: StepState::Running,
                verdict: None,
            },
        )
        .unwrap();
    let before = rig.exceptions();
    assert!(!sentinel.is_empty());

    rig.apply().unwrap();

    let log = rig.exceptions();
    assert!(log[before.len()..].contains("run sentinel"), "{log}");
    assert!(rig.mail.calls.is_empty());
}
