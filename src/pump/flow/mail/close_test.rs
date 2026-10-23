//! Closing-half tests on a real mailspace.

use super::MailspacePort;
use crate::mailspace::{RoleUpdate, SendRequest};
use crate::pump::fixture::{Project, project};
use crate::pump::flow::mail::{MailReader, MailWriter, NeedStatus, TaskStatus};

/// The port over the fixture; the fixture's pump identity (`[pump] from`) is
/// `operator`.
fn port(p: &Project) -> MailspacePort {
    MailspacePort {
        mailspace: p.mailspace.clone(),
        pump_identity: "operator".into(),
    }
}

/// A need authored by `elsewhere`, addressed to the pump's reply identity.
fn foreign_need(p: &Project) -> String {
    need_to(p, "elsewhere", &["mind"])
}

/// A need authored by `from` addressed to `to`.
fn need_to(p: &Project, from: &str, to: &[&str]) -> String {
    p.mailspace
        .send(SendRequest {
            from: from.into(),
            to: to.iter().map(|t| (*t).to_string()).collect(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "authored elsewhere".into(),
            body: "done_when: ok".into(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

#[test]
fn the_pump_closes_a_foreign_need_with_an_open_record_and_writes_a_done_letter() {
    let p = project();
    let mut port = port(&p);
    let need = foreign_need(&p);
    let record = MailWriter::open_record(&mut port, &need, "kind: build").unwrap();
    assert_eq!(
        MailReader::open_record(&port, &need).unwrap().unwrap().0,
        record
    );

    port.close_need(&need, "flow finished").unwrap();
    assert_eq!(port.need_state(&need).unwrap(), NeedStatus::Done);
    let event = p.mailspace.item_event_state(&need).unwrap().unwrap();
    assert_eq!(event.state, "done");

    port.close_record(&need, "landed").unwrap();
    assert!(MailReader::open_record(&port, &need).unwrap().is_none());
}

#[test]
fn the_same_close_is_refused_once_the_record_is_closed() {
    let p = project();
    let mut port = port(&p);
    let need = foreign_need(&p);
    MailWriter::open_record(&mut port, &need, "kind: build").unwrap();
    port.close_record(&need, "landed").unwrap();

    assert!(port.close_need(&need, "too late").is_err());
    assert!(p.mailspace.item_event_state(&need).unwrap().is_none());
    assert_eq!(port.need_state(&need).unwrap(), NeedStatus::Open);
}

#[test]
fn a_verdict_is_visible_on_a_closed_task_and_a_reopen_clears_it() {
    let p = project();
    let port = port(&p);
    let task = port
        .send_step("operator", "elsewhere", "step", "x", &[])
        .unwrap();
    port.close_task(&task, "merged", Some("need/x"), Some("abc123"))
        .unwrap();
    let state = port.task_state(&task).unwrap();
    assert_eq!(state.status, TaskStatus::Done);
    assert_eq!(state.verdict.as_deref(), Some("merged"));
    assert_eq!(state.closer.as_deref(), Some("operator"));

    port.reopen_task(&task).unwrap();
    assert_eq!(port.task_state(&task).unwrap().status, TaskStatus::Open);
}

#[test]
fn a_branch_without_a_commit_is_refused() {
    let p = project();
    let port = port(&p);
    let task = port
        .send_step("operator", "elsewhere", "step", "x", &[])
        .unwrap();
    assert!(port.close_task(&task, "merged", Some("b"), None).is_err());
    assert_eq!(port.task_state(&task).unwrap().status, TaskStatus::Open);
}

#[test]
fn a_second_close_of_the_pumps_own_close_is_ok_and_an_operators_close_errors() {
    let mut p = project();
    let mut port = port(&p);
    let need = foreign_need(&p);
    MailWriter::open_record(&mut port, &need, "kind: build").unwrap();
    port.close_need(&need, "flow finished").unwrap();
    // The retry after a partial failure is safe.
    port.close_need(&need, "flow finished").unwrap();
    assert_eq!(port.need_state(&need).unwrap(), NeedStatus::Done);

    // A need closed by someone else is not the pump's to close again.
    p.mailspace
        .set_role(
            "mind",
            RoleUpdate {
                kind: Some(Some("mind".into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    let other = need_to(&p, "operator", &["mind"]);
    p.mailspace
        .move_item_outcome("mind", &other, "done", None, "need done", None)
        .unwrap();
    let port = self::port(&p);
    let state = port.task_state(&other).unwrap();
    assert_eq!(state.closer.as_deref(), Some("mind"));
    let err = port.close_need(&other, "mine too").unwrap_err();
    assert!(!err.to_string().contains("not found"), "{err}");
}

#[test]
fn closing_the_sentinel_after_the_need_leaves_the_pumps_close_as_the_last_word() {
    let p = project();
    let mut port = port(&p);
    let need = foreign_need(&p);
    let sentinel = port
        .send_step("operator", "operator", "run", "pump-sentinel: x r1", &[])
        .unwrap();
    port.bind(&need, std::slice::from_ref(&sentinel)).unwrap();
    MailWriter::open_record(&mut port, &need, "kind: build").unwrap();

    port.close_need(&need, "flow finished").unwrap();
    port.close_task(&sentinel, "delivered", None, None).unwrap();

    let state = port.task_state(&need).unwrap();
    assert_eq!(state.status, TaskStatus::Done);
    assert_eq!(state.closer.as_deref(), Some("operator"));
}
