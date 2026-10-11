use crate::chat::b64;
use crate::storage::{
    MessageIngestRequest, RemoteBindingInput, StoredMessageView, is_traveling_id, sha256_hex,
};
use crate::sync::fake_server::{Checkout, FakeServer, PROJECT, checkout};
use crate::sync::state::SyncState;
use crate::sync::sync_dir;
use crate::sync::transport::{AppendFailure, Appended, Envelope, Page, Record, Transport};
use crate::{VivariumError, sync::pull};

const ID_A: &str = "msg_0000000000000000000000a1";
const ID_B: &str = "msg_0000000000000000000000b2";

fn eml(subject: &str) -> Vec<u8> {
    format!(
        "From: alice@x.test\r\nTo: bob@x.test\r\nSubject: {subject}\r\n\
         Message-ID: <{subject}@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\nbody\r\n"
    )
    .into_bytes()
}

fn record(sequence: u64, id: &str, from: &str, kind: Option<&str>, bytes: &[u8]) -> Record {
    Record {
        sequence,
        message_id: id.into(),
        from: from.into(),
        to: None,
        kind: kind.map(String::from),
        content_sha256: sha256_hex(bytes),
        raw_eml: b64::encode(bytes),
    }
}

/// A transport that serves fixed pages and refuses appends.
struct Canned {
    pages: Vec<Page>,
}

impl Transport for Canned {
    fn append(&self, _: &str, _: &Envelope<'_>) -> Result<Appended, AppendFailure> {
        Err(AppendFailure::Stop("canned".into()))
    }

    fn fetch(&self, since: u64) -> Result<Page, VivariumError> {
        Ok(self
            .pages
            .iter()
            .find(|page| page.records.first().is_some_and(|r| r.sequence > since))
            .cloned()
            .unwrap_or(Page {
                records: Vec::new(),
                next_cursor: since,
            }))
    }
}

fn pull_canned(fx: &Checkout, records: Vec<Record>) -> pull::PullReport {
    let next_cursor = records.last().map_or(0, |r| r.sequence);
    let canned = Canned {
        pages: vec![Page {
            records,
            next_cursor,
        }],
    };
    pull::pull(&fx.mailspace, &fx.settings, &canned).unwrap()
}

#[test]
fn the_summary_names_roles_counts_links_refusals_and_the_cursor() {
    let fx = checkout("bob", "mind", &["mind"], &[]);
    let need = eml("need-one");
    let note = eml("note-one");
    let records = vec![
        record(1, ID_A, "alice", Some("need"), &need),
        record(2, ID_B, "alice", Some("mail"), &note),
        record(3, "legacy-id", "alice", None, &eml("legacy")),
    ];

    let report = pull_canned(&fx, records);
    let summary = report.summary();

    assert_eq!(
        summary,
        "sync pull: 3 fetched, 2 new (1 inbox, 1 needs), 0 already present, 0 reply link(s), \
         1 refused: legacy-id, cursor 3"
    );
}

#[test]
fn letters_file_by_the_envelope_kind_and_the_wire_id_is_kept() {
    let fx = checkout("bob", "mind", &["mind"], &[]);
    let records = vec![
        record(1, ID_A, "alice", Some("task"), &eml("task-one")),
        record(2, ID_B, "alice", Some("want"), &eml("want-one")),
        record(
            3,
            "msg_0000000000000000000000c3",
            "alice",
            Some("novel"),
            &eml("odd"),
        ),
    ];

    let report = pull_canned(&fx, records);

    assert_eq!(report.filed.get("tasks"), Some(&1));
    assert_eq!(report.filed.get("wants"), Some(&1));
    assert_eq!(
        report.filed.get("inbox"),
        Some(&1),
        "unknown kinds are mail"
    );
    for id in [ID_A, ID_B] {
        let row = fx.row(id).unwrap();
        assert_eq!(row.account, "mind");
        assert!(is_traveling_id(&row.message_id));
    }
    let events = fx
        .mailspace
        .storage()
        .unwrap()
        .list_mailspace_events(ID_A)
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].command, "sync pull");
    assert_eq!(events[0].event_type, "delivered");
}

#[test]
fn the_checkouts_own_letters_file_as_read_sent_copies_without_backlog() {
    let fx = checkout("bob", "mind", &["mind"], &[]);

    let report = pull_canned(
        &fx,
        vec![record(1, ID_A, "bob", Some("need"), &eml("mine"))],
    );

    assert_eq!(report.filed.get("sent"), Some(&1));
    let row = fx.row(ID_A).unwrap();
    assert_eq!(row.local_role, "sent");
    assert!(row.read_state);
    assert!(!fx.mailspace.backlog_item_tracked(&row.handle).unwrap());
}

#[test]
fn content_already_held_under_another_id_is_not_filed_twice() {
    let fx = checkout("bob", "mind", &["mind"], &[]);
    let bytes = eml("same-bytes");
    pull_canned(&fx, vec![record(1, ID_A, "alice", Some("need"), &bytes)]);

    let report = pull_canned(&fx, vec![record(2, ID_B, "alice", Some("need"), &bytes)]);

    assert_eq!(report.existing, 1);
    assert!(report.filed.is_empty());
    assert!(fx.row(ID_B).is_none());
    assert_eq!(fx.rows().len(), 1);
}

#[test]
fn an_id_held_with_different_bytes_is_refused_not_swapped_in() {
    let fx = checkout("bob", "mind", &["mind"], &[]);
    let original = eml("original");
    pull_canned(&fx, vec![record(1, ID_A, "alice", Some("need"), &original)]);

    let report = pull_canned(
        &fx,
        vec![record(2, ID_A, "mallory", Some("need"), &eml("different"))],
    );

    assert_eq!(report.refused.len(), 1);
    assert_eq!(report.refused[0].message_id, ID_A);
    assert_eq!(fx.rows().len(), 1);
    assert_eq!(fx.row(ID_A).unwrap().content_id, sha256_hex(&original));
}

#[test]
fn an_undecodable_record_is_refused() {
    let fx = checkout("bob", "mind", &["mind"], &[]);
    let mut bad = record(1, ID_A, "alice", None, &eml("x"));
    bad.raw_eml = "!!!not base64".into();

    let report = pull_canned(&fx, vec![bad]);

    assert_eq!(report.refused.len(), 1);
    assert!(fx.rows().is_empty());
}

#[test]
fn the_cursor_is_stored_and_the_next_pull_resumes_after_it() {
    let server = FakeServer::new();
    let (a, b) = (
        checkout("alice", "mind", &["mind", "hand"], &[]),
        checkout("bob", "mind", &["mind"], &[]),
    );
    let (ca, cb) = (server.client("alice"), server.client("bob"));
    a.send_need("mind", "hand", "first");
    a.send_need("mind", "hand", "second");
    a.push(&ca);

    let first = b.pull(&cb);
    assert_eq!((first.fetched, first.cursor), (2, 2));
    let state = SyncState::load(&sync_dir(&b.mailspace.dir), PROJECT).unwrap();
    assert_eq!(state.since, 2);

    a.send_need("mind", "hand", "third");
    a.push(&ca);
    let second = b.pull(&cb);
    assert_eq!((second.fetched, second.cursor), (1, 3));
    assert_eq!(second.existing, 0, "only the new record was read");

    let empty = b.pull(&cb);
    assert_eq!(
        (empty.fetched, empty.cursor),
        (0, 3),
        "an empty read keeps the cursor"
    );
}

#[test]
fn a_long_backlog_is_read_page_after_page() {
    let server = FakeServer::new();
    let (a, b) = (
        checkout("alice", "mind", &["mind", "hand"], &[]),
        checkout("bob", "mind", &["mind"], &[]),
    );
    let (ca, cb) = (server.client("alice"), server.client("bob"));
    for subject in ["one", "two", "three", "four", "five"] {
        a.send_need("mind", "hand", subject);
    }
    a.push(&ca);
    server.page_limit(2);

    let report = b.pull(&cb);

    assert_eq!(report.fetched, 5);
    assert_eq!(report.filed.get("needs"), Some(&5));
    assert_eq!(report.cursor, 5);
}

#[test]
fn a_failed_read_leaves_the_cursor_where_it_was() {
    struct Down;
    impl Transport for Down {
        fn append(&self, _: &str, _: &Envelope<'_>) -> Result<Appended, AppendFailure> {
            Err(AppendFailure::Stop("down".into()))
        }
        fn fetch(&self, _: u64) -> Result<Page, VivariumError> {
            Err(VivariumError::Message("project server unreachable".into()))
        }
    }
    let fx = checkout("bob", "mind", &["mind"], &[]);

    let error = pull::pull(&fx.mailspace, &fx.settings, &Down).unwrap_err();

    assert!(error.to_string().contains("unreachable"));
    assert!(!sync_dir(&fx.mailspace.dir).join("state.json").exists());
}

#[test]
fn pull_requires_a_resolved_recipient() {
    let fx = checkout("bob", "mind", &["mind"], &[]);
    let mut settings = fx.settings.clone();
    settings.recipient = None;

    let canned = Canned { pages: Vec::new() };
    let error = pull::pull(&fx.mailspace, &settings, &canned).unwrap_err();

    assert!(error.to_string().contains("recipient"));
}

#[test]
fn a_row_bound_to_another_remote_is_left_alone_by_a_held_content_pull() {
    let fx = checkout("bob", "mind", &["mind"], &[]);
    let bytes = eml("room-item");
    let mut storage = fx.mailspace.storage().unwrap();
    storage
        .ingest_message(
            &MessageIngestRequest {
                account: "mind".into(),
                local_role: "inbox".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: Some(ID_A.into()),
                seed_hint: "room".into(),
                remote: Some(RemoteBindingInput {
                    account: "mind".into(),
                    provider: "vivi-room".into(),
                    remote_mailbox: "room-x".into(),
                    remote_uid: 4,
                    remote_uidvalidity: 1,
                }),
            },
            &bytes,
        )
        .unwrap();
    drop(storage);

    let report = pull_canned(&fx, vec![record(9, ID_B, "alice", Some("mail"), &bytes)]);

    assert_eq!(report.existing, 1);
    let row: StoredMessageView = fx.row(ID_A).unwrap();
    assert_eq!(
        row.remote.unwrap().provider,
        "vivi-room",
        "its binding is kept"
    );
}
