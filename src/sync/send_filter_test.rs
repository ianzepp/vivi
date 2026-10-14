//! `send = "addressed"`: what leaves this machine.
//!
//! The default (`all`, or no key) is today's behavior, letter for letter.
//! With `addressed`, a letter goes only when a recipient address is in
//! another mailspace's domain, or it is an event letter for an item already
//! bound to this project, so an event for work that has left the machine
//! follows it. Everything else is machine-local traffic, counted as `local
//! only` and never unsent.

use crate::mailspace::SendRequest;
use crate::sync::fake_server::{Checkout, FakeServer, checkout};
use crate::sync::push::{self, PushReport};
use crate::sync::select::{Selection, SkipReason};
use crate::sync::{SendFilter, SyncSettings};

/// A checkout whose `[sync]` `send` key is `value` (`None` leaves it absent).
fn sent_as(name: &str, roles: &[&str], value: Option<&str>) -> Checkout {
    let mut co = checkout(name, roles);
    let sync = co.mailspace.config.sync.as_mut().unwrap();
    sync.send = value.map(str::to_string);
    co.settings = SyncSettings::resolve(&co.mailspace).unwrap();
    co
}

/// A task the way `vivi task send` writes it; returns the printed handle.
fn send_task(co: &Checkout, to: &str, subject: &str) -> String {
    let result = co
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec![to.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: format!("{subject} body\n"),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    result
        .delivered
        .first()
        .map_or(result.sent, |delivered| delivered.handle.clone())
}

fn pending(co: &Checkout) -> Selection {
    push::pending(&co.mailspace, &co.settings).unwrap()
}

#[test]
fn send_all_or_absent_selects_local_letters_as_before() {
    for value in [None, Some("all")] {
        let server = FakeServer::new();
        let a = sent_as("alice", &["mind", "hand"], value);
        let task = send_task(&a, &a.address("hand"), "a local task");
        let selection = pending(&a);
        assert_eq!(selection.letters.len(), 1, "{value:?}: {selection:?}");
        assert_eq!(selection.letters[0].handle, task, "{value:?}");
        assert!(selection.skipped.is_empty(), "{value:?}");
        assert_eq!(selection.unsent(), 1, "{value:?}");
        assert_eq!(a.push(&a.client(&server)).sent, 1, "{value:?}");
    }
}

#[test]
fn addressed_local_only_letter_is_counted_not_unsent() {
    let a = sent_as("alice", &["mind", "hand"], Some("addressed"));
    let task = send_task(&a, &a.address("hand"), "a local task");
    let selection = pending(&a);
    assert!(selection.letters.is_empty(), "{selection:?}");
    assert_eq!(selection.held_back, 0);
    assert_eq!(selection.unsent(), 0);
    assert_eq!(selection.skipped.len(), 1, "{selection:?}");
    assert_eq!(selection.skipped[0].handle, task);
    assert_eq!(selection.skipped[0].reason, SkipReason::LocalOnly);
    assert!(!selection.skipped[0].reason.actionable());
    assert_eq!(
        selection.skipped[0].reason.label(),
        "local only (send = addressed)"
    );
    let report = PushReport {
        skipped: selection.skipped,
        ..PushReport::default()
    };
    assert!(
        report.lines()[0].contains("not synced by design: 1 local only (send = addressed)"),
        "{}",
        report.lines()[0]
    );
    assert_eq!(report.failure(), None);
}

#[test]
fn addressed_need_to_a_remote_member_is_selected() {
    let server = FakeServer::new();
    let a = sent_as("alice", &["mind"], Some("addressed"));
    let b = checkout("bob", &["mind"]);
    let need = a.send_need("mind", &b.address("mind"), "remote need");
    let selection = pending(&a);
    assert_eq!(selection.letters.len(), 1, "{selection:?}");
    assert_eq!(selection.letters[0].handle, need);
    assert_eq!(selection.letters[0].kind, "need");
    assert_eq!(selection.unsent(), 1);
    assert_eq!(a.push(&a.client(&server)).sent, 1);
}

#[test]
fn addressed_reply_to_a_remote_member_is_selected() {
    let a = sent_as("alice", &["mind"], Some("addressed"));
    let b = checkout("bob", &["mind"]);
    let need = a.send_need("mind", &b.address("mind"), "remote need");
    let reply = a.reply(&need, "mind", &b.address("mind"), "an answer");
    let selection = pending(&a);
    let handles: Vec<&str> = selection
        .letters
        .iter()
        .map(|letter| letter.handle.as_str())
        .collect();
    assert!(handles.contains(&need.as_str()), "{handles:?}");
    assert!(handles.contains(&reply.as_str()), "{handles:?}");
}

#[test]
fn addressed_event_letter_follows_bound_work_and_stays_home_otherwise() {
    let server = FakeServer::new();
    // Bound: the item left this machine while every letter was sent.
    let mut a = checkout("alice", &["mind", "hand"]);
    let away = send_task(&a, &a.address("hand"), "sent away");
    assert_eq!(a.push(&a.client(&server)).sent, 1);
    a.mailspace.config.sync.as_mut().unwrap().send = Some("addressed".into());
    a.settings = SyncSettings::resolve(&a.mailspace).unwrap();
    a.mailspace
        .move_task("mind", &away, "done", None, None, &[], &[])
        .unwrap();
    let selection = pending(&a);
    assert_eq!(selection.letters.len(), 1, "{selection:?}");
    assert!(selection.skipped.is_empty(), "{selection:?}");
    assert_eq!(selection.unsent(), 1);

    // Unbound: the event on a purely local item is local too.
    let b = sent_as("bella", &["mind", "hand"], Some("addressed"));
    let home = send_task(&b, &b.address("hand"), "kept home");
    b.mailspace
        .move_task("mind", &home, "done", None, None, &[], &[])
        .unwrap();
    let selection = pending(&b);
    assert!(selection.letters.is_empty(), "{selection:?}");
    assert_eq!(selection.unsent(), 0);
    assert_eq!(selection.skipped.len(), 2, "{selection:?}");
    assert!(
        selection
            .skipped
            .iter()
            .all(|skipped| skipped.reason == SkipReason::LocalOnly),
        "{selection:?}"
    );
}

#[test]
fn an_unknown_send_value_is_a_config_error() {
    let mut co = checkout("alice", &["mind"]);
    co.mailspace.config.sync.as_mut().unwrap().send = Some("everything".into());
    let error = SyncSettings::resolve(&co.mailspace)
        .unwrap_err()
        .to_string();
    assert!(error.contains("[sync] send"), "{error}");
    assert!(error.contains("\"all\""), "{error}");
    assert!(error.contains("\"addressed\""), "{error}");
    assert_eq!(SendFilter::parse(None).unwrap(), SendFilter::All);
    assert_eq!(
        SendFilter::parse(Some("addressed")).unwrap(),
        SendFilter::Addressed
    );
}
