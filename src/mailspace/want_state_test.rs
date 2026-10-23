use std::collections::BTreeMap;

use super::{WantState, fold, refuse_hand_close};
use crate::letter::fixture::{need_body_with, want_body};
use crate::letter::work::NeedFront;
use crate::mailspace::{Mailspace, SendRequest};

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity("hand").unwrap();
    (tmp, mailspace)
}

fn send(mailspace: &Mailspace, kind: &str, body: String, reply_to: Option<&str>) -> String {
    let result = mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: format!("{kind} subject"),
            body,
            role: format!("{kind}s"),
            kind: Some(kind.into()),
            reply_to: reply_to.map(str::to_string),
        })
        .unwrap();
    result.delivered[0].handle.clone()
}

fn want(mailspace: &Mailspace) -> String {
    send(mailspace, "want", want_body("q", "p"), None)
}

fn need(mailspace: &Mailspace, serves: Option<&str>, explores: Option<&str>) -> String {
    let mut front = NeedFront::new(vec!["done".to_string()]);
    front.serves = serves.map(str::to_string);
    front.explores = explores.map(str::to_string);
    send(mailspace, "need", need_body_with(&front, "prose"), None)
}

fn close(mailspace: &Mailspace, handle: &str, verdict: Option<&str>) {
    let metadata = verdict.map(|v| BTreeMap::from([("verdict".to_string(), v.to_string())]));
    mailspace
        .move_item("mind", handle, "done", None, "close", metadata.as_ref())
        .unwrap();
}

fn state(mailspace: &Mailspace, want: &str) -> WantState {
    mailspace.want_state(want).unwrap().state
}

#[test]
fn fold_follows_the_rule_table() {
    assert_eq!(fold(false, []), WantState::Open);
    assert_eq!(fold(true, []), WantState::Done);
    assert_eq!(fold(false, [false]), WantState::Open);
    assert_eq!(fold(false, [true]), WantState::Done);
    assert_eq!(fold(false, [true, false]), WantState::Open);
    assert_eq!(fold(false, [true, true]), WantState::Done);
    assert_eq!(fold(true, [false]), WantState::Open);
    assert_eq!(fold(true, [true, false]), WantState::Open);
}

#[test]
fn a_want_with_no_serving_needs_is_open_until_closed_by_hand() {
    let (_tmp, mailspace) = space();
    let want = want(&mailspace);
    let report = mailspace.want_state(&want).unwrap();
    assert_eq!(report.state, WantState::Open);
    assert!(report.serving.is_empty() && !report.closed_by_hand);
    assert_eq!(report.summary(), "open (no serving needs)");

    close(&mailspace, &want, None);
    let report = mailspace.want_state(&want).unwrap();
    assert_eq!(report.state, WantState::Done);
    assert!(report.closed_by_hand);
    assert_eq!(report.summary(), "done (closed by hand; no serving needs)");
}

#[test]
fn serving_needs_decide_the_state() {
    let (_tmp, mailspace) = space();
    let want = want(&mailspace);
    let first = need(&mailspace, Some(&want), None);
    let report = mailspace.want_state(&want).unwrap();
    assert_eq!(report.state, WantState::Open);
    assert_eq!(report.serving.len(), 1);
    assert_eq!(report.serving[0].handle, first);
    assert_eq!(report.serving[0].subject, "need subject");
    assert!(!report.serving[0].done && report.serving[0].verdict.is_none());

    close(&mailspace, &first, None);
    assert_eq!(state(&mailspace, &want), WantState::Done);

    let second = need(&mailspace, Some(&want), None);
    let report = mailspace.want_state(&want).unwrap();
    assert_eq!(report.state, WantState::Open);
    assert_eq!(report.summary(), "open (1 of 2 serving needs done)");

    close(&mailspace, &second, Some("withdrawn"));
    let report = mailspace.want_state(&want).unwrap();
    assert_eq!(report.state, WantState::Done);
    assert_eq!(report.summary(), "done (2 of 2 serving needs done)");
    let withdrawn = report.serving.iter().find(|n| n.handle == second).unwrap();
    assert_eq!(withdrawn.verdict.as_deref(), Some("withdrawn"));
}

#[test]
fn a_new_serving_need_wins_over_a_hand_close() {
    let (_tmp, mailspace) = space();
    let want = want(&mailspace);
    close(&mailspace, &want, None);
    assert_eq!(state(&mailspace, &want), WantState::Done);

    need(&mailspace, Some(&want), None);
    let report = mailspace.want_state(&want).unwrap();
    assert_eq!(report.state, WantState::Open);
    assert!(report.closed_by_hand);
}

#[test]
fn a_reply_on_a_done_want_keeps_it_done() {
    let (_tmp, mailspace) = space();
    let want = want(&mailspace);
    close(&mailspace, &want, None);
    send(
        &mailspace,
        "task",
        crate::letter::fixture::task_body("p"),
        Some(&want),
    );
    assert_eq!(state(&mailspace, &want), WantState::Done);
}

#[test]
fn a_reopened_serving_need_is_open() {
    let (_tmp, mailspace) = space();
    let want = want(&mailspace);
    let serving = need(&mailspace, Some(&want), None);
    close(&mailspace, &serving, None);
    assert_eq!(state(&mailspace, &want), WantState::Done);

    mailspace
        .move_item("mind", &serving, "needs", None, "need reopen", None)
        .unwrap();
    assert_eq!(state(&mailspace, &want), WantState::Open);
}

#[test]
fn an_exploring_need_never_counts() {
    let (_tmp, mailspace) = space();
    let want = want(&mailspace);
    need(&mailspace, None, Some(&want));
    let report = mailspace.want_state(&want).unwrap();
    assert_eq!(report.state, WantState::Open);
    assert!(report.serving.is_empty());
}

#[test]
fn want_state_refuses_a_need() {
    let (_tmp, mailspace) = space();
    let need = need(&mailspace, None, None);
    let err = mailspace.want_state(&need).unwrap_err().to_string();
    assert!(err.contains("is a need, not a want"), "{err}");
}

#[test]
fn refuse_hand_close_names_the_serving_needs() {
    let (_tmp, mailspace) = space();
    let want = want(&mailspace);
    let storage = mailspace.storage().unwrap();
    let row = storage.resolve_message_token(&want).unwrap();
    let view = storage.message_by_id(&row).unwrap().unwrap();
    refuse_hand_close(&storage, &view).unwrap();
    drop(storage);

    let serving = need(&mailspace, Some(&want), None);
    let storage = mailspace.storage().unwrap();
    let err = refuse_hand_close(&storage, &view).unwrap_err().to_string();
    assert!(
        err.contains(&format!("is served by needs {serving}")),
        "{err}"
    );
}

#[test]
fn want_states_of_keys_a_batch_by_content_id() {
    let (_tmp, mailspace) = space();
    let a = want(&mailspace);
    let b = want(&mailspace);
    need(&mailspace, Some(&a), None);
    let views: Vec<_> = {
        let storage = mailspace.storage().unwrap();
        [&a, &b]
            .iter()
            .map(|handle| {
                let row = storage.resolve_message_token(handle).unwrap();
                storage.message_by_id(&row).unwrap().unwrap()
            })
            .collect()
    };
    let states = mailspace.want_states_of(&views).unwrap();
    assert_eq!(states.len(), 2);
    assert_eq!(states[&views[0].content_id].serving.len(), 1);
    assert!(states[&views[1].content_id].serving.is_empty());
}

fn quiet_space() -> (tempfile::TempDir, Mailspace) {
    let (tmp, mut mailspace) = space();
    mailspace.config.absorb = Some(crate::mailspace::AbsorbConfig { quiet_hours: 0 });
    (tmp, mailspace)
}

fn need_with_sent(mailspace: &Mailspace, serves: &str) -> (String, String) {
    let mut front = NeedFront::new(vec!["done".to_string()]);
    front.serves = Some(serves.to_string());
    let result = mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "need subject".into(),
            body: need_body_with(&front, "prose"),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap();
    (result.delivered[0].handle.clone(), result.sent)
}

#[test]
fn absorbing_a_closed_serving_need_keeps_the_want_done() {
    let (_tmp, mailspace) = quiet_space();
    let want = want(&mailspace);
    let (serving, sent) = need_with_sent(&mailspace, &want);
    close(&mailspace, &serving, None);
    assert_eq!(state(&mailspace, &want), WantState::Done);

    // Both ends seal their copy once the need is done.
    mailspace.absorb("mind", &sent, None, "need").unwrap();
    mailspace.absorb("hand", &serving, None, "need").unwrap();
    let report = mailspace.want_state(&want).unwrap();
    assert_eq!(report.state, WantState::Done, "{report:?}");
    assert_eq!(report.summary(), "done (1 of 1 serving needs done)");

    let storage = mailspace.storage().unwrap();
    let row = storage.resolve_message_token(&want).unwrap();
    let view = storage.message_by_id(&row).unwrap().unwrap();
    assert!(refuse_hand_close(&storage, &view).is_err());
}

#[test]
fn an_assignee_absorbing_its_open_copy_keeps_the_want_open() {
    let (_tmp, mailspace) = quiet_space();
    let want = want(&mailspace);
    let closed = need(&mailspace, Some(&want), None);
    close(&mailspace, &closed, None);
    let open = need(&mailspace, Some(&want), None);
    assert_eq!(state(&mailspace, &want), WantState::Open);

    mailspace.absorb("hand", &open, None, "need").unwrap();
    let report = mailspace.want_state(&want).unwrap();
    assert_eq!(report.state, WantState::Open, "{report:?}");
    assert_eq!(report.summary(), "open (1 of 2 serving needs done)");
}

#[test]
fn an_unaccepted_assignee_report_does_not_close_the_serving_need() {
    let (_tmp, mailspace) = space();
    let want = want(&mailspace);
    let serving = need(&mailspace, Some(&want), None);

    // The assignee's `done` is a report: only its own row moves.
    mailspace
        .move_item("hand", &serving, "done", None, "report", None)
        .unwrap();
    let report = mailspace.want_state(&want).unwrap();
    assert_eq!(report.state, WantState::Open, "{report:?}");
    assert!(!report.serving[0].done);

    // The owner accepts it: now the need, and so the want, is done.
    close(&mailspace, &serving, None);
    let report = mailspace.want_state(&want).unwrap();
    assert_eq!(report.state, WantState::Done, "{report:?}");
    assert!(report.serving[0].done);
}
