//! Round trips for the note reply: plain prose, the close facts of a local
//! report, and a pool event note.

use super::*;
use crate::mailspace::SendRequest;
use vivi_mail::letter::plain::CloseFront;
use vivi_mail::letter::records::PoolNote;

/// The content headers W3-B4 moved into front matter; none may appear on a
/// note letter.
const MOVED_HEADERS: [&str; 7] = [
    "X-Vivi-Verdict",
    "X-Vivi-Repo",
    "X-Vivi-Branch",
    "X-Vivi-Commit",
    "X-Vivi-Tag",
    "X-Vivi-Rule-Scope",
    "X-Vivi-Supersedes",
];

fn space_with_mail() -> (tempfile::TempDir, Mailspace, StoredMessageView) {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(tmp.path())).unwrap();
    space.add_identity("mind").unwrap();
    space.add_identity("hand").unwrap();
    let sent = space
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "unit".into(),
            body: "note reply target".into(),
            role: "inbox".into(),
            kind: Some("mail".into()),
            reply_to: None,
        })
        .unwrap();
    let parent = {
        let storage = space.storage().unwrap();
        let id = storage
            .resolve_message_token(&sent.delivered[0].handle)
            .unwrap();
        storage.message_by_id(&id).unwrap().unwrap()
    };
    (tmp, space, parent)
}

fn assert_no_moved_headers(text: &str) {
    for header in MOVED_HEADERS {
        assert!(
            !text.contains(&format!("{header}:")),
            "{header} stayed a header: {text}"
        );
    }
}

#[test]
fn a_plain_note_reply_is_prose_mail_with_the_format_header() {
    let (_tmp, space, parent) = space_with_mail();
    let storage = space.storage().unwrap();
    let reply = space
        .note_reply(
            &storage,
            "hand",
            &parent,
            "moved for review",
            MailFront::default(),
        )
        .unwrap();
    let text = String::from_utf8_lossy(&reply.data).into_owned();
    assert!(text.contains("X-Vivi-Format: 1"), "{text}");
    assert!(text.contains("X-Vivi-Kind: mail"), "{text}");
    assert!(!text.contains("+++"), "plain prose shows no fence: {text}");
    assert_no_moved_headers(&text);
    let parsed = vivi_mail::letter::of_message(&reply.data, "t").unwrap();
    assert_eq!(parsed.kind, vivi_mail::letter::LetterKind::Mail);
    assert_eq!(
        parsed.front,
        vivi_mail::letter::Front::Mail(MailFront::default())
    );
    assert_eq!(parsed.prose, "moved for review");
}

#[test]
fn a_report_note_carries_its_close_facts_as_front_matter() {
    let (_tmp, space, parent) = space_with_mail();
    let storage = space.storage().unwrap();
    let close = CloseFront {
        verdict: Some("fixed".into()),
        branch: Some("mind/review".into()),
        commit: Some("deadbeef".into()),
        repos: [("vivi".to_string(), "sha9".to_string())]
            .into_iter()
            .collect(),
    };
    let reply = space
        .note_reply(
            &storage,
            "hand",
            &parent,
            "done",
            MailFront {
                close: Some(close.clone()),
                ..MailFront::default()
            },
        )
        .unwrap();
    let text = String::from_utf8_lossy(&reply.data).into_owned();
    assert!(text.contains("X-Vivi-Format: 1"), "{text}");
    assert_no_moved_headers(&text);
    let parsed = vivi_mail::letter::of_message(&reply.data, "t").unwrap();
    assert_eq!(parsed.kind, vivi_mail::letter::LetterKind::Mail);
    let vivi_mail::letter::Front::Mail(front) = parsed.front else {
        panic!("not a mail front: {text}");
    };
    assert_eq!(front.close.as_ref(), Some(&close));
    assert_eq!(parsed.prose, "done");
}

#[test]
fn a_pool_note_carries_the_event_and_record_in_its_front() {
    let (_tmp, space, parent) = space_with_mail();
    let storage = space.storage().unwrap();
    let reply = space
        .note_reply(
            &storage,
            "hand",
            &parent,
            "dispatched to slot 1",
            MailFront {
                pool: Some(PoolNote {
                    event: "pool_dispatched".into(),
                    record: r#"{"job":"j1"}"#.into(),
                }),
                ..MailFront::default()
            },
        )
        .unwrap();
    let text = String::from_utf8_lossy(&reply.data).into_owned();
    assert!(text.contains("X-Vivi-Format: 1"), "{text}");
    assert_no_moved_headers(&text);
    let parsed = vivi_mail::letter::of_message(&reply.data, "t").unwrap();
    assert_eq!(parsed.kind, vivi_mail::letter::LetterKind::Mail);
    let vivi_mail::letter::Front::Mail(front) = parsed.front else {
        panic!("not a mail front: {text}");
    };
    let pool = front
        .pool
        .as_ref()
        .unwrap_or_else(|| panic!("no [pool] table: {text}"));
    assert_eq!(pool.event, "pool_dispatched");
    assert_eq!(pool.record, r#"{"job":"j1"}"#);
    assert_eq!(parsed.prose, "dispatched to slot 1");
}

#[test]
fn a_note_with_an_invalid_front_is_refused_at_write_time() {
    let (_tmp, space, parent) = space_with_mail();
    let storage = space.storage().unwrap();
    let error = space
        .note_reply(
            &storage,
            "hand",
            &parent,
            "dispatched",
            MailFront {
                pool: Some(PoolNote {
                    event: "pool_dispatched".into(),
                    record: " ".into(),
                }),
                ..MailFront::default()
            },
        )
        .err()
        .map(|err| err.to_string())
        .unwrap_or_else(|| panic!("the note was accepted"));
    assert!(error.contains("pool.record"), "{error}");
}

#[test]
fn unaddressed_reply_links_without_delivery() {
    let (_tmp, space, parent) = space_with_mail();
    let reply = space
        .reply(
            &parent.handle,
            "hand",
            Vec::new(),
            Vec::new(),
            None,
            "thread-only answer".into(),
        )
        .unwrap();
    assert!(reply.delivered.is_empty());
    assert!(reply.remote.is_empty());
    let storage = space.storage().unwrap();
    let sent = space.list("hand", "sent").unwrap();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].read_state);
    let bytes = storage.read_message(&sent[0].message_id).unwrap();
    let parsed = mail_parser::MessageParser::default().parse(&bytes).unwrap();
    assert!(parsed.to().is_none());
    assert!(parsed.cc().is_none());
    assert!(parsed.in_reply_to().as_text().is_some());
    assert!(parsed.references().as_text().is_some());
    let events = storage.list_mailspace_events(&sent[0].message_id).unwrap();
    assert!(!events.iter().any(|event| event.event_type == "delivered"));
    let parent_events = storage.list_mailspace_events(&parent.message_id).unwrap();
    assert!(
        parent_events
            .iter()
            .any(|event| event.event_type == "replied")
    );
    assert_eq!(
        space.thread(&parent.handle, false, 10, 10).unwrap().len(),
        2
    );
    assert_eq!(space.trace(&parent.handle, 10, 10).unwrap().nodes.len(), 2);

    let cc_reply = space
        .reply(
            &parent.handle,
            "hand",
            Vec::new(),
            vec!["mind".into()],
            None,
            "cc only".into(),
        )
        .unwrap();
    assert_eq!(cc_reply.delivered.len(), 1);
    assert_eq!(cc_reply.delivered[0].identity, "mind");
}

#[test]
fn a_reply_to_a_self_addressed_item_is_kept_on_its_thread() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(tmp.path())).unwrap();
    space.add_identity("operator").unwrap();
    let item = space
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["operator".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "own need".into(),
            body: "an item addressed to its author".into(),
            role: "inbox".into(),
            kind: Some("mail".into()),
            reply_to: None,
        })
        .unwrap();
    let reply = space
        .reply(
            &item.sent,
            "operator",
            Vec::new(),
            Vec::new(),
            None,
            "a reply as myself".into(),
        )
        .expect("replying to one's own item must not be refused");
    assert!(reply.delivered.is_empty(), "no recipients were supplied");
    assert!(reply.remote.is_empty());
    let sent = space.list("operator", "sent").unwrap();
    assert_eq!(sent.len(), 2);
    assert!(sent.iter().all(|copy| copy.read_state));
    let storage = space.storage().unwrap();
    let raw = sent
        .iter()
        .map(|copy| storage.read_message(&copy.message_id).unwrap())
        .find(|bytes| String::from_utf8_lossy(bytes).contains("a reply as myself"))
        .unwrap();
    let raw = String::from_utf8_lossy(&raw);
    assert!(!raw.contains("\nTo:"), "{raw}");
    assert!(!raw.contains("\nCc:"), "{raw}");
    assert!(raw.contains("In-Reply-To:"), "{raw}");
    let thread = space.thread(&item.sent, false, 10, 10).unwrap();
    assert_eq!(thread.len(), 2);
}
