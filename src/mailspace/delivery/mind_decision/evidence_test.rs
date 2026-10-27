//! Companion tests for mind decision evidence discovery.

use super::super::{DECISION_MARKER_HEADER, decision_marker};
use super::find_decision_evidence;
use crate::mailspace::{DeliveryResult, Mailspace, SendRequest};
use crate::storage::MessageIngestRequest;

const KEY: &str = "need:b67c125b run:4 wait:slice-76a6fb5f-a1";
const OTHER_KEY: &str = "need:b67c125b run:4 wait:slice-other-attempt";
const SENDER: &str = "pump";
const MIND: &str = "mind";
const REMOTE: &str = "clerk@3f9a1c7e.local";

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity(MIND).unwrap();
    mailspace.add_identity(SENDER).unwrap();
    (tmp, mailspace)
}

fn request(from: &str, to: &str, reply_to: Option<String>) -> SendRequest {
    SendRequest {
        from: from.into(),
        to: vec![to.into()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "decision request: slice-76a6fb5f-a1".into(),
        body: "decide the parked slice".into(),
        role: "inbox".into(),
        kind: Some("mail".into()),
        reply_to,
    }
}

fn send_marked(
    mailspace: &Mailspace,
    key: &str,
    from: &str,
    to: &str,
    reply_to: Option<String>,
) -> DeliveryResult {
    let marker = decision_marker(key).unwrap();
    mailspace
        .send_with_extras(
            request(from, to, reply_to),
            &[(DECISION_MARKER_HEADER.into(), marker)],
        )
        .unwrap()
}

fn evidence(mailspace: &Mailspace, key: &str) -> super::DecisionEvidence {
    evidence_as(mailspace, key, SENDER, MIND)
}

fn evidence_as(
    mailspace: &Mailspace,
    key: &str,
    sender: &str,
    recipient: &str,
) -> super::DecisionEvidence {
    let storage = mailspace.storage().unwrap();
    find_decision_evidence(&storage, key, sender, recipient).unwrap()
}

/// A marked copy ingested straight into storage: no send events, no link,
/// and no other copy — the shape of a delivery recovered from bytes alone.
fn raw_copy(mailspace: &Mailspace, key: &str, account: &str, role: &str) -> String {
    let marker = decision_marker(key).unwrap();
    let eml = format!(
        "From: {SENDER}@vivi.local\r\nTo: {MIND}@vivi.local\r\n\
         Subject: decision request: slice-76a6fb5f-a1\r\n\
         Message-ID: <decision-raw@vivi.local>\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\n\
         {DECISION_MARKER_HEADER}: {marker}\r\n\r\nbody\r\n"
    );
    let storage = mailspace.storage().unwrap();
    let stored = storage
        .ingest_message(
            &MessageIngestRequest {
                account: account.into(),
                local_role: role.into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: format!("evidence-test\0{account}\0{role}"),
                remote: None,
            },
            eml.as_bytes(),
        )
        .unwrap();
    storage.display_handle(&stored.message_id).unwrap()
}

#[test]
fn recipient_only_evidence_retains_bytes_and_copy_identity() {
    let (_tmp, mailspace) = space();
    let handle = raw_copy(&mailspace, KEY, MIND, "inbox");
    let found = evidence(&mailspace, KEY);
    assert!(found.recipient_present());
    assert!(!found.sent_present());
    assert!(!found.required_events.sent_copy_created);
    assert!(!found.required_events.delivered);
    assert!(!found.complete());
    let copy = found.recipient.unwrap();
    assert_eq!(copy.handle, handle);
    assert_eq!(copy.account, MIND);
    assert_eq!(copy.local_role, "inbox");
    assert!(!copy.message_id.is_empty());
    assert!(!copy.content_id.is_empty());
    assert!(String::from_utf8_lossy(&copy.bytes).contains(DECISION_MARKER_HEADER));
    let stored = mailspace
        .storage()
        .unwrap()
        .read_message(&copy.message_id)
        .unwrap();
    assert_eq!(copy.bytes, stored);
}

#[test]
fn an_eventless_sent_copy_is_recovery_evidence() {
    let (_tmp, mailspace) = space();
    raw_copy(&mailspace, KEY, SENDER, "sent");
    let found = evidence(&mailspace, KEY);
    assert!(found.recipient.is_none());
    assert!(!found.required_events.sent_copy_created);
    assert!(!found.required_events.delivered);
    assert!(!found.complete());
    let sent = found.sent.unwrap();
    assert_eq!(sent.account, SENDER);
    assert_eq!(sent.local_role, "sent");
}

#[test]
fn sent_only_evidence_when_the_recipient_is_not_local() {
    let (_tmp, mailspace) = space();
    let result = send_marked(&mailspace, KEY, MIND, REMOTE, None);
    assert!(result.delivered.is_empty());
    let found = evidence_as(&mailspace, KEY, MIND, MIND);
    assert!(found.recipient.is_none());
    assert!(found.required_events.sent_copy_created);
    assert!(!found.required_events.delivered);
    assert!(!found.complete());
    let sent = found.sent.unwrap();
    assert_eq!(sent.handle, result.sent);
    assert_eq!(sent.account, MIND);
    assert_eq!(sent.local_role, "sent");
}

#[test]
fn complete_evidence_for_a_delivered_decision() {
    let (_tmp, mailspace) = space();
    let parent = mailspace.send(request(MIND, MIND, None)).unwrap();
    let decision = send_marked(&mailspace, KEY, SENDER, MIND, Some(parent.sent.clone()));
    let found = evidence(&mailspace, KEY);
    assert!(found.recipient_present());
    assert!(found.sent_present());
    assert!(found.required_events.sent_copy_created);
    assert!(found.required_events.delivered);
    assert!(found.complete());
    let recipient = found.recipient.unwrap();
    assert_eq!(recipient.handle, decision.delivered[0].handle);
    assert_eq!(recipient.account, MIND);
    assert_eq!(recipient.local_role, "inbox");
    let sent = found.sent.unwrap();
    assert_eq!(sent.handle, decision.sent);
    assert_eq!(sent.account, SENDER);
    assert_eq!(sent.local_role, "sent");
    let link = found.step_parent_link.unwrap();
    assert_eq!(link.child_content_id, recipient.content_id);
    assert_ne!(link.parent_content_id, link.child_content_id);
    let storage = mailspace.storage().unwrap();
    assert!(
        storage
            .reply_child_contents(&link.parent_content_id)
            .unwrap()
            .contains(&recipient.content_id)
    );
}

#[test]
fn a_decision_without_a_step_parent_link_reports_the_gap() {
    let (_tmp, mailspace) = space();
    send_marked(&mailspace, KEY, SENDER, MIND, None);
    let found = evidence(&mailspace, KEY);
    assert!(found.recipient_present());
    assert!(found.sent_present());
    assert!(found.required_events_present());
    assert!(!found.step_parent_link_present());
    assert!(!found.complete());
}

#[test]
fn a_moved_recipient_copy_is_still_the_mind_evidence() {
    let (_tmp, mailspace) = space();
    let parent = mailspace.send(request(SENDER, MIND, None)).unwrap();
    let handle = send_marked(&mailspace, KEY, SENDER, MIND, Some(parent.sent.clone())).delivered[0]
        .handle
        .clone();
    mailspace
        .move_item(MIND, &handle, "done", None, "close", None)
        .unwrap();
    let found = evidence(&mailspace, KEY);
    assert!(found.sent_present());
    assert!(found.required_events_present());
    assert!(found.complete());
    let recipient = found.recipient.unwrap();
    assert_eq!(recipient.handle, handle);
    assert_eq!(recipient.local_role, "done");
}

#[test]
fn unrelated_markers_and_plain_letters_are_ignored() {
    let (_tmp, mailspace) = space();
    send_marked(&mailspace, OTHER_KEY, SENDER, MIND, None);
    mailspace.send(request(SENDER, MIND, None)).unwrap();
    let found = evidence(&mailspace, KEY);
    assert!(found.recipient.is_none());
    assert!(found.sent.is_none());
    assert_eq!(found.marker, decision_marker(KEY).unwrap());
    assert!(!found.complete());
    let other = evidence(&mailspace, OTHER_KEY);
    assert!(other.recipient.is_some());
    assert!(other.sent.is_some());
}

#[test]
fn sender_and_recipient_are_both_mind_and_both_copies_are_found() {
    let (_tmp, mailspace) = space();
    let parent = mailspace.send(request(SENDER, MIND, None)).unwrap();
    let result = send_marked(&mailspace, KEY, MIND, MIND, Some(parent.sent.clone()));
    assert_eq!(result.delivered.len(), 1);
    let found = evidence_as(&mailspace, KEY, MIND, MIND);
    assert!(found.recipient_present());
    assert!(found.sent_present());
    assert!(found.complete());
    let recipient = found.recipient.unwrap();
    let sent = found.sent.unwrap();
    assert_eq!(recipient.handle, result.delivered[0].handle);
    assert_eq!(sent.handle, result.sent);
    assert_eq!(recipient.local_role, "inbox");
    assert_eq!(sent.local_role, "sent");
    assert_ne!(recipient.message_id, sent.message_id);
    assert_eq!(recipient.content_id, sent.content_id);
}

#[test]
fn the_marker_is_stable_canonical_and_keyed() {
    assert_eq!(DECISION_MARKER_HEADER, "X-Vivi-Mind-Decision");
    assert_eq!(decision_marker(KEY).unwrap(), decision_marker(KEY).unwrap());
    assert_eq!(
        decision_marker("  key ").unwrap(),
        decision_marker("key").unwrap()
    );
    assert_ne!(
        decision_marker(KEY).unwrap(),
        decision_marker(OTHER_KEY).unwrap()
    );
    assert_eq!(decision_marker(KEY).unwrap().len(), 64);
    assert!(decision_marker("").is_err());
    assert!(decision_marker("   ").is_err());
}
