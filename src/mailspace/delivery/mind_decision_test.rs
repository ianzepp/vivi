//! Companion tests for atomic mind decision delivery.

use super::{DECISION_MARKER_HEADER, DecisionEvents, decision_marker, find_decision_evidence};
use crate::mailspace::{Mailspace, RoleUpdate, SendRequest};
use crate::storage::{MessageIngestRequest, StoredMessageView};

const KEY: &str = "need:b67c125b run:4 wait:slice-6cc93604-a1";
const SENDER: &str = "pump";
const BODY: &str = "decide the parked slice";
/// The folder a sender's own copy is filed under.
const SENT: &str = "sent";

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity(SENDER).unwrap();
    set_kind(&mut mailspace, "mind");
    (tmp, mailspace)
}

fn set_kind(mailspace: &mut Mailspace, kind: &str) {
    mailspace
        .set_role(
            "mind",
            RoleUpdate {
                kind: Some(Some(kind.into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
}

fn request(from: &str, to: &str, reply_to: Option<String>) -> SendRequest {
    SendRequest {
        from: from.into(),
        to: vec![to.into()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "decision request: slice-6cc93604-a1".into(),
        body: "decide the parked slice".into(),
        role: "inbox".into(),
        kind: Some("mail".into()),
        reply_to,
    }
}

/// The affected step the notice replies to: a plain letter's sent handle.
fn step_parent(mailspace: &Mailspace) -> String {
    mailspace.send(request(SENDER, "mind", None)).unwrap().sent
}

fn evidence(mailspace: &Mailspace) -> super::DecisionEvidence {
    evidence_as(mailspace, SENDER, "mind")
}

fn evidence_as(mailspace: &Mailspace, sender: &str, recipient: &str) -> super::DecisionEvidence {
    let storage = mailspace.storage().unwrap();
    find_decision_evidence(&storage, KEY, sender, recipient).unwrap()
}

fn header_value(bytes: &[u8], name: &str) -> Option<String> {
    for line in bytes.split(|byte| *byte == b'\n') {
        let line = String::from_utf8_lossy(line);
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            break; // End of the header block.
        }
        let Some((header, value)) = line.split_once(':') else {
            continue;
        };
        if header.trim().eq_ignore_ascii_case(name) {
            return Some(value.trim().to_string());
        }
    }
    None
}

/// Every stored copy carrying this decision's marker, wherever moves left
/// them.
fn marked_copies(mailspace: &Mailspace) -> Vec<StoredMessageView> {
    let marker = decision_marker(KEY).unwrap();
    let storage = mailspace.storage().unwrap();
    storage
        .list_messages()
        .unwrap()
        .into_iter()
        .filter(|row| {
            header_value(
                &storage.read_message(&row.message_id).unwrap(),
                DECISION_MARKER_HEADER,
            )
            .as_deref()
                == Some(marker.as_str())
        })
        .collect()
}

/// The event types logged on one stored copy, in log order.
fn event_types(mailspace: &Mailspace, message_id: &str) -> Vec<String> {
    mailspace
        .storage()
        .unwrap()
        .list_mailspace_events_for_messages(&[message_id.to_string()])
        .unwrap()
        .remove(message_id)
        .unwrap_or_default()
        .into_iter()
        .map(|event| event.event_type)
        .collect()
}

/// The stored Message-ID of a message handle, as the reply headers carry it.
fn message_id_of(mailspace: &Mailspace, handle: &str) -> String {
    let storage = mailspace.storage().unwrap();
    let bytes = storage
        .read_message(&storage.resolve_message_token(handle).unwrap())
        .unwrap();
    mail_parser::MessageParser::default()
        .parse(&bytes)
        .unwrap()
        .message_id()
        .unwrap()
        .to_string()
}

/// The stored message id behind a handle.
fn storage_id(mailspace: &Mailspace, handle: &str) -> String {
    mailspace
        .storage()
        .unwrap()
        .resolve_message_token(handle)
        .unwrap()
}

/// The content id behind a handle.
fn content_id_of(mailspace: &Mailspace, handle: &str) -> String {
    mailspace
        .storage()
        .unwrap()
        .read_message(&storage_id(mailspace, handle))
        .map(|bytes| {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(bytes))
        })
        .unwrap()
}

/// A marked copy planted straight into storage: no events, no link, and no
/// other copy — the durable shape of an interrupted send. `reply_to` plants
/// the thread headers a real notice carries. The same bytes planted for two
/// sides share one content id, as the two copies of one send do.
fn raw_copy(mailspace: &Mailspace, account: &str, role: &str, reply_to: Option<&str>) -> String {
    let marker = decision_marker(KEY).unwrap();
    let reply_headers = reply_to
        .map(|id| format!("In-Reply-To: {id}\r\nReferences: {id}\r\n"))
        .unwrap_or_default();
    let eml = format!(
        "From: {SENDER}@vivi.local\r\nTo: mind@vivi.local\r\n\
         Subject: mind decision: {KEY}\r\n\
         Message-ID: <decision-raw@vivi.local>\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\n\
         {reply_headers}\
         {DECISION_MARKER_HEADER}: {marker}\r\n\r\nbody\r\n"
    );
    let storage = mailspace.storage().unwrap();
    let stored = storage
        .ingest_message(
            &MessageIngestRequest {
                account: account.into(),
                local_role: role.into(),
                read_state: role == SENT,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: format!("decision-recovery-test\0{account}\0{role}"),
                remote: None,
            },
            eml.as_bytes(),
        )
        .unwrap();
    storage.display_handle(&stored.message_id).unwrap()
}

#[test]
fn a_fresh_notice_reaches_the_mind_seat_alone() {
    let (_tmp, mailspace) = space();
    let parent = step_parent(&mailspace);
    let sent = mailspace
        .send_mind_decision(SENDER, &parent, KEY, BODY)
        .unwrap();
    assert!(!sent.already_complete);
    let found = evidence(&mailspace);
    assert!(found.complete());
    assert_eq!(
        found.recipient.as_ref().unwrap().handle,
        sent.recipient_handle
    );
    assert_eq!(found.sent.as_ref().unwrap().handle, sent.sent_handle);
    let storage = mailspace.storage().unwrap();
    let copy = found.recipient.unwrap();
    assert_eq!(copy.account, "mind");
    assert_eq!(copy.local_role, "inbox");
    let bytes = storage.read_message(&copy.message_id).unwrap();
    let mind_address = mailspace.address_for("mind");
    let to = header_value(&bytes, "To").unwrap();
    assert!(to.contains(&mind_address), "To '{to}' lacks {mind_address}");
    assert!(!to.contains(SENDER), "To '{to}' names anyone but the seat");
    assert_eq!(header_value(&bytes, "Cc"), None);
    assert_eq!(header_value(&bytes, "Bcc"), None);
    assert_eq!(
        header_value(&bytes, DECISION_MARKER_HEADER).as_deref(),
        Some(decision_marker(KEY).unwrap().as_str())
    );
    let parent_id = message_id_of(&mailspace, &parent);
    assert_eq!(
        header_value(&bytes, "In-Reply-To").as_deref(),
        Some(parent_id.as_str())
    );
    assert_eq!(
        header_value(&bytes, "References").as_deref(),
        Some(parent_id.as_str())
    );
    assert_eq!(event_types(&mailspace, &copy.message_id), ["delivered"]);
    let sent_copy = found.sent.unwrap();
    assert_eq!(sent_copy.account, SENDER);
    assert_eq!(sent_copy.local_role, "sent");
    assert_eq!(
        event_types(&mailspace, &sent_copy.message_id),
        ["sent_copy_created"]
    );
}

#[test]
fn decision_notice_failure_between_recipient_and_sent_is_retryable() {
    let (_tmp, mailspace) = space();
    let parent = step_parent(&mailspace);
    let failed = mailspace.send_mind_decision_failing_after_recipient(SENDER, &parent, KEY, BODY);
    assert!(failed.is_err());
    // No visible partial successful notice survives the rollback.
    let found = evidence(&mailspace);
    assert!(found.recipient.is_none());
    assert!(found.sent.is_none());
    assert!(!found.step_parent_link_present());
    assert_eq!(found.required_events, DecisionEvents::default());
    assert!(marked_copies(&mailspace).is_empty());
    // The retry creates exactly one logical delivery.
    let retry = mailspace
        .send_mind_decision(SENDER, &parent, KEY, BODY)
        .unwrap();
    assert!(!retry.already_complete);
    assert_eq!(marked_copies(&mailspace).len(), 2);
    let found = evidence(&mailspace);
    assert!(found.complete());
    let recipient = found.recipient.unwrap();
    let sent_copy = found.sent.unwrap();
    assert_eq!(recipient.handle, retry.recipient_handle);
    assert_eq!(sent_copy.handle, retry.sent_handle);
    assert_eq!(
        event_types(&mailspace, &recipient.message_id),
        ["delivered"]
    );
    assert_eq!(
        event_types(&mailspace, &sent_copy.message_id),
        ["sent_copy_created"]
    );
    // A repeated invocation creates no extra copy and no extra event.
    let again = mailspace
        .send_mind_decision(SENDER, &parent, KEY, BODY)
        .unwrap();
    assert!(again.already_complete);
    assert_eq!(again.recipient_handle, retry.recipient_handle);
    assert_eq!(again.sent_handle, retry.sent_handle);
    assert_eq!(marked_copies(&mailspace).len(), 2);
    assert_eq!(
        event_types(&mailspace, &recipient.message_id),
        ["delivered"]
    );
    assert_eq!(
        event_types(&mailspace, &sent_copy.message_id),
        ["sent_copy_created"]
    );
}

#[test]
fn a_complete_notice_is_success_that_stores_nothing() {
    let (_tmp, mailspace) = space();
    let parent = step_parent(&mailspace);
    let first = mailspace
        .send_mind_decision(SENDER, &parent, KEY, BODY)
        .unwrap();
    let again = mailspace
        .send_mind_decision(SENDER, &parent, KEY, BODY)
        .unwrap();
    assert!(again.already_complete);
    assert_eq!(again.recipient_handle, first.recipient_handle);
    assert_eq!(again.sent_handle, first.sent_handle);
    assert_eq!(marked_copies(&mailspace).len(), 2);
}

/// The post-recipient/pre-sent boundary state of an interrupted send: one
/// durable mind copy, no sent copy, no link, no events. Recovery — from a
/// freshly reopened mailspace, against durable state alone — keeps the
/// recipient copy byte-for-byte and makes the sent and thread evidence
/// durable, without redelivering the notice.
#[test]
fn recipient_only_decision_notice_is_reconciled() {
    let (tmp, mailspace) = space();
    let parent = step_parent(&mailspace);
    let planted = raw_copy(&mailspace, "mind", "inbox", None);
    let before = mailspace
        .storage()
        .unwrap()
        .read_message(&storage_id(&mailspace, &planted))
        .unwrap();
    drop(mailspace);
    let reopened = Mailspace::init(Some(tmp.path())).unwrap();
    let recovered = reopened
        .send_mind_decision(SENDER, &parent, KEY, BODY)
        .unwrap();
    assert!(!recovered.already_complete);
    assert_eq!(recovered.recipient_handle, planted);
    assert_eq!(marked_copies(&reopened).len(), 2);
    let found = evidence(&reopened);
    assert!(found.complete());
    let recipient = found.recipient.unwrap();
    // The mind copy is untouched: same identity, same bytes, same folder.
    assert_eq!(recipient.handle, planted);
    assert_eq!(recipient.local_role, "inbox");
    let storage = reopened.storage().unwrap();
    assert_eq!(storage.read_message(&recipient.message_id).unwrap(), before);
    // The sent copy and the thread link are newly durable.
    let sent_copy = found.sent.unwrap();
    assert_eq!(sent_copy.account, SENDER);
    assert_eq!(sent_copy.local_role, SENT);
    assert_eq!(sent_copy.content_id, recipient.content_id);
    assert_eq!(
        found.step_parent_link.unwrap().parent_content_id,
        content_id_of(&reopened, &parent)
    );
    assert_eq!(event_types(&reopened, &recipient.message_id), ["delivered"]);
    assert_eq!(
        event_types(&reopened, &sent_copy.message_id),
        ["sent_copy_created"]
    );
    assert!(
        event_types(&reopened, &storage_id(&reopened, &parent)).contains(&"replied".to_string())
    );
}

/// Sent-only evidence — a boundary state before the recipient copy was
/// stored — recovers the mind seat's copy from the sent bytes.
#[test]
fn sent_only_evidence_is_reconciled_from_the_sent_bytes() {
    let (_tmp, mailspace) = space();
    let parent = step_parent(&mailspace);
    let planted = raw_copy(&mailspace, SENDER, SENT, None);
    let recovered = mailspace
        .send_mind_decision(SENDER, &parent, KEY, BODY)
        .unwrap();
    assert!(!recovered.already_complete);
    assert_eq!(recovered.sent_handle, planted);
    assert_eq!(marked_copies(&mailspace).len(), 2);
    let found = evidence(&mailspace);
    assert!(found.complete());
    let sent_copy = found.sent.unwrap();
    assert_eq!(sent_copy.handle, planted);
    assert_eq!(
        event_types(&mailspace, &sent_copy.message_id),
        ["sent_copy_created"]
    );
    let recipient = found.recipient.unwrap();
    assert_eq!(recipient.account, "mind");
    assert_eq!(recipient.local_role, "inbox");
    assert_eq!(recipient.content_id, sent_copy.content_id);
    let storage = mailspace.storage().unwrap();
    assert_eq!(
        storage.read_message(&recipient.message_id).unwrap(),
        storage.read_message(&sent_copy.message_id).unwrap()
    );
    assert_eq!(
        event_types(&mailspace, &recipient.message_id),
        ["delivered"]
    );
}

/// Both copies on file but no step-thread link: recovery adds the link and
/// the missing events — and nothing else; a second pass is a no-op success.
#[test]
fn both_copies_without_a_link_gain_only_the_link_and_missing_events() {
    let (_tmp, mailspace) = space();
    let parent = step_parent(&mailspace);
    raw_copy(&mailspace, "mind", "inbox", None);
    raw_copy(&mailspace, SENDER, SENT, None);
    let recovered = mailspace
        .send_mind_decision(SENDER, &parent, KEY, BODY)
        .unwrap();
    assert!(!recovered.already_complete);
    assert_eq!(marked_copies(&mailspace).len(), 2);
    let found = evidence(&mailspace);
    assert!(found.complete());
    assert!(found.step_parent_link_present());
    let recipient = found.recipient.unwrap();
    let sent_copy = found.sent.unwrap();
    assert_eq!(
        event_types(&mailspace, &recipient.message_id),
        ["delivered"]
    );
    assert_eq!(
        event_types(&mailspace, &sent_copy.message_id),
        ["sent_copy_created"]
    );
    let again = mailspace
        .send_mind_decision(SENDER, &parent, KEY, BODY)
        .unwrap();
    assert!(again.already_complete);
    assert_eq!(again.recipient_handle, recipient.handle);
    assert_eq!(again.sent_handle, sent_copy.handle);
    assert_eq!(marked_copies(&mailspace).len(), 2);
    assert_eq!(
        event_types(&mailspace, &recipient.message_id),
        ["delivered"]
    );
    assert_eq!(
        event_types(&mailspace, &sent_copy.message_id),
        ["sent_copy_created"]
    );
}

/// A recipient copy a move filed away is still the delivery evidence: it is
/// recovered in place, folder and bytes unchanged.
#[test]
fn a_moved_recipient_copy_is_recovered_in_place() {
    let (_tmp, mailspace) = space();
    let parent = step_parent(&mailspace);
    let planted = raw_copy(&mailspace, "mind", "inbox", None);
    mailspace
        .move_item("mind", &planted, "done", None, "close", None)
        .unwrap();
    let recovered = mailspace
        .send_mind_decision(SENDER, &parent, KEY, BODY)
        .unwrap();
    assert!(!recovered.already_complete);
    assert_eq!(recovered.recipient_handle, planted);
    let found = evidence(&mailspace);
    assert!(found.complete());
    let recipient = found.recipient.unwrap();
    assert_eq!(recipient.handle, planted);
    assert_eq!(recipient.local_role, "done");
    assert_eq!(marked_copies(&mailspace).len(), 2);
    let events = event_types(&mailspace, &recipient.message_id);
    assert!(events.contains(&"delivered".to_string()), "{events:?}");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.as_str() == "delivered")
            .count(),
        1
    );
}

/// Sender and seat are both mind: recovery still keeps two distinct copies —
/// the inbox delivery and the sent filing — with one event each.
#[test]
fn sender_equal_to_mind_recovers_two_distinct_copies() {
    let (_tmp, mailspace) = space();
    let parent = step_parent(&mailspace);
    let planted = raw_copy(&mailspace, "mind", "inbox", None);
    let recovered = mailspace
        .send_mind_decision("mind", &parent, KEY, BODY)
        .unwrap();
    assert!(!recovered.already_complete);
    assert_eq!(recovered.recipient_handle, planted);
    let found = evidence_as(&mailspace, "mind", "mind");
    assert!(found.complete());
    let recipient = found.recipient.unwrap();
    let sent_copy = found.sent.unwrap();
    assert_eq!(recipient.handle, planted);
    assert_eq!(recipient.local_role, "inbox");
    assert_eq!(sent_copy.account, "mind");
    assert_eq!(sent_copy.local_role, SENT);
    assert_ne!(recipient.message_id, sent_copy.message_id);
    assert_eq!(recipient.content_id, sent_copy.content_id);
    assert_eq!(
        event_types(&mailspace, &recipient.message_id),
        ["delivered"]
    );
    assert_eq!(
        event_types(&mailspace, &sent_copy.message_id),
        ["sent_copy_created"]
    );
    assert_eq!(marked_copies(&mailspace).len(), 2);
}

/// A recovery failure rolls the whole batch back and leaves the delivery
/// retryable: nothing it wrote survives, and the store takes the next try.
#[test]
fn a_failed_recovery_rolls_back_and_stays_retryable() {
    let (_tmp, mailspace) = space();
    // The step letter itself carries the marker, so recovery would have to
    // link the notice to its own content — a refused write, mid-recovery.
    let marker = decision_marker(KEY).unwrap();
    let parent = mailspace
        .send_with_extras(
            request(SENDER, "mind", None),
            &[(DECISION_MARKER_HEADER.into(), marker)],
        )
        .unwrap()
        .sent;
    let copies_before = marked_copies(&mailspace).len();
    let failed = mailspace.send_mind_decision(SENDER, &parent, KEY, BODY);
    assert!(failed.is_err());
    let found = evidence(&mailspace);
    assert!(!found.step_parent_link_present());
    assert!(found.required_events_present());
    assert_eq!(marked_copies(&mailspace).len(), copies_before);
    // The retry fails the same way and still stores nothing: the batch
    // boundary, not the store, is what stopped.
    let retried = mailspace.send_mind_decision(SENDER, &parent, KEY, BODY);
    assert!(retried.is_err());
    assert_eq!(marked_copies(&mailspace).len(), copies_before);
}

#[test]
fn the_seat_must_be_a_mind_role_and_nothing_else() {
    let (tmp, mut mailspace) = space();
    let (bare_tmp, bare) = bare_space(&tmp);
    // No seat at all: no fallback to any other identity.
    let refused = bare
        .send_mind_decision(SENDER, "step-unknown", KEY, BODY)
        .unwrap_err();
    assert!(refused.to_string().contains("mind"), "{refused}");
    drop(bare_tmp);
    set_kind(&mut mailspace, "hand");
    let parent = step_parent(&mailspace);
    let refused = mailspace
        .send_mind_decision(SENDER, &parent, KEY, BODY)
        .unwrap_err();
    assert!(refused.to_string().contains("kind 'mind'"), "{refused}");
    // Never a fallback to the operator seat: an operator-kind seat stays
    // refused.
    set_kind(&mut mailspace, "operator");
    let refused = mailspace
        .send_mind_decision(SENDER, &parent, KEY, BODY)
        .unwrap_err();
    assert!(refused.to_string().contains("kind 'mind'"), "{refused}");
}

/// A mailspace with no `mind` seat, kept beside `tmp`'s mailspace.
fn bare_space(tmp: &tempfile::TempDir) -> (tempfile::TempDir, Mailspace) {
    let dir = tempfile::tempdir_in(tmp.path()).unwrap();
    let mailspace = Mailspace::init(Some(dir.path())).unwrap();
    (dir, mailspace)
}

#[test]
fn an_empty_key_or_a_missing_step_is_refused() {
    let (_tmp, mailspace) = space();
    let parent = step_parent(&mailspace);
    let refused = mailspace
        .send_mind_decision(SENDER, &parent, "   ", BODY)
        .unwrap_err();
    assert!(
        refused.to_string().contains("non-empty decision key"),
        "{refused}"
    );
    let refused = mailspace
        .send_mind_decision(SENDER, "no-such-step", KEY, BODY)
        .unwrap_err();
    assert!(refused.to_string().contains("no-such-step"), "{refused}");
}
