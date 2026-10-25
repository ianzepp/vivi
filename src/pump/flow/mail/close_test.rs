//! Closing-half tests on a real mailspace.

use super::MailspacePort;
use crate::letter::fixture::{need_body, record_body, sentinel_body, task_body};
use crate::mailspace::SendRequest;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::mail::{MailReader, MailWriter, NeedStatus, TaskStatus};

/// The port over the fixture; the fixture's pump identity (`[pump] reply_from`) is
/// `mind`; `[pump] from` (`operator`) has no pump authority.
fn port(p: &Project) -> MailspacePort {
    MailspacePort {
        mailspace: p.mailspace.clone(),
        pump_identity: "mind".into(),
    }
}

/// A need authored by `elsewhere`, addressed to the pump's reply identity and
/// delegated to the third role `operator`, so the pump owns no copy it could
/// close locally.
fn foreign_need(p: &Project) -> String {
    let need = need_to(p, "elsewhere", &["mind"]);
    p.mailspace
        .assign_need("elsewhere", &need, "operator")
        .unwrap();
    need
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
            body: need_body(&["ok"], ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
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
    let record = MailWriter::open_record(&mut port, &need, &record_body(&need, "build")).unwrap();
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
fn the_same_close_no_longer_travels_once_the_record_is_closed() {
    let p = project();
    let mut port = port(&p);
    let need = foreign_need(&p);
    MailWriter::open_record(&mut port, &need, &record_body(&need, "build")).unwrap();
    port.close_record(&need, "landed").unwrap();

    // The pump identity holds its own copy, so the late close is only a local
    // report: no `done` event letter, so nothing travels to other members.
    port.close_need(&need, "too late").unwrap();
    assert!(p.mailspace.item_event_state(&need).unwrap().is_none());
}

#[test]
fn a_verdict_is_visible_on_a_closed_task_and_a_reopen_clears_it() {
    let p = project();
    let port = port(&p);
    let task = port
        .send_step("mind", "elsewhere", "step", &task_body("x"), &[])
        .unwrap();
    port.close_task(&task, "merged", Some("need/x"), Some("abc123"))
        .unwrap();
    let state = port.task_state(&task).unwrap();
    assert_eq!(state.status, TaskStatus::Done);
    assert_eq!(state.verdict.as_deref(), Some("merged"));
    assert_eq!(state.closer.as_deref(), Some("mind"));

    port.reopen_task(&task).unwrap();
    assert_eq!(port.task_state(&task).unwrap().status, TaskStatus::Open);
}

#[test]
fn a_branch_without_a_commit_is_refused() {
    let p = project();
    let port = port(&p);
    let task = port
        .send_step("mind", "elsewhere", "step", &task_body("x"), &[])
        .unwrap();
    assert!(port.close_task(&task, "merged", Some("b"), None).is_err());
    assert_eq!(port.task_state(&task).unwrap().status, TaskStatus::Open);
}

#[test]
fn a_second_close_of_the_pumps_own_close_is_ok_and_an_operators_close_stays_the_last_word() {
    let p = project();
    let mut port = port(&p);
    let need = foreign_need(&p);
    MailWriter::open_record(&mut port, &need, &record_body(&need, "build")).unwrap();
    port.close_need(&need, "flow finished").unwrap();
    // The retry after a partial failure is safe.
    port.close_need(&need, "flow finished").unwrap();
    assert_eq!(port.need_state(&need).unwrap(), NeedStatus::Done);

    // A need closed by someone else is not the pump's to close again.
    let other = foreign_need(&p);
    p.mailspace
        .move_item_outcome("operator", &other, "done", None, "need done", None)
        .unwrap();
    let port = self::port(&p);
    let state = port.task_state(&other).unwrap();
    assert_eq!(state.closer.as_deref(), Some("operator"));
    // The pump's late close is a local report only: the operator's close
    // remains the need's state for every member.
    port.close_need(&other, "mine too").unwrap();
    let event = p.mailspace.item_event_state(&other).unwrap().unwrap();
    assert_eq!(event.state, "done");
}

#[test]
fn closing_the_sentinel_after_the_need_leaves_the_pumps_close_as_the_last_word() {
    let p = project();
    let mut port = port(&p);
    let need = foreign_need(&p);
    let sentinel = port
        .send_step("mind", "mind", "run", &sentinel_body("x", 1, ""), &[])
        .unwrap();
    port.bind(&need, std::slice::from_ref(&sentinel)).unwrap();
    MailWriter::open_record(&mut port, &need, &record_body(&need, "build")).unwrap();

    port.close_need(&need, "flow finished").unwrap();
    port.close_task(&sentinel, "delivered", None, None).unwrap();

    let state = port.task_state(&need).unwrap();
    assert_eq!(state.status, TaskStatus::Done);
    assert_eq!(state.closer.as_deref(), Some("mind"));
}

#[test]
fn a_landed_close_records_the_branch_and_commit_it_landed() {
    let p = project();
    let mut port = port(&p);
    let need = foreign_need(&p);
    MailWriter::open_record(&mut port, &need, &record_body(&need, "build")).unwrap();
    let commit = "b05ff62aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    MailWriter::close_need_landed(&mut port, &need, "landed by the pump flow", "main", commit)
        .unwrap();
    assert_eq!(port.need_state(&need).unwrap(), NeedStatus::Done);
    let metadata = p.mailspace.storage().unwrap().item_metadata(&need).unwrap();
    assert_eq!(metadata.get("branch").map(String::as_str), Some("main"));
    assert_eq!(metadata.get("commit").map(String::as_str), Some(commit));
}
