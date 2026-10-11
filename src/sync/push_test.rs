use crate::storage::{MessageIngestRequest, RemoteBindingInput};
use crate::sync::MAX_LETTER_BYTES;
use crate::sync::fake_server::{Checkout, FakeServer, PROJECT, checkout};
use crate::sync::push;
use crate::sync::select::SkipReason;
use crate::sync::transport::{Envelope, Transport};

fn alice() -> Checkout {
    checkout(
        "alice",
        "mind",
        &["mind", "hand", "peer"],
        &[("peer", "bob")],
    )
}

fn eml(subject: &str, total: Option<usize>) -> Vec<u8> {
    let head = format!(
        "From: mind@x.test\r\nTo: hand@x.test\r\nSubject: {subject}\r\n\
         Message-ID: <{subject}@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\n"
    );
    let pad = total.map_or(String::from("body\r\n"), |n| "x".repeat(n - head.len()));
    format!("{head}{pad}").into_bytes()
}

fn put(fx: &Checkout, account: &str, role: &str, id: Option<&str>, bytes: &[u8]) -> String {
    let stored = fx
        .mailspace
        .storage()
        .unwrap()
        .ingest_message(
            &MessageIngestRequest {
                account: account.into(),
                local_role: role.into(),
                read_state: false,
                starred: false,
                message_id_hint: id.map(String::from),
                presented_id: None,
                seed_hint: format!("test\0{role}\0{}", bytes.len()),
                remote: None,
            },
            bytes,
        )
        .unwrap();
    stored.message_id
}

fn reasons(report: &push::PushReport) -> Vec<SkipReason> {
    let mut reasons: Vec<SkipReason> = report.skipped.iter().map(|s| s.reason.clone()).collect();
    reasons.sort_by_key(SkipReason::label); // by label: size, draft, id, memo, trash
    reasons
}

#[test]
fn memos_drafts_trash_oversize_and_foreign_ids_stay_home_and_are_reported() {
    let server = FakeServer::new();
    let a = alice();
    let client = server.client("alice");
    a.mailspace
        .save_memo("mind", "a memo", "memo body")
        .unwrap();
    put(&a, "mind", "drafts", None, &eml("a-draft", None));
    put(&a, "mind", "trash", None, &eml("in-the-trash", None));
    put(&a, "mind", "inbox", None, &eml("too-big", Some(65_537)));
    put(&a, "mind", "inbox", Some("legacy-1"), &eml("old-id", None));
    let at_cap = put(&a, "mind", "inbox", None, &eml("at-the-cap", Some(65_536)));
    let need = a.send_need("mind", "hand", "real work");

    let report = a.push(&client);

    assert_eq!(report.sent, 2, "the need and the letter exactly at the cap");
    let sent: Vec<String> = server.items().into_iter().map(|i| i.message_id).collect();
    assert!(sent.contains(&at_cap));
    assert!(sent.contains(&a.id_of(&need)));
    assert_eq!(
        reasons(&report),
        vec![
            SkipReason::TooLarge(65_537),
            SkipReason::Draft,
            SkipReason::NotTravelingId("legacy-1".into()),
            SkipReason::Memo,
            SkipReason::Trash,
        ]
    );
    let lines = report.lines().join("\n");
    assert!(lines.contains("over the 64 KiB cap"), "{lines}");
    assert!(lines.contains("not a traveling id"), "{lines}");
    assert!(lines.contains("1 memo"), "{lines}");
    assert_eq!(MAX_LETTER_BYTES, 65_536);
    assert!(report.failure().is_none(), "policy skips are not failures");
    for row in a.rows() {
        let skipped = matches!(row.local_role.as_str(), "memos" | "drafts" | "trash")
            || row.message_id == "legacy-1"
            || row.byte_size > MAX_LETTER_BYTES;
        assert_eq!(
            row.remote.is_none(),
            skipped || row.local_role == "sent",
            "{row:?}"
        );
    }
}

#[test]
fn the_wire_id_is_the_delivered_row_else_the_sent_row() {
    let server = FakeServer::new();
    let a = alice();
    let client = server.client("alice");
    let need = a.send_need("mind", "hand", "delivered and sent");
    let delivered_id = a.id_of(&need);
    let sent_only = put(&a, "mind", "sent", None, &eml("only-sent", None));

    let report = a.push(&client);

    assert_eq!(report.sent, 2);
    let ids: Vec<String> = server.items().into_iter().map(|i| i.message_id).collect();
    assert!(ids.contains(&delivered_id), "not the sent copy's id");
    assert!(ids.contains(&sent_only));
    let rows = a.rows();
    let sent_copy = rows
        .iter()
        .find(|row| {
            row.local_role == "sent" && row.content_id == a.row(&delivered_id).unwrap().content_id
        })
        .unwrap();
    assert!(
        sent_copy.remote.is_none(),
        "only the row the id names is bound"
    );
}

#[test]
fn kind_and_recipient_follow_the_letter_and_the_names_map() {
    let server = FakeServer::new();
    let a = alice();
    let client = server.client("alice");
    a.send_need("mind", "peer", "to a mapped role");
    a.send_need("mind", "hand", "to an unmapped role");
    a.reply(
        &a.send_need("mind", "hand", "parent"),
        "hand",
        "peer",
        "mail to bob",
    );

    a.push(&client);

    let by_subject = |needle: &str| {
        server
            .items()
            .into_iter()
            .find(|item| String::from_utf8_lossy(&item.bytes).contains(needle))
            .unwrap()
    };
    let mapped = by_subject("Subject: to a mapped role");
    assert_eq!(mapped.to.as_deref(), Some("bob"));
    assert_eq!(mapped.kind.as_deref(), Some("need"));
    let unmapped = by_subject("Subject: to an unmapped role");
    assert_eq!(
        unmapped.to.as_deref(),
        Some("alice"),
        "the checkout's own name"
    );
    let mail = by_subject("mail to bob");
    assert_eq!(mail.to.as_deref(), Some("bob"));
    assert_eq!(mail.kind.as_deref(), Some("mail"));
}

#[test]
fn tasks_and_wants_travel_with_their_kind() {
    let server = FakeServer::new();
    let a = alice();
    let client = server.client("alice");
    for (role, kind) in [("tasks", "task"), ("wants", "want")] {
        a.mailspace
            .send(crate::mailspace::SendRequest {
                from: "mind".into(),
                to: vec!["hand".into()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: format!("a {kind}"),
                body: "b\n".into(),
                role: role.into(),
                kind: Some(kind.into()),
                reply_to: None,
                depends_on: Vec::new(),
            })
            .unwrap();
    }

    a.push(&client);

    let mut kinds: Vec<String> = server.items().into_iter().filter_map(|i| i.kind).collect();
    kinds.sort();
    assert_eq!(kinds, ["task", "want"]);
}

#[test]
fn a_rate_limit_stops_the_pass_and_leaves_the_rest_unbound() {
    let server = FakeServer::new();
    let a = alice();
    let client = server.client("alice");
    for subject in ["one", "two", "three", "four"] {
        a.send_need("mind", "hand", subject);
    }
    server.rate_limit_after(2);

    let report = a.push(&client);

    assert_eq!(report.sent, 2);
    let stopped = report.stopped.as_ref().expect("the pass stopped");
    assert_eq!(stopped.remaining, 2);
    assert!(report.failure().unwrap().contains("2 letter(s) unsent"));
    assert!(report.lines().join("\n").contains("left unsent"));
    let bound = a
        .rows()
        .iter()
        .filter(|row| row.local_role == "needs" && row.remote.is_some())
        .count();
    assert_eq!(bound, 2);
    assert_eq!(push::unsent(&a.mailspace, &a.settings).unwrap(), 2);

    server.lift_rate_limit();
    let next = a.push(&client);

    assert_eq!(next.sent, 2, "the remainder goes on the next run");
    assert!(next.stopped.is_none());
    assert_eq!(server.items().len(), 4);
}

#[test]
fn one_refused_letter_is_reported_and_the_rest_proceed() {
    let server = FakeServer::new();
    let a = alice();
    let client = server.client("alice");
    let handles: Vec<String> = ["one", "two", "three"]
        .iter()
        .map(|subject| a.send_need("mind", "hand", subject))
        .collect();
    let refused_id = a.id_of(&handles[1]);
    server.refuse(&refused_id, 409);

    let report = a.push(&client);

    assert_eq!(report.sent, 2);
    assert_eq!(report.refused.len(), 1);
    assert_eq!(report.refused[0].handle, handles[1]);
    assert!(report.refused[0].reason.contains("409"));
    assert!(report.stopped.is_none());
    assert!(report.failure().unwrap().contains("refused"));
    assert!(
        a.row(&refused_id).unwrap().remote.is_none(),
        "retried next run"
    );
    for handle in [&handles[0], &handles[2]] {
        assert!(a.row(&a.id_of(handle)).unwrap().remote.is_some());
    }
}

#[test]
fn bytes_the_server_already_holds_from_this_sender_are_bound_not_resent() {
    let server = FakeServer::new();
    let a = alice();
    let ca = server.client("alice");
    let handle = a.send_need("mind", "hand", "already there");
    // This sender pushed the identical letter earlier under another id.
    let envelope = Envelope {
        raw_eml: &a.bytes_of(&handle),
        to: "alice",
        kind: "need",
    };
    ca.append("msg_ffffffffffffffffffffffff", &envelope)
        .unwrap();

    let report = a.push(&ca);

    assert_eq!(report.sent, 0);
    assert_eq!(report.duplicates, 1);
    assert_eq!(server.items().len(), 1);
    let binding = a.row(&a.id_of(&handle)).unwrap().remote.unwrap();
    assert_eq!(binding.remote_uid, 1, "the original sequence");
    assert_eq!(binding.remote_mailbox, PROJECT);
}

#[test]
fn another_members_identical_bytes_are_an_ordinary_fresh_append() {
    let server = FakeServer::new();
    let a = alice();
    let (ca, cx) = (server.client("alice"), server.client("xena"));
    let handle = a.send_need("mind", "hand", "held by two members");
    let envelope = Envelope {
        raw_eml: &a.bytes_of(&handle),
        to: "xena",
        kind: "need",
    };
    cx.append("msg_ffffffffffffffffffffffff", &envelope)
        .unwrap();

    let report = a.push(&ca);

    assert_eq!(report.sent, 1);
    assert_eq!(report.duplicates, 0);
    assert_eq!(server.items().len(), 2);
}

#[test]
fn a_row_bound_to_another_remote_is_never_rebound() {
    let server = FakeServer::new();
    let a = alice();
    let client = server.client("alice");
    let id = {
        let bytes = eml("from-a-room", None);
        let stored = a
            .mailspace
            .storage()
            .unwrap()
            .ingest_message(
                &MessageIngestRequest {
                    account: "mind".into(),
                    local_role: "inbox".into(),
                    read_state: false,
                    starred: false,
                    message_id_hint: None,
                    presented_id: None,
                    seed_hint: "room".into(),
                    remote: Some(RemoteBindingInput {
                        account: "mind".into(),
                        provider: "vivi-room".into(),
                        remote_mailbox: "room-x".into(),
                        remote_uid: 3,
                        remote_uidvalidity: 1,
                    }),
                },
                &bytes,
            )
            .unwrap();
        stored.message_id
    };

    let report = a.push(&client);

    assert_eq!(report.sent, 0);
    assert!(server.items().is_empty());
    assert!(matches!(
        report.skipped[0].reason,
        SkipReason::BoundElsewhere(_)
    ));
    assert_eq!(a.row(&id).unwrap().remote.unwrap().provider, "vivi-room");
}

#[test]
fn a_corrupted_local_file_is_not_sent() {
    let server = FakeServer::new();
    let a = alice();
    let client = server.client("alice");
    let handle = a.send_need("mind", "hand", "bit rot");
    let row = a.row(&a.id_of(&handle)).unwrap();
    std::fs::write(a.mailspace.dir.join(&row.blob_relpath), b"rotted").unwrap();

    let report = a.push(&client);

    assert_eq!(report.sent, 0);
    assert_eq!(report.refused.len(), 1);
    assert!(report.refused[0].reason.contains("content id"));
    assert_eq!(server.append_calls(), 0);
}

#[test]
fn a_server_hash_that_differs_is_not_bound() {
    use crate::sync::transport::{AppendFailure, Appended, Page};
    struct Liar;
    impl Transport for Liar {
        fn append(&self, id: &str, _: &Envelope<'_>) -> Result<Appended, AppendFailure> {
            Ok(Appended {
                message_id: id.into(),
                sequence: 1,
                duplicate: false,
                content_sha256: Some("0".repeat(64)),
            })
        }
        fn fetch(&self, since: u64) -> Result<Page, crate::VivariumError> {
            Ok(Page {
                records: Vec::new(),
                next_cursor: since,
            })
        }
    }
    let a = alice();
    let handle = a.send_need("mind", "hand", "lied about");

    let report = push::push(&a.mailspace, &a.settings, &Liar).unwrap();

    assert_eq!(report.sent, 0);
    assert_eq!(report.unbound.len(), 1);
    assert!(report.unbound[0].reason.contains("content_sha256"));
    assert!(report.failure().is_some());
    assert!(a.row(&a.id_of(&handle)).unwrap().remote.is_none());
}

#[test]
fn an_ack_without_a_content_hash_is_not_bound() {
    use crate::sync::transport::{AppendFailure, Appended, Page};
    struct Silent;
    impl Transport for Silent {
        fn append(&self, id: &str, _: &Envelope<'_>) -> Result<Appended, AppendFailure> {
            Ok(Appended {
                message_id: id.into(),
                sequence: 1,
                duplicate: false,
                content_sha256: None,
            })
        }
        fn fetch(&self, since: u64) -> Result<Page, crate::VivariumError> {
            Ok(Page {
                records: Vec::new(),
                next_cursor: since,
            })
        }
    }
    let a = alice();
    a.send_need("mind", "hand", "unconfirmed");

    let report = push::push(&a.mailspace, &a.settings, &Silent).unwrap();

    assert_eq!(report.unbound.len(), 1);
    assert!(report.failure().is_some());
}

#[test]
fn a_sequence_that_does_not_fit_a_binding_is_reported() {
    use crate::sync::transport::{AppendFailure, Appended, Page};
    struct Huge;
    impl Transport for Huge {
        fn append(&self, id: &str, letter: &Envelope<'_>) -> Result<Appended, AppendFailure> {
            Ok(Appended {
                message_id: id.into(),
                sequence: u64::from(u32::MAX) + 1,
                duplicate: false,
                content_sha256: Some(crate::storage::sha256_hex(letter.raw_eml)),
            })
        }
        fn fetch(&self, since: u64) -> Result<Page, crate::VivariumError> {
            Ok(Page {
                records: Vec::new(),
                next_cursor: since,
            })
        }
    }
    let a = alice();
    a.send_need("mind", "hand", "far future");

    let report = push::push(&a.mailspace, &a.settings, &Huge).unwrap();

    assert_eq!(report.unbound.len(), 1);
    assert!(report.unbound[0].reason.contains("does not fit"));
}
