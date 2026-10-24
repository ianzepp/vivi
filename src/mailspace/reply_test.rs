//! Round trips for the note reply: plain prose, the close facts of a local
//! report, and a pool event note.

use super::*;
use crate::letter::plain::CloseFront;
use crate::letter::records::PoolNote;
use crate::mailspace::SendRequest;

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
    let parsed = crate::letter::of_message(&reply.data, "t").unwrap();
    assert_eq!(parsed.kind, crate::letter::LetterKind::Mail);
    assert_eq!(
        parsed.front,
        crate::letter::Front::Mail(MailFront::default())
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
    let parsed = crate::letter::of_message(&reply.data, "t").unwrap();
    assert_eq!(parsed.kind, crate::letter::LetterKind::Mail);
    let crate::letter::Front::Mail(front) = parsed.front else {
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
    let parsed = crate::letter::of_message(&reply.data, "t").unwrap();
    assert_eq!(parsed.kind, crate::letter::LetterKind::Mail);
    let crate::letter::Front::Mail(front) = parsed.front else {
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
