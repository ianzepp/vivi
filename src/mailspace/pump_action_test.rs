use std::collections::BTreeSet;

use super::*;
use crate::mailspace::SendRequest;

fn roster() -> (Mailspace, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("operator").unwrap();
    mailspace.add_identity("mind").unwrap();
    (mailspace, tmp)
}

fn send_need(mailspace: &Mailspace, subject: &str) -> String {
    mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: crate::letter::fixture::need_body(&["it works"], ""),
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

fn none() -> BTreeSet<String> {
    BTreeSet::new()
}

#[test]
fn a_posted_action_is_a_reply_on_the_open_record_with_machine_readable_headers() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    let record = mailspace.pump_send(None, &need, None).unwrap();
    let request = PumpActionRequest::requeue("u1", Some(2), false, true, "host was down");

    let posted = mailspace.pump_post_action(None, &need, &request).unwrap();

    assert_eq!(posted.record, record.handle);
    assert_eq!(posted.need, need);
    let thread = mailspace
        .thread(&record.handle, false, usize::MAX, usize::MAX)
        .unwrap();
    assert_eq!(thread.len(), 2, "the record and the action: {thread:?}");
    let reply = thread
        .iter()
        .find(|message| message.subject.contains("requeue action"))
        .unwrap();
    let storage = mailspace.storage().unwrap();
    let raw = storage.read_message(&reply.message_id).unwrap();
    let raw = String::from_utf8(raw).unwrap();
    for header in [
        "X-Vivi-Pump-Action: requeue",
        "X-Vivi-Pump-Unit: u1",
        "X-Vivi-Pump-Attempts: 2",
        "X-Vivi-Pump-Refund: 1",
        &format!("X-Vivi-Pump-Record: {}", record.handle),
        &format!("X-Vivi-Need: {need}"),
    ] {
        assert!(raw.contains(header), "{header} missing from {raw}");
    }
    assert!(!raw.contains("X-Vivi-Pump-Fresh"), "{raw}");
    let pending = mailspace.pump_pending_actions(&none()).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, posted.id);
    assert_eq!(pending[0].request, request);
}

#[test]
fn every_kind_round_trips_through_the_letter() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    mailspace.pump_send(None, &need, None).unwrap();
    let requests = [
        PumpActionRequest::requeue("u1", None, true, false, "start over"),
        PumpActionRequest::withdraw("operator changed their mind"),
        PumpActionRequest::replan(true),
    ];
    for request in &requests {
        mailspace.pump_post_action(None, &need, request).unwrap();
    }

    let pending = mailspace.pump_pending_actions(&none()).unwrap();

    let seen: Vec<_> = pending
        .iter()
        .map(|action| action.request.clone())
        .collect();
    assert_eq!(seen, requests, "oldest first");
}

#[test]
fn an_action_needs_an_open_record_and_says_how_to_open_one() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    let error = mailspace
        .pump_post_action(None, &need, &PumpActionRequest::withdraw("stop"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("no open pump record"), "{error}");
    assert!(
        error.contains(&format!("vivi pump send --need {need}")),
        "{error}"
    );

    let record = mailspace.pump_send(None, &need, None).unwrap();
    mailspace
        .pump_done(&record.handle, Some("gave-up"))
        .unwrap();
    let error = mailspace
        .pump_post_action(None, &need, &PumpActionRequest::replan(false))
        .unwrap_err()
        .to_string();
    assert!(error.contains("no open pump record"), "{error}");
    assert!(mailspace.pump_pending_actions(&none()).unwrap().is_empty());
}

#[test]
fn a_requeue_names_its_unit_and_a_non_need_is_refused() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    mailspace.pump_send(None, &need, None).unwrap();
    let mut request = PumpActionRequest::withdraw("x");
    request.kind = PumpActionKind::Requeue;
    let error = mailspace
        .pump_post_action(None, &need, &request)
        .unwrap_err()
        .to_string();
    assert!(error.contains("names the unit"), "{error}");
    let error = mailspace
        .pump_post_action(None, "nonsense", &PumpActionRequest::replan(false))
        .unwrap_err()
        .to_string();
    assert!(error.contains("unknown need"), "{error}");
    assert!(mailspace.pump_pending_actions(&none()).unwrap().is_empty());
}

#[test]
fn applied_actions_and_actions_on_closed_records_are_not_pending() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    let other = send_need(&mailspace, "fix billing");
    let record = mailspace.pump_send(None, &need, None).unwrap();
    mailspace.pump_send(None, &other, None).unwrap();
    let first = mailspace
        .pump_post_action(None, &need, &PumpActionRequest::replan(false))
        .unwrap();
    let second = mailspace
        .pump_post_action(None, &other, &PumpActionRequest::withdraw("stop"))
        .unwrap();

    let applied: BTreeSet<String> = [first.id.clone()].into();
    let pending = mailspace.pump_pending_actions(&applied).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, second.id);

    mailspace.pump_done(&record.handle, Some("landed")).unwrap();
    let pending = mailspace.pump_pending_actions(&none()).unwrap();
    assert_eq!(pending.len(), 1, "the closed record's action is dropped");
    assert_eq!(pending[0].need, other);
}

#[test]
fn actions_show_in_the_needs_pump_history_threads() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    mailspace.pump_send(None, &need, None).unwrap();
    mailspace
        .pump_post_action(None, &need, &PumpActionRequest::withdraw("stop"))
        .unwrap();

    let messages = mailspace.pump_threads(&need).unwrap();

    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(
        messages
            .iter()
            .any(|message| message.subject.contains("withdraw action")),
        "{messages:?}"
    );
}

fn send_task(mailspace: &Mailspace, body: &str) -> String {
    mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "step".into(),
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
fn an_answer_posts_on_the_open_record_of_the_steps_need_with_step_and_word_headers() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    let record = mailspace.pump_send(None, &need, None).unwrap();
    let step = send_task(
        &mailspace,
        &crate::letter::fixture::step_body(&need, 1, "plan", "Plan"),
    );

    let posted = mailspace
        .pump_post_answer(None, &step, "planned", "operator ruled")
        .unwrap();

    assert_eq!(posted.record, record.handle);
    assert_eq!(posted.need, need);
    let pending = mailspace.pump_pending_actions(&none()).unwrap();
    assert_eq!(pending.len(), 1);
    let request = &pending[0].request;
    assert_eq!(request.kind, PumpActionKind::Answer);
    assert_eq!(request.step.as_deref(), Some(step.as_str()));
    assert_eq!(request.word.as_deref(), Some("planned"));
    assert_eq!(request.reason, "operator ruled");
}

#[test]
fn an_answer_to_the_run_sentinel_posts_too() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    mailspace.pump_send(None, &need, None).unwrap();
    let sentinel = send_task(
        &mailspace,
        &crate::letter::fixture::sentinel_body(&need, 1, ""),
    );

    mailspace
        .pump_post_answer(None, &sentinel, "failed", "")
        .unwrap();

    assert_eq!(mailspace.pump_pending_actions(&none()).unwrap().len(), 1);
}

#[test]
fn an_answer_is_refused_without_an_open_record_a_pump_step_or_a_word() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    let step = send_task(
        &mailspace,
        &crate::letter::fixture::step_body(&need, 1, "plan", ""),
    );
    let error = mailspace
        .pump_post_answer(None, &step, "planned", "")
        .unwrap_err()
        .to_string();
    assert!(error.contains("no open pump record"), "{error}");

    mailspace.pump_send(None, &need, None).unwrap();
    let plain = send_task(
        &mailspace,
        &crate::letter::fixture::task_body("just a task"),
    );
    let error = mailspace
        .pump_post_answer(None, &plain, "planned", "")
        .unwrap_err()
        .to_string();
    assert!(error.contains("not a pump step task"), "{error}");
    let error = mailspace
        .pump_post_answer(None, &step, "  ", "")
        .unwrap_err()
        .to_string();
    assert!(error.contains("names the word"), "{error}");
    assert!(mailspace.pump_pending_actions(&none()).unwrap().is_empty());
}

#[test]
fn an_unlock_carries_its_entry_in_the_word_and_none_without_one() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    mailspace.pump_send(None, &need, None).unwrap();
    mailspace
        .pump_post_action(None, &need, &PumpActionRequest::unlock(Some("slice"), "ok"))
        .unwrap();
    mailspace
        .pump_post_action(None, &need, &PumpActionRequest::unlock(None, ""))
        .unwrap();

    let pending = mailspace.pump_pending_actions(&none()).unwrap();

    assert_eq!(pending.len(), 2);
    assert_eq!(pending[0].request.kind, PumpActionKind::Unlock);
    assert_eq!(pending[0].request.word.as_deref(), Some("slice"));
    assert_eq!(pending[0].request.reason, "ok");
    assert_eq!(pending[1].request.kind, PumpActionKind::Unlock);
    assert_eq!(pending[1].request.word, None);
}
