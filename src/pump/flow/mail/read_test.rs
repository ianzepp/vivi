//! Reader tests on a real mailspace.

use super::{run_count, sentinel_count_of};
use crate::mailspace::{Mailspace, SendRequest};
use crate::pump::fixture::project;
use crate::pump::flow::mail::{MailReader, MailspacePort, NeedStatus, OpenRecords, TaskStatus};

/// A task from `mind` to `elsewhere`; the handle `elsewhere` holds.
fn send_task(mailspace: &Mailspace, subject: &str, body: &str) -> String {
    mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["elsewhere".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: body.into(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

#[test]
fn run_count_goes_from_zero_to_two_as_sentinels_are_bound() {
    let project = project();
    let need = project.need("flow need");
    let mailspace = &project.mailspace;
    assert_eq!(run_count(mailspace, &need), 0);

    let first = send_task(
        mailspace,
        "sentinel 1",
        &format!("pump-run-sentinel: {need} r1"),
    );
    let step = send_task(mailspace, "step", &format!("pump-step: {need} r1 plan"));
    mailspace
        .backlog_bind_units(&need, &[first.clone(), step])
        .unwrap();
    assert_eq!(run_count(mailspace, &need), 1);

    let second = send_task(
        mailspace,
        "sentinel 2",
        &format!("pump-run-sentinel: {need} r2"),
    );
    mailspace.backlog_bind_units(&need, &[second]).unwrap();
    assert_eq!(run_count(mailspace, &need), 2);
    assert_eq!(sentinel_count_of(mailspace, &need).unwrap(), 2);

    let port = MailspacePort {
        mailspace: project.mailspace,
        pump_identity: "mind".into(),
    };
    assert_eq!(port.sentinel_count(&need).unwrap(), 2);
    assert_eq!(port.bound_units(&need).unwrap().len(), 3);
}

#[test]
fn closer_and_verdict_are_read_back_and_a_reopen_clears_them() {
    let project = project();
    let need = project.need("flow need");
    let task = send_task(
        &project.mailspace,
        "step",
        &format!("pump-step: {need} r1 plan"),
    );
    project
        .mailspace
        .backlog_bind_units(&need, std::slice::from_ref(&task))
        .unwrap();
    let port = MailspacePort {
        mailspace: project.mailspace,
        pump_identity: "mind".into(),
    };
    assert_eq!(port.task_state(&task).unwrap().status, TaskStatus::Open);

    port.mailspace
        .move_task("elsewhere", &task, "done", None, Some("merged"), &[], &[])
        .unwrap();
    let state = port.task_state(&task).unwrap();
    assert_eq!(state.status, TaskStatus::Done);
    assert_eq!(state.closer.as_deref(), Some("elsewhere"));
    assert_eq!(state.verdict.as_deref(), Some("merged"));

    port.mailspace
        .move_task("elsewhere", &task, "tasks", None, None, &[], &[])
        .unwrap();
    assert_eq!(port.task_state(&task).unwrap().status, TaskStatus::Open);
}

#[test]
fn need_state_marks_and_open_record_are_read() {
    let project = project();
    let need = project.need("flow need");
    let port = MailspacePort {
        mailspace: project.mailspace,
        pump_identity: "mind".into(),
    };
    assert_eq!(port.need_state(&need).unwrap(), NeedStatus::Open);
    assert!(port.need_marks(&need).unwrap().is_empty());
    assert!(port.open_record(&need).unwrap().is_none());

    port.mailspace
        .set_marks("mind", &need, "need", true, &["research".to_string()])
        .unwrap();
    assert_eq!(
        port.need_marks(&need).unwrap(),
        vec!["research".to_string()]
    );

    let record = port
        .mailspace
        .pump_send(None, &need, Some("kind: release"))
        .unwrap();
    let (handle, body) = port.open_record(&need).unwrap().expect("an open record");
    assert_eq!(handle, record.handle);
    assert!(body.contains("kind: release"));
    let listed = port.open_record_needs().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, need);
}

#[test]
fn sent_step_finds_the_open_task_not_the_closed_one() {
    let project = project();
    let port = MailspacePort {
        mailspace: project.mailspace,
        pump_identity: "mind".into(),
    };
    let one = port
        .send_step("mind", "elsewhere", "step", "pump-step: n r1 plan", &[])
        .unwrap();
    let two = port
        .send_step("mind", "elsewhere", "step", "pump-step: n r1 slice", &[])
        .unwrap();
    assert_eq!(
        port.sent_step("step", "pump-step: n r1 plan").unwrap(),
        Some(one.clone())
    );
    assert_eq!(
        port.sent_step("step", "pump-step: n r1 slice").unwrap(),
        Some(two.clone())
    );
    port.close_task(&one, "withdrawn", None, None).unwrap();
    assert_eq!(
        port.sent_step("step", "pump-step: n r1 plan").unwrap(),
        None
    );
    assert_eq!(
        port.sent_step("step", "pump-step: n r1 slice").unwrap(),
        Some(two)
    );
    assert_eq!(
        port.sent_step("step", "pump-step: n r1 nope").unwrap(),
        None
    );
    assert_eq!(
        port.sent_step("other", "pump-step: n r1 slice").unwrap(),
        None
    );
}
