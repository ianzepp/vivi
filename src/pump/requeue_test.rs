//! Tests for [`crate::pump::requeue`]: the CLI half that posts a requeue
//! action for a flow step on its need's open pump record, and the `--job`
//! release of a held pool job.

use std::collections::BTreeSet;

use crate::cli::PumpRequeueCommand;
use crate::mailspace::PumpAction;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::index::{RunRow, StepState};
use crate::pump::jobs::{JobSpec, JobStatus, enqueue};
use crate::pump::requeue::{RequeueResult, RequeueTarget, requeue, run};
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};

/// A requeue command with `steps` and nothing else set.
fn command(steps: &[&str]) -> PumpRequeueCommand {
    PumpRequeueCommand {
        steps: steps.iter().map(|task| (*task).to_owned()).collect(),
        reason: None,
        dry_run: false,
        job: None,
        project: None,
    }
}

/// The actions posted and not yet applied.
fn posted(project: &Project) -> Vec<PumpAction> {
    project
        .mailspace
        .pump_pending_actions(&BTreeSet::new())
        .unwrap()
}

#[test]
fn run_requeue_without_a_pump_database_is_refused() {
    let project = project();
    let err = run(project.root(), &command(&["step-task-1"])).unwrap_err();
    assert!(err.to_string().contains("no pump.db at"), "{err}");
}

/// Seed a `live` flow run of `need` holding one step with `task`.
fn seed_flow_step(project: &Project, need: &str, task: &str) {
    let mut store = PumpStore::open(&project.home().join(DB_FILENAME)).unwrap();
    store.initialize_control("running").unwrap();
    let run = RunRow {
        need: need.to_owned(),
        run: 1,
        flow: String::from("build"),
        flow_version: 2,
        kind: String::from("build"),
        entry: String::from("plan"),
        state: String::from("live"),
    };
    store.insert_run(&run).unwrap();
    store
        .insert_run_step(&run, &run.step("plan", task, StepState::Pending))
        .unwrap();
}

#[test]
fn run_requeue_posts_a_requeue_action_for_a_flow_step_task() {
    let project = project();
    let need = project.need("fix auth");
    let record = project.mailspace.pump_send(None, &need, None).unwrap();
    seed_flow_step(&project, &need, "step-task-1");

    let mut dry = command(&["step-task-1"]);
    dry.dry_run = true;
    run(project.root(), &dry).unwrap();
    assert!(posted(&project).is_empty(), "a dry run posts nothing");

    run(project.root(), &command(&["step-task-1"])).unwrap();

    let actions = posted(&project);
    assert_eq!(actions.len(), 1, "one action was posted");
    assert_eq!(actions[0].record, record.handle);
    assert_eq!(actions[0].need, need);
    assert_eq!(actions[0].request.unit.as_deref(), Some("step-task-1"));
    assert_eq!(actions[0].request.reason, "operator requeue");
}

#[test]
fn run_requeue_of_a_flow_step_needs_an_open_record_and_a_known_task() {
    let project = project();
    let need = project.need("fix auth");
    seed_flow_step(&project, &need, "step-task-1");

    let text = run(project.root(), &command(&["step-task-1"]))
        .unwrap_err()
        .to_string();
    assert!(text.contains("no open pump record"), "{text}");
    assert!(posted(&project).is_empty());

    let text = run(project.root(), &command(&["neither"]))
        .unwrap_err()
        .to_string();
    assert!(
        text.contains("neither is not a step of an active flow run"),
        "{text}"
    );
}

/// Write `pump.db` with job `m1` Pending and held, and job `d1` Dispatched.
fn seed_jobs(project: &Project) -> PumpStore {
    let mut st = State::default();
    for id in ["m1", "d1"] {
        let spec = JobSpec::new(id, "merge", "merge", "integrate", "merge branches");
        enqueue(&mut st, spec).unwrap();
    }
    st.jobs.get_mut("m1").unwrap().held = Some(String::from("no usable bridge"));
    st.jobs.get_mut("d1").unwrap().status = JobStatus::Dispatched;
    let mut store = PumpStore::open(&project.home().join(DB_FILENAME)).unwrap();
    store.initialize_control("running").unwrap();
    store.save(&st).unwrap();
    store
}

fn job_command(id: &str, dry_run: bool) -> PumpRequeueCommand {
    PumpRequeueCommand {
        job: Some(id.to_owned()),
        dry_run,
        ..command(&[])
    }
}

#[test]
fn requeue_job_writes_a_release_row_for_a_held_job() {
    let project = project();
    let store = seed_jobs(&project);

    run(project.root(), &job_command("m1", true)).unwrap();
    assert!(store.take_job_releases().unwrap().is_empty(), "dry run");

    run(project.root(), &job_command("m1", false)).unwrap();
    assert_eq!(store.take_job_releases().unwrap(), ["m1"]);
}

#[test]
fn requeue_job_refuses_a_job_that_is_not_held_or_unknown() {
    let project = project();
    let store = seed_jobs(&project);

    let error = run(project.root(), &job_command("d1", false)).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("d1") && text.contains("Dispatched"), "{text}");
    let error = run(project.root(), &job_command("nope", false)).unwrap_err();
    assert!(error.to_string().contains("unknown job: nope"), "{error}");
    assert!(store.take_job_releases().unwrap().is_empty());
}

/// Plant an open pump record in the old form: it lists with no need and a
/// letter error.
fn plant_legacy_record(project: &Project, need: &str, id: &str) {
    let raw = format!(
        "From: {}\r\nTo: {}\r\nSubject: pump old {id}\r\nMessage-ID: <{id}@x.test>\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\nX-Vivi-Kind: pump\r\nX-Vivi-Need: {need}\r\n\
         \r\nDispatch of an old record.\r\n",
        project.mailspace.address_for("operator"),
        project.mailspace.address_for("mind"),
    );
    let sent = project
        .mailspace
        .deliver_raw(raw.as_bytes(), "pumps")
        .unwrap();
    let storage = project.mailspace.storage().unwrap();
    let message_id = storage
        .resolve_message_token(&sent.delivered[0].handle)
        .unwrap();
    let view = storage.message_by_id(&message_id).unwrap().unwrap();
    storage
        .append_mailspace_event(&crate::storage::MailspaceEventInput {
            command: "pump send".into(),
            event_type: "delivered".into(),
            actor_identity: Some("operator".into()),
            account: "mind".into(),
            message_id: view.message_id.clone(),
            content_id: view.content_id.clone(),
            from_role: None,
            to_role: Some("pumps".into()),
            from_identity: Some("operator".into()),
            to_identity: Some("mind".into()),
            subject: view.subject.clone(),
            note: None,
        })
        .unwrap();
}

#[test]
fn a_legacy_record_never_counts_as_the_open_record_of_need_empty() {
    let project = project();
    let need = project.need("fix auth");
    plant_legacy_record(&project, &need, "old-one");
    let open = project
        .mailspace
        .pump_list(None, Some(crate::mailspace::PumpState::Open))
        .unwrap();
    assert_eq!(open.len(), 1, "{open:?}");
    assert!(open[0].letter_error.is_some(), "{open:?}");
    seed_flow_step(&project, "", "step-task-1");

    let error = run(project.root(), &command(&["step-task-1"])).unwrap_err();

    assert!(error.to_string().contains("no open pump record"), "{error}");
    assert!(posted(&project).is_empty());
}

#[test]
fn run_requeue_without_a_step_is_refused() {
    let project = project();
    let err = run(project.root(), &command(&[])).unwrap_err();
    assert!(err.to_string().contains("at least one step"), "{err}");
    assert!(posted(&project).is_empty());
}

#[test]
fn requeue_returns_the_step_it_posted_and_a_dry_run_posts_nothing() {
    let project = project();
    let need = project.need("fix auth");
    let record = project.mailspace.pump_send(None, &need, None).unwrap();
    seed_flow_step(&project, &need, "step-task-1");
    let tasks = vec![String::from("step-task-1")];

    let preview = requeue(
        project.root(),
        &RequeueTarget::Steps {
            tasks: &tasks,
            reason: None,
        },
        true,
    )
    .unwrap();
    assert_eq!(
        preview,
        vec![RequeueResult::StepPreview {
            task: String::from("step-task-1"),
            need: need.clone(),
        }]
    );
    assert_eq!(
        preview[0].to_string(),
        format!("would requeue flow step step-task-1 of need {need}")
    );
    assert!(posted(&project).is_empty(), "a dry run posts nothing");

    let done = requeue(
        project.root(),
        &RequeueTarget::Steps {
            tasks: &tasks,
            reason: Some("again"),
        },
        false,
    )
    .unwrap();
    let RequeueResult::Step {
        task,
        need: got,
        record: posted_on,
        id,
    } = &done[0]
    else {
        panic!("expected a posted step, got {done:?}");
    };
    assert_eq!(task, "step-task-1");
    assert_eq!(got, &need);
    assert_eq!(posted_on, &record.handle);
    let actions = posted(&project);
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].id, *id);
    assert_eq!(actions[0].request.reason, "again");
    assert_eq!(
        done[0].to_string(),
        "requeue step-task-1 requested; the pump applies it at its next tick"
    );
}

#[test]
fn requeue_job_returns_the_release_line() {
    let project = project();
    let store = seed_jobs(&project);

    let preview = requeue(project.root(), &RequeueTarget::Job("m1"), true).unwrap();
    assert_eq!(
        preview,
        vec![RequeueResult::JobPreview {
            id: String::from("m1"),
        }]
    );
    assert_eq!(preview[0].to_string(), "would request release of job m1");
    assert!(store.take_job_releases().unwrap().is_empty(), "dry run");

    let done = requeue(project.root(), &RequeueTarget::Job("m1"), false).unwrap();
    assert_eq!(
        done,
        vec![RequeueResult::Job {
            id: String::from("m1"),
        }]
    );
    assert_eq!(
        done[0].to_string(),
        "release of job m1 requested; the pump applies it at its next tick"
    );
    assert_eq!(store.take_job_releases().unwrap(), ["m1"]);
}
