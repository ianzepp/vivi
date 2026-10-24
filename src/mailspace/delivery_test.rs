use std::collections::BTreeMap;

use chrono::Utc;

use crate::letter::fixture::{body_for, body_with_deps, need_body, task_body};
use crate::mailspace::memos::MemoConfig;
use crate::mailspace::{Mailspace, SendRequest};
use crate::storage::{MessageIngestRequest, StoredMessageView};
use crate::sync::SyncConfig;

const REMOTE: &str = "hand@3f9a1c7e.local";

fn space(synced: bool) -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity("hand").unwrap();
    if synced {
        mailspace.config.sync = Some(SyncConfig {
            server: Some("http://sync.test".into()),
            project: Some("abcde-fghjk-mnpqr-stvwz-23456".into()),
            name: Some("alice".into()),
            send: None,
        });
    }
    (tmp, mailspace)
}

fn request(to: &str, body: String) -> SendRequest {
    SendRequest {
        from: "mind".into(),
        to: vec![to.into()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "s".into(),
        body,
        role: "inbox".into(),
        kind: Some("mail".into()),
        reply_to: None,
    }
}

fn rows(mailspace: &Mailspace) -> Vec<StoredMessageView> {
    mailspace.storage().unwrap().list_messages().unwrap()
}

fn letter(from: &str, to: &str) -> String {
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: raw\r\nMessage-ID: <raw@audit.local>\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\nbody\r\n"
    )
}

#[test]
fn a_letter_over_the_cap_is_refused_at_send_when_sync_is_configured() {
    let (_tmp, mailspace) = space(true);
    let body = "x".repeat(70_000);

    for to in ["hand", REMOTE] {
        let error = mailspace.send(request(to, body.clone())).unwrap_err();
        let text = error.to_string();
        assert!(text.contains(" bytes, over the 65536-byte"), "{text}");
        assert!(text.contains("65536-byte (64 KiB) cap"), "{text}");
    }

    assert!(rows(&mailspace).is_empty(), "nothing is stored");
}

#[test]
fn a_letter_under_the_cap_is_stored_and_so_is_any_size_without_sync() {
    let (_tmp, synced) = space(true);
    synced.send(request("hand", "x".repeat(60_000))).unwrap();
    assert!(!rows(&synced).is_empty());

    let (_tmp, local) = space(false);
    local.send(request("hand", "x".repeat(70_000))).unwrap();
    assert!(!rows(&local).is_empty());
}

#[test]
fn a_reply_records_a_replied_event_on_the_parent_need() {
    let (_tmp, mailspace) = space(false);
    let sent = mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "need a thing".into(),
            body: need_body(&["it works"], ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap();
    let handle = sent.delivered[0].handle.clone();
    let parent_id = {
        let storage = mailspace.storage().unwrap();
        storage.resolve_message_token(&handle).unwrap()
    };

    mailspace
        .reply(
            &handle,
            "hand",
            Vec::new(),
            Vec::new(),
            None,
            "please add tests".into(),
        )
        .unwrap();

    let storage = mailspace.storage().unwrap();
    let events = storage
        .list_mailspace_events_for_messages(&[parent_id.clone()])
        .unwrap()
        .remove(&parent_id)
        .unwrap_or_default();
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "replied" && event.command == "mail reply"),
        "{events:?}"
    );
}

#[test]
fn a_task_reply_does_not_rekind_its_mail_parent() {
    let (_tmp, mailspace) = space(false);
    let sent = mailspace
        .send(request("hand", "please look".into()))
        .unwrap();
    let handle = sent.delivered[0].handle.clone();
    let parent_id = {
        let storage = mailspace.storage().unwrap();
        storage.resolve_message_token(&handle).unwrap()
    };

    mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Implement follow-up".into(),
            body: body_for("task", "Follow up on the review."),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: Some(handle),
        })
        .unwrap();

    let storage = mailspace.storage().unwrap();
    let events = storage
        .list_mailspace_events_for_messages(&[parent_id.clone()])
        .unwrap()
        .remove(&parent_id)
        .unwrap_or_default();
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "replied" && event.command == "mail reply"),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .filter(|event| event.event_type == "replied")
            .all(|event| event.command != "task send"),
        "{events:?}"
    );
}

#[test]
fn a_reply_over_the_cap_is_refused_too() {
    let (_tmp, mailspace) = space(true);
    let sent = mailspace.send(request("hand", "hi".into())).unwrap();
    let parent = sent.delivered[0].handle.clone();
    let before = rows(&mailspace).len();

    let error = mailspace
        .reply(
            &parent,
            "hand",
            Vec::new(),
            Vec::new(),
            None,
            "y".repeat(70_000),
        )
        .unwrap_err();

    assert!(error.to_string().contains("64 KiB"), "{error}");
    assert_eq!(rows(&mailspace).len(), before);
}

#[test]
fn the_queued_line_needs_sync_and_the_note_says_the_letter_stays() {
    let remote = [REMOTE.to_string()];
    let (_tmp, synced) = space(true);
    let (_tmp2, local) = space(false);

    assert_eq!(
        synced.remote_line(&remote).as_deref(),
        Some("queued for sync: hand@3f9a1c7e.local")
    );
    let note = local.remote_line(&remote).unwrap();
    assert!(!note.contains("queued for sync"), "{note}");
    assert!(note.contains("no [sync] is configured"), "{note}");
    assert!(note.contains("will not leave this checkout"), "{note}");
    assert_eq!(synced.remote_line(&[]), None);
    assert_eq!(local.remote_line(&[]), None);
}

#[test]
fn deliver_refuses_an_oversize_letter_only_when_sync_is_configured() {
    let big = format!(
        "{}{}",
        letter("mind@local", "hand@local"),
        "x".repeat(70_000)
    );
    let (_tmp, synced) = space(true);
    let (_tmp2, local) = space(false);

    let error = synced.deliver_raw(big.as_bytes(), "inbox").unwrap_err();

    assert!(error.to_string().contains("64 KiB"), "{error}");
    assert!(rows(&synced).is_empty());
    assert_eq!(
        local
            .deliver_raw(big.as_bytes(), "inbox")
            .unwrap()
            .delivered
            .len(),
        1
    );
}

#[test]
fn deliver_classifies_local_and_remote_recipients_like_send() {
    let (_tmp, mailspace) = space(true);
    let raw = format!(
        "{}\r\n",
        letter("mind@local", &format!("hand@local, {REMOTE}"))
    );

    let result = mailspace.deliver_raw(raw.as_bytes(), "inbox").unwrap();

    assert_eq!(result.remote, [REMOTE]);
    assert_eq!(result.sent, None);
    assert_eq!(result.delivered.len(), 1);
    assert_eq!(result.delivered[0].identity, "hand");
    let rows = rows(&mailspace);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let stored = mailspace
        .storage()
        .unwrap()
        .read_message(&rows[0].message_id)
        .unwrap();
    assert_eq!(stored, raw.as_bytes(), "kept byte for byte");
}

#[test]
fn deliver_to_remote_only_keeps_the_senders_copy_for_sync() {
    let (_tmp, mailspace) = space(true);
    let raw = letter("mind@local", REMOTE);

    let result = mailspace.deliver_raw(raw.as_bytes(), "inbox").unwrap();

    assert!(result.delivered.is_empty());
    assert_eq!(result.remote, [REMOTE]);
    let handle = result.sent.unwrap();
    let rows = rows(&mailspace);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].handle, handle);
    assert_eq!(rows[0].local_role, "sent");
    assert_eq!(rows[0].account, "mind");
}

#[test]
fn deliver_to_remote_only_needs_a_local_from_and_stores_nothing_otherwise() {
    let (_tmp, mailspace) = space(true);

    for from in ["stranger@example.com", "mind@3f9a1c7e.local"] {
        let raw = letter(from, REMOTE);
        let error = mailspace.deliver_raw(raw.as_bytes(), "inbox").unwrap_err();
        assert!(error.to_string().contains("needs a From"), "{error}");
    }
    let unknown = letter("mind@local", "ghost@local");
    let error = mailspace
        .deliver_raw(unknown.as_bytes(), "inbox")
        .unwrap_err();
    assert!(error.to_string().contains("unknown local role"), "{error}");
    assert!(rows(&mailspace).is_empty());
}

#[test]
fn deliver_keeps_an_attachment_letter_byte_for_byte() {
    let (_tmp, mailspace) = space(true);
    let raw = format!(
        "From: mind@{}.local\r\nTo: {REMOTE}\r\nSubject: att\r\nMessage-ID: <att@audit.local>\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\n\
         Content-Type: text/plain\r\n\r\nsee file\r\n--b\r\n\
         Content-Type: application/octet-stream\r\n\
         Content-Disposition: attachment; filename=\"a.bin\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\nAAECAwQF\r\n--b--\r\n",
        mailspace.address_prefix()
    );

    let result = mailspace.deliver_raw(raw.as_bytes(), "inbox").unwrap();

    let id = {
        let storage = mailspace.storage().unwrap();
        let handle = result.sent.unwrap();
        storage.resolve_message_token(&handle).unwrap()
    };
    let stored = mailspace.storage().unwrap().read_message(&id).unwrap();
    assert_eq!(stored, raw.as_bytes());
}

#[test]
fn a_close_with_verdict_carries_it_as_front_matter() {
    let (_tmp, mailspace) = space(false);
    let mut sent = mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "unit".into(),
            body: task_body("lands"),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap();
    let handle = sent.delivered.remove(0).handle;

    let outcome = mailspace
        .move_task_outcome(
            "mind",
            &handle,
            "done",
            None,
            Some("accepted"),
            &["vivi".into()],
            &["abc123".into()],
        )
        .unwrap();

    assert!(outcome.event.is_some(), "{outcome:?}");
    let storage = mailspace.storage().unwrap();
    let event = outcome.event.unwrap();
    let id = storage.resolve_message_token(&event).unwrap();
    let letter = String::from_utf8_lossy(&storage.read_message(&id).unwrap()).into_owned();
    assert!(letter.contains("X-Vivi-Format: 1"), "{letter}");
    for header in [
        "X-Vivi-Verdict",
        "X-Vivi-Repo",
        "X-Vivi-Branch",
        "X-Vivi-Commit",
    ] {
        assert!(
            !letter.contains(&format!("{header}:")),
            "close metadata stayed a header: {letter}"
        );
    }
    let parsed = crate::letter::of_message(letter.as_bytes(), &event).unwrap();
    assert_eq!(parsed.kind, crate::letter::LetterKind::Event("done".into()));
    let crate::letter::Front::Event(front) = parsed.front else {
        panic!("not a close front: {letter}");
    };
    assert_eq!(front.verdict.as_deref(), Some("accepted"));
    assert_eq!(front.repos.get("vivi").map(String::as_str), Some("abc123"));
}

#[test]
fn a_close_without_metadata_stores_its_note_with_no_fence() {
    let (_tmp, mailspace) = space(false);
    let mut sent = mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "unit".into(),
            body: task_body("lands"),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap();
    let handle = sent.delivered.remove(0).handle;

    let outcome = mailspace
        .move_task_outcome("mind", &handle, "done", Some("all landed"), None, &[], &[])
        .unwrap();

    let storage = mailspace.storage().unwrap();
    let id = storage
        .resolve_message_token(outcome.event.as_deref().unwrap())
        .unwrap();
    let letter = String::from_utf8_lossy(&storage.read_message(&id).unwrap()).into_owned();
    assert!(letter.contains("X-Vivi-Format: 1"), "{letter}");
    assert!(
        !letter.contains("+++"),
        "an empty front shows no fence: {letter}"
    );
    let parsed = crate::letter::of_message(letter.as_bytes(), &handle).unwrap();
    let crate::letter::Front::Event(front) = &parsed.front else {
        panic!("not a close front: {letter}");
    };
    assert_eq!(front, &crate::letter::plain::CloseFront::default());
    assert_eq!(parsed.prose, "all landed");
}

#[test]
fn a_report_with_verdict_carries_it_as_a_letter() {
    let (_tmp, mailspace) = space(false);
    let mut sent = mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "unit".into(),
            body: task_body("lands"),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap();
    let handle = sent.delivered.remove(0).handle;

    let outcome = mailspace
        .move_task_outcome(
            "hand",
            &handle,
            "done",
            None,
            Some("fixed"),
            &["vivi".into()],
            &["sha1".into()],
        )
        .unwrap();

    assert!(outcome.reported, "{outcome:?}");
    assert!(outcome.event.is_none(), "{outcome:?}");
    let thread = mailspace.thread(&handle, false, 10, 10).unwrap();
    assert_eq!(thread.len(), 2, "the report is a letter on the thread");
    let storage = mailspace.storage().unwrap();
    let reply = thread
        .iter()
        .find(|message| message.message_id != storage.resolve_message_token(&handle).unwrap())
        .unwrap();
    let letter =
        String::from_utf8_lossy(&storage.read_message(&reply.message_id).unwrap()).into_owned();
    assert!(letter.contains("X-Vivi-Format: 1"), "{letter}");
    for header in [
        "X-Vivi-Verdict",
        "X-Vivi-Repo",
        "X-Vivi-Branch",
        "X-Vivi-Commit",
    ] {
        assert!(
            !letter.contains(&format!("{header}:")),
            "close metadata stayed a header: {letter}"
        );
    }
    let parsed = crate::letter::of_message(letter.as_bytes(), &handle).unwrap();
    assert_eq!(parsed.kind, crate::letter::LetterKind::Mail);
    let crate::letter::Front::Mail(front) = parsed.front else {
        panic!("not a mail front: {letter}");
    };
    let close = front
        .close
        .as_ref()
        .unwrap_or_else(|| panic!("no close front: {letter}"));
    assert_eq!(close.verdict.as_deref(), Some("fixed"));
    assert_eq!(close.repos.get("vivi").map(String::as_str), Some("sha1"));
    assert_eq!(parsed.prose, "done");
}

#[test]
fn deliver_raw_mints_the_backlog_node_and_its_edges() {
    let (_tmp, mailspace) = space(false);
    let domain = mailspace.address_domain();
    let work = |subject: &str, kind: &str, deps: &[&str]| {
        let headers = format!(
            "From: mind@{domain}\r\nTo: hand@{domain}\r\nSubject: {subject}\r\n\
             Message-ID: <{subject}@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\
             X-Vivi-Kind: {kind}\r\nX-Vivi-Format: 1\r\n"
        );
        let deps: Vec<String> = deps.iter().map(ToString::to_string).collect();
        format!("{headers}\r\n{}", body_with_deps(kind, subject, &deps))
    };
    let node = |item: &str| {
        mailspace
            .graph_show("backlog")
            .unwrap()
            .nodes
            .into_iter()
            .find(|node| node.source_id == item)
            .unwrap_or_else(|| panic!("no node for {item}"))
    };
    let nodes = || mailspace.graph_show("backlog").unwrap().nodes.len();

    let first_bytes = work("first", "need", &[]);
    mailspace
        .deliver_raw(first_bytes.as_bytes(), "needs")
        .unwrap();
    let first = mailspace
        .list_kind(Some("hand"), "needs", "need")
        .unwrap()
        .remove(0)
        .handle;
    let minted = node(&first);
    assert_eq!(minted.state, "open");
    assert_eq!(minted.readiness, "ready");

    mailspace
        .deliver_raw(work("second", "task", &[&first]).as_bytes(), "tasks")
        .unwrap();
    let second = mailspace
        .list_kind(Some("hand"), "tasks", "task")
        .unwrap()
        .remove(0)
        .handle;
    let show = mailspace.graph_show("backlog").unwrap();
    assert_eq!(
        show.nodes
            .iter()
            .find(|node| node.source_id == second)
            .expect("the dependent node")
            .readiness,
        "blocked"
    );
    assert_eq!(show.edges.len(), 1, "{:?}", show.edges);
    // Ingest minted both nodes and the edge: the audit finds no drift to
    // repair. (It runs before the no-mint deliveries below because the audit
    // treats any letter in a work folder as work, and the synthetic event
    // letter files under `tasks`.)
    let findings = mailspace.backlog_audit(false).unwrap().findings;
    assert!(findings.is_empty(), "{findings:?}");

    let before = nodes();
    mailspace
        .deliver_raw(letter("mind@local", "hand@local").as_bytes(), "inbox")
        .unwrap();
    assert_eq!(nodes(), before, "plain mail mints no node");
    let event = format!(
        "From: mind@{domain}\r\nTo: hand@{domain}\r\nSubject: done\r\n\
         Message-ID: <event@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\
         X-Vivi-Event: done\r\nX-Vivi-Kind: task\r\nX-Vivi-Format: 1\r\n\r\n{}",
        task_body("third")
    );
    mailspace.deliver_raw(event.as_bytes(), "tasks").unwrap();
    assert_eq!(nodes(), before, "an event letter mints no node");
}

#[test]
fn retention_prunes_expired_memos_when_a_tagged_memo_is_saved() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("digest").unwrap();
    mailspace.config.memo = Some(MemoConfig {
        hidden_tags: Vec::new(),
        retain: BTreeMap::from([("digest".to_string(), "90d".to_string())]),
    });

    let address = mailspace.address_for("digest");
    let old_date = (Utc::now() - chrono::Duration::days(100)).to_rfc2822();
    let raw = format!(
        "From: {address}\r\nTo: {address}\r\nDate: {old_date}\r\n\
         Subject: old digest\r\nX-Vivi-Kind: memo\r\nX-Vivi-Tag: digest\r\n\r\nold body\r\n"
    );
    mailspace
        .storage()
        .unwrap()
        .ingest_message(
            &MessageIngestRequest {
                account: "digest".into(),
                local_role: "memos".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: "old-digest".into(),
                remote: None,
            },
            raw.as_bytes(),
        )
        .unwrap();

    let handle = mailspace
        .save_memo(
            "digest",
            "new digest",
            "fresh body",
            &["digest".to_string()],
        )
        .unwrap();
    let remaining = mailspace
        .list_kind(Some("digest"), "memos", "memo")
        .unwrap();
    assert_eq!(remaining.len(), 1, "expired tagged memo should be pruned");
    assert_eq!(remaining[0].handle, handle);
}

#[test]
fn memo_save_stores_tag_headers() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("hand").unwrap();

    let error = mailspace
        .save_memo(
            "hand",
            "s",
            "b",
            &["digest".to_string(), "ops note".to_string()],
        )
        .unwrap_err();
    assert!(error.to_string().contains("ops note"), "{error}");

    let handle = mailspace
        .save_memo("hand", "s", "b", &["digest".to_string(), "ops".to_string()])
        .unwrap();
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(&handle).unwrap();
    let memo = String::from_utf8_lossy(&storage.read_message(&id).unwrap()).into_owned();
    assert!(
        memo.contains("X-Vivi-Tag: digest"),
        "no digest tag header: {memo}"
    );
    assert!(
        memo.contains("X-Vivi-Tag: ops"),
        "no ops tag header: {memo}"
    );
}
