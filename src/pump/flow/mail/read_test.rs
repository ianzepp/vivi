//! Reader tests on a real mailspace.

use super::{run_count, sentinel_count_of};
use crate::letter::fixture::{
    need_body_with, record_body, sentinel_body, step_body, task_body_with,
};
use crate::letter::work::{NeedFront, StepFront, TaskFront};
use crate::mailspace::{Mailspace, SendRequest};
use crate::pump::fixture::project;
use crate::pump::flow::mail::{MailReader, MailspacePort, NeedStatus, OpenRecords, TaskStatus};
use crate::pump::flow::slice_dispatch::SliceMail;
use crate::storage::MailspaceEventInput;

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

    let first = send_task(mailspace, "sentinel 1", &sentinel_body(&need, 1, ""));
    let step = send_task(mailspace, "step", &step_body(&need, 1, "plan", ""));
    mailspace
        .backlog_bind_units(&need, &[first.clone(), step])
        .unwrap();
    assert_eq!(run_count(mailspace, &need), 1);

    let second = send_task(mailspace, "sentinel 2", &sentinel_body(&need, 2, ""));
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
    let task = send_task(&project.mailspace, "step", &step_body(&need, 1, "plan", ""));
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
        .pump_send(None, &need, Some(&record_body(&need, "release")))
        .unwrap();
    let (handle, body) = port.open_record(&need).unwrap().expect("an open record");
    assert_eq!(handle, record.handle);
    let front = crate::letter::read::record_of(&handle, &body)
        .unwrap()
        .front;
    assert_eq!(
        front.kind,
        Some(crate::letter::records::RecordKind::Release)
    );
    let listed = port.open_record_needs().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, need);
}

#[test]
fn prerequisites_and_last_closed_record_are_read() {
    let project = project();
    let first = project.need("first need");
    let second = project.need("second need");
    let dependent = project
        .mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "dependent need".into(),
            body: need_body_with(
                &NeedFront {
                    depends_on: vec![first.clone()],
                    ..NeedFront::new(vec!["it works".into()])
                },
                "",
            ),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;
    let port = MailspacePort {
        mailspace: project.mailspace,
        pump_identity: "mind".into(),
    };
    assert_eq!(port.need_prerequisites(&dependent).unwrap(), vec![first]);
    assert!(port.need_prerequisites(&second).unwrap().is_empty());

    assert_eq!(port.last_closed_record(&second).unwrap(), None);
    port.open_pump_record(&second, &record_body(&second, "release"))
        .unwrap();
    assert_eq!(port.last_closed_record(&second).unwrap(), None);
    port.close_pump_record(&second, "landed").unwrap();
    assert_eq!(
        port.last_closed_record(&second).unwrap(),
        Some("landed".to_string())
    );
}

#[test]
fn sent_step_finds_the_open_task_not_the_closed_one() {
    let key = |node: &str| StepFront {
        need: "n".into(),
        run: 1,
        node: Some(node.into()),
        sentinel: false,
    };
    let project = project();
    let port = MailspacePort {
        mailspace: project.mailspace,
        pump_identity: "mind".into(),
    };
    let one = port
        .send_step(
            "mind",
            "elsewhere",
            "step",
            &step_body("n", 1, "plan", ""),
            &[],
        )
        .unwrap();
    let two = port
        .send_step(
            "mind",
            "elsewhere",
            "step",
            &step_body("n", 1, "slice", ""),
            &[],
        )
        .unwrap();
    assert_eq!(
        port.sent_step("step", &key("plan")).unwrap(),
        Some(one.clone())
    );
    assert_eq!(
        port.sent_step("step", &key("slice")).unwrap(),
        Some(two.clone())
    );
    port.close_task(&one, "withdrawn", None, None).unwrap();
    assert_eq!(port.sent_step("step", &key("plan")).unwrap(), None);
    assert_eq!(port.sent_step("step", &key("slice")).unwrap(), Some(two));
    assert_eq!(port.sent_step("step", &key("nope")).unwrap(), None);
    assert_eq!(port.sent_step("other", &key("slice")).unwrap(), None);
}

#[test]
fn task_body_and_replies_have_no_cr_for_a_multi_line_body() {
    let project = project();
    let port = MailspacePort {
        mailspace: project.mailspace,
        pump_identity: "mind".into(),
    };
    let task = port
        .send_step(
            "mind",
            "elsewhere",
            "step",
            &task_body_with(
                &TaskFront {
                    target: Some("vivi".into()),
                    ..TaskFront::step("n", 1, "plan")
                },
                "the brief",
            ),
            &[],
        )
        .unwrap();
    let reply =
        "pool result j1: finished\nsummary\n\nthe answer\n\n--- stderr tail\nnoise".to_owned();
    port.reply(&task, &reply).unwrap();

    let body = SliceMail::task_body(&port, &task).unwrap();
    let replies = SliceMail::marked_replies(&port, &task, "pool result ").unwrap();

    assert!(!body.contains('\r'), "{body:?}");
    assert!(body.contains("target = \"vivi\""), "{body:?}");
    assert!(body.contains("\n\nthe brief"), "{body:?}");
    assert_eq!(replies.len(), 1);
    assert!(!replies[0].1.contains('\r'), "{:?}", replies[0].1);
    assert!(
        replies[0].1.contains("summary\n\nthe answer"),
        "{:?}",
        replies[0].1
    );
}

/// Ingest an old-form letter raw (no `X-Vivi-Format` header), which the send
/// gate would refuse or wrap: `body` with `kind` and `extra` header lines
/// ("Name: value\r\n" each) from the operator to the mind, into `folder`.
/// Returns the handle the mind holds.
fn raw_old_letter(
    mailspace: &Mailspace,
    kind: &str,
    folder: &str,
    extra: &str,
    body: &str,
) -> String {
    let raw = format!(
        "From: {}\r\nTo: {}\r\nSubject: old\r\nMessage-ID: <old-{kind}@x.test>\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\nX-Vivi-Kind: {kind}\r\n{extra}\r\n{body}\r\n",
        mailspace.address_for("operator"),
        mailspace.address_for("mind"),
    );
    mailspace
        .deliver_raw(raw.as_bytes(), folder)
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

#[test]
fn a_legacy_record_letter_is_unreadable_and_not_an_open_record_need() {
    let project = project();
    let good = project.need("good need");
    project
        .mailspace
        .pump_send(None, &good, Some(&record_body(&good, "build")))
        .unwrap();
    let old = raw_old_letter(
        &project.mailspace,
        "pump",
        "pumps",
        "",
        "Dispatch of an old record.",
    );
    let storage = project.mailspace.storage().unwrap();
    let id = storage.resolve_message_token(&old).unwrap();
    let view = storage.message_by_id(&id).unwrap().unwrap();
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
    drop(storage);

    let port = MailspacePort {
        mailspace: project.mailspace,
        pump_identity: "mind".into(),
    };

    let unreadable = port.unreadable_records().unwrap();
    let needs = port.open_record_needs().unwrap();

    assert_eq!(unreadable.len(), 1, "{unreadable:?}");
    assert!(unreadable[0].1.contains(&unreadable[0].0), "{unreadable:?}");
    assert_eq!(needs.len(), 1, "{needs:?}");
    assert_eq!(needs[0].0, good);
}

#[test]
fn a_legacy_need_gives_a_letter_error_and_a_front_matter_need_none() {
    let project = project();
    let good = project.need("good need");
    let raw = format!(
        "From: operator@a1b2c3d4.local\r\nTo: {}\r\nSubject: old need\r\n\
         Message-ID: <old-need@a1b2c3d4.local>\r\nX-Vivi-Kind: need\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\ndone_when: it works\r\n",
        project.mailspace.address_for("mind"),
    );
    let old = project
        .mailspace
        .deliver_raw(raw.as_bytes(), "needs")
        .unwrap()
        .delivered
        .remove(0)
        .handle;
    let port = MailspacePort {
        mailspace: project.mailspace,
        pump_identity: "mind".into(),
    };

    assert_eq!(port.need_letter_error(&good).unwrap(), None);
    let error = port
        .need_letter_error(&old)
        .unwrap()
        .expect("a letter error");
    assert!(error.contains(&old), "{error}");
}

#[test]
fn a_header_only_action_letter_is_unreadable_until_it_is_applied() {
    let project = project();
    let need = project.need("good need");
    let record = project
        .mailspace
        .pump_send(None, &need, Some(&record_body(&need, "build")))
        .unwrap();
    let letter = raw_old_letter(
        &project.mailspace,
        "mail",
        "inbox",
        &format!(
            "X-Vivi-Pump-Action: withdraw\r\nX-Vivi-Pump-Record: {}\r\n",
            record.handle
        ),
        "stop it",
    );
    let storage = project.mailspace.storage().unwrap();
    let id = storage.resolve_message_token(&letter).unwrap();
    let view = storage.message_by_id(&id).unwrap().unwrap();
    storage
        .append_mailspace_event(&MailspaceEventInput {
            command: "pump action".into(),
            event_type: "posted".into(),
            actor_identity: Some("operator".into()),
            account: "operator".into(),
            message_id: view.message_id.clone(),
            content_id: view.content_id.clone(),
            from_role: None,
            to_role: Some("inbox".into()),
            from_identity: Some("operator".into()),
            to_identity: None,
            subject: view.subject.clone(),
            note: Some(record.handle.clone()),
        })
        .unwrap();
    drop(storage);
    let port = MailspacePort {
        mailspace: project.mailspace,
        pump_identity: "mind".into(),
    };

    let unreadable = port.unreadable_actions().unwrap();

    assert_eq!(unreadable.len(), 1, "{unreadable:?}");
    assert!(unreadable[0].1.contains(&unreadable[0].0), "{unreadable:?}");
}
