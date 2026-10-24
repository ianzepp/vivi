//! The W3-B4 grep pin: every writer family composes through the gate, so
//! the composed bytes carry `X-Vivi-Format: 1` and none of the seven
//! content headers the wave moved into front matter. One test composes via
//! each writer and greps the raw bytes.

use super::*;
use crate::letter::plain::CloseFront;
use crate::letter::records::MailFront;
use std::collections::BTreeMap;

/// The content headers W3-B4 moved into front matter; no writer may emit
/// one.
const MOVED_HEADERS: [&str; 7] = [
    "X-Vivi-Verdict",
    "X-Vivi-Repo",
    "X-Vivi-Branch",
    "X-Vivi-Commit",
    "X-Vivi-Tag",
    "X-Vivi-Rule-Scope",
    "X-Vivi-Supersedes",
];

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    for role in ["mind", "hand", "other"] {
        mailspace.add_identity(role).unwrap();
    }
    (tmp, mailspace)
}

fn send_work(mailspace: &Mailspace, kind: &str, subject: &str) -> (String, String) {
    let result = mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: crate::letter::fixture::body_for(kind, "body"),
            role: format!("{kind}s"),
            kind: Some(kind.into()),
            reply_to: None,
        })
        .unwrap();
    (result.sent, result.delivered[0].handle.clone())
}

fn read(mailspace: &Mailspace, handle: &str) -> Vec<u8> {
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(handle).unwrap();
    storage.read_message(&id).unwrap()
}

/// Every stored letter whose header block carries `X-Vivi-Event: <verb>`.
fn event_letters(mailspace: &Mailspace, verb: &str) -> Vec<Vec<u8>> {
    let storage = mailspace.storage().unwrap();
    let header = format!("X-Vivi-Event: {verb}");
    storage
        .list_messages()
        .unwrap()
        .into_iter()
        .filter_map(|row| {
            let bytes = storage.read_message(&row.message_id).ok()?;
            let text = String::from_utf8_lossy(&bytes);
            text.lines()
                .any(|line| line.trim().eq_ignore_ascii_case(&header))
                .then_some(bytes)
        })
        .collect()
}

/// Lifecycle events: a close with verdict and repo tips, and a close whose
/// front is empty (stored with no fence).
fn event_writers(mailspace: &Mailspace) -> Vec<(&'static str, Vec<u8>)> {
    let mut metadata = BTreeMap::new();
    metadata.insert("verdict".to_string(), "fixed".to_string());
    metadata.insert("branch".to_string(), "mind/review".to_string());
    metadata.insert("commit".to_string(), "deadbeef".to_string());
    metadata.insert("repo:vivi".to_string(), "sha9".to_string());
    let (need, _) = send_work(mailspace, "need", "close with facts");
    let closed = mailspace
        .move_item_outcome("mind", &need, "done", None, "need done", Some(&metadata))
        .unwrap();
    let (task, _) = send_work(mailspace, "task", "close bare");
    let bare = mailspace
        .move_item_outcome("mind", &task, "done", None, "task done", None)
        .unwrap();
    vec![
        (
            "close event with verdict and commit",
            read(mailspace, &closed.event.unwrap()),
        ),
        ("empty-front event", read(mailspace, &bare.event.unwrap())),
    ]
}

/// Note replies: plain prose mail, the close facts of a local report, and a
/// pool result note — the composers `move_row_locally` and
/// `record_pool_note` call.
fn note_writers(mailspace: &Mailspace) -> Vec<(&'static str, Vec<u8>)> {
    let (task, _) = send_work(mailspace, "task", "note target");
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(&task).unwrap();
    let parent = storage.message_by_id(&id).unwrap().unwrap();
    let note = mailspace
        .note_reply(
            &storage,
            "mind",
            &parent,
            "moved for review",
            MailFront::default(),
        )
        .unwrap();
    let report = mailspace
        .note_reply(
            &storage,
            "mind",
            &parent,
            "done",
            MailFront {
                close: Some(CloseFront {
                    verdict: Some("fixed".into()),
                    branch: Some("mind/review".into()),
                    commit: Some("deadbeef".into()),
                    repos: BTreeMap::new(),
                }),
                ..MailFront::default()
            },
        )
        .unwrap();
    drop(storage);
    mailspace
        .record_pool_note(
            &task,
            "pool_dispatched",
            r#"{"job":"j1"}"#,
            "dispatched to slot 1",
        )
        .unwrap();
    let pool = mailspace
        .thread(&task, false, 20, 20)
        .unwrap()
        .into_iter()
        .find(|message| message.subject.starts_with("Re:"))
        .unwrap()
        .message_id;
    let pool_bytes = mailspace.storage().unwrap().read_message(&pool).unwrap();
    vec![
        ("note", note.data),
        ("report note", report.data),
        ("pool result note", pool_bytes),
    ]
}

/// Memos (tagged and bare) and a rule whose front carries scope and
/// supersedes.
fn memo_and_rule_writers(mailspace: &Mailspace) -> Vec<(&'static str, Vec<u8>)> {
    let tagged = mailspace
        .save_memo("mind", "tagged", "memo body", &["ops".into()])
        .unwrap();
    let bare = mailspace
        .save_memo("mind", "bare", "prose as written", &[])
        .unwrap();
    let first = mailspace
        .save_rule("mind", "first rule", "rule body", true)
        .unwrap();
    let superseded = mailspace
        .supersede_rule("mind", &first, "second rule", "rule body two", false)
        .unwrap();
    vec![
        ("memo with tags", read(mailspace, &tagged)),
        ("memo without tags", read(mailspace, &bare)),
        (
            "rule with scope and supersedes",
            read(mailspace, &superseded.handle),
        ),
    ]
}

/// Property letters: marks, bound, dependency, delegation and state.
fn property_writers(mailspace: &mut Mailspace) -> Vec<(&'static str, Vec<u8>)> {
    let (dep, _) = send_work(mailspace, "need", "prerequisite");
    let (release, _) = send_work(mailspace, "need", "release");
    mailspace
        .set_dependencies("mind", &release, "need", &[dep], &[])
        .unwrap();
    let (marked, _) = send_work(mailspace, "need", "mark me");
    mailspace
        .set_marks("mind", &marked, "need", true, &["ready".into()])
        .unwrap();
    let (_, parent) = send_work(mailspace, "need", "lower me");
    let (_, unit_task) = send_work(mailspace, "task", "unit");
    mailspace.backlog_bind_units(&parent, &[unit_task]).unwrap();
    let (delegate, _) = send_work(mailspace, "need", "delegate me");
    let assigned = mailspace.assign_need("mind", &delegate, "other").unwrap();
    mailspace.config.absorb = Some(AbsorbConfig { quiet_hours: 0 });
    let (sealed, _) = send_work(mailspace, "need", "seal me");
    mailspace
        .move_item("mind", &sealed, "done", None, "need done", None)
        .unwrap();
    mailspace
        .absorb("mind", &sealed, Some("sealed the row"), "need")
        .unwrap();
    let one = |verb: &str| {
        let letters = event_letters(mailspace, verb);
        (!letters.is_empty())
            .then(|| letters)
            .unwrap_or_else(|| panic!("no {verb} letter"))
    };
    vec![
        ("marks property letter", one("marked").remove(0)),
        ("bound property letter", one("bound").remove(0)),
        (
            "dependency property letter",
            one("dependency-added").remove(0),
        ),
        ("delegation property letter", read(mailspace, &assigned)),
        ("state property letter", one("absorbed").remove(0)),
    ]
}

/// The task-from-source writer: `vivi task from` over a need.
fn task_from_writer(mailspace: &Mailspace) -> Vec<(&'static str, Vec<u8>)> {
    let (need, _) = send_work(mailspace, "need", "lower me by task from");
    let result = mailspace
        .task_from_source(SourceTaskRequest {
            source_handle: need,
            actor: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            subject: "unit from source".into(),
            body: "do the first part".into(),
        })
        .unwrap();
    vec![("task from source", read(mailspace, &result.sent))]
}

#[test]
fn every_writer_family_composes_format_one_with_no_moved_content_header() {
    let (_tmp, mut mailspace) = space();
    let mut letters = Vec::new();
    letters.extend(event_writers(&mailspace));
    letters.extend(note_writers(&mailspace));
    letters.extend(memo_and_rule_writers(&mailspace));
    letters.extend(property_writers(&mut mailspace));
    letters.extend(task_from_writer(&mailspace));
    assert_eq!(letters.len(), 14, "a writer family went missing");
    for (family, bytes) in &letters {
        let text = String::from_utf8_lossy(bytes);
        assert!(
            text.contains("X-Vivi-Format: 1"),
            "{family} lacks the format header: {text}"
        );
        for header in MOVED_HEADERS {
            assert!(
                !text.contains(&format!("{header}:")),
                "{family} still writes {header}: {text}"
            );
        }
    }
}
