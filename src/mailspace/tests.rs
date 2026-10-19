use super::*;
use crate::storage::MessageIngestRequest;
use std::collections::BTreeSet;
use std::time::Instant;

#[test]
fn detects_mailspace_from_subdirectory() {
    let tmp = tempfile::tempdir().unwrap();
    Mailspace::init(Some(tmp.path())).unwrap();
    let deep = tmp.path().join("src/deep");
    fs::create_dir_all(&deep).unwrap();
    let old = std::env::current_dir().unwrap();
    std::env::set_current_dir(&deep).unwrap();
    let found = Mailspace::discover(None).unwrap();
    std::env::set_current_dir(old).unwrap();

    assert_eq!(found.root, tmp.path().canonicalize().unwrap());
    assert!(!deep.join(".vivi").exists());
}

#[test]
fn local_delivery_rejects_unknown_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();

    let err = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "hello".into(),
            body: "body".into(),
            role: "inbox".into(),
            kind: None,
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap_err();

    assert!(err.to_string().contains("unknown local role"));
}

#[test]
fn absorb_mail_marks_message_read() {
    // Absorb means "read, processed, loaded into context", so it must mark the
    // message read. Otherwise absorbed mail inflates `inbox_unread`, which
    // boards and sensors (and Minds) read on as a neglect signal.
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    let unread = |ms: &Mailspace| {
        ms.status()
            .unwrap()
            .identities
            .iter()
            .find(|identity| identity.identity == "cto")
            .unwrap()
            .inbox_unread
    };

    let sent = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "absorb me".into(),
            body: "read, processed, loaded into context".into(),
            role: "inbox".into(),
            kind: None,
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let handle = sent.delivered[0].handle.clone();

    assert_eq!(unread(&mailspace), 1, "delivered inbox mail starts unread");
    let absorbed = mailspace.absorb_mail("cto", &handle, None).unwrap();
    assert_eq!(absorbed, handle);
    assert_eq!(
        unread(&mailspace),
        0,
        "absorbed mail must no longer count as unread"
    );
}

#[test]
fn absorb_rejects_mutation_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    let sent = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "task".into(),
            body: "do it".into(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let handle = sent.delivered[0].handle.clone();
    let first = mailspace.absorb("cto", &handle, None, "task").unwrap();
    let second = mailspace.absorb("cto", &handle, None, "task").unwrap();
    assert_eq!(first, handle);
    assert_eq!(second, handle);
    let err = mailspace
        .move_item("cto", &handle, "done", None, "task done", None)
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("is absorbed and can no longer be changed"),
        "{err}"
    );
}

#[test]
fn set_archive_requires_git_repo_and_absorb_writes_once() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();

    let missing = tmp.path().join("missing-archive");
    let err = mailspace
        .set_archive(Some(missing.to_string_lossy().into_owned()))
        .unwrap_err();
    assert!(err.to_string().contains("does not exist"), "{err}");

    let not_git = tmp.path().join("not-git");
    fs::create_dir_all(&not_git).unwrap();
    let err = mailspace
        .set_archive(Some(not_git.to_string_lossy().into_owned()))
        .unwrap_err();
    assert!(
        err.to_string().contains("must be a git repository"),
        "{err}"
    );

    let archive = tmp.path().join("vivi-archive");
    fs::create_dir_all(archive.join(".git")).unwrap();
    mailspace
        .set_archive(Some(archive.to_string_lossy().into_owned()))
        .unwrap();

    let sent = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "archive me".into(),
            body: "seal this task".into(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let handle = sent.delivered[0].handle.clone();
    mailspace
        .absorb("cto", &handle, Some("done"), "task")
        .unwrap();

    let storage = mailspace.storage().unwrap();
    let resolved = storage.resolve_message_token(&handle).unwrap();
    let path =
        super::archive::archive_file_path(&archive, &mailspace.config.name, "task", &resolved);
    let rendered = fs::read_to_string(&path).unwrap();
    assert!(rendered.starts_with("+++"), "{rendered}");
    assert!(rendered.contains("kind = \"task\""), "{rendered}");
    assert!(rendered.contains("seal this task"), "{rendered}");
    assert!(rendered.contains("command = \"task absorb\""), "{rendered}");

    let first = fs::read_to_string(&path).unwrap();
    mailspace.absorb("cto", &handle, None, "task").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), first);

    fs::write(&path, "stale").unwrap();
    mailspace.absorb("cto", &handle, None, "task").unwrap();
    let restored = fs::read_to_string(&path).unwrap();
    assert_eq!(restored, first);
    assert!(!restored.contains("stale"), "{restored}");
}

#[test]
fn export_archive_writes_prior_absorbs_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    let sent = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "later".into(),
            body: "already sealed".into(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let handle = sent.delivered[0].handle.clone();
    mailspace.absorb("cto", &handle, None, "task").unwrap();

    let err = mailspace.export_archive().unwrap_err();
    assert!(err.to_string().contains("no archive configured"), "{err}");

    let archive = tmp.path().join("vivi-archive");
    fs::create_dir_all(archive.join(".git")).unwrap();
    mailspace
        .set_archive(Some(archive.to_string_lossy().into_owned()))
        .unwrap();

    let first = mailspace.export_archive().unwrap();
    assert_eq!(first.scanned, 1);
    assert_eq!(first.written, 1);
    assert_eq!(first.unchanged, 0);

    let storage = mailspace.storage().unwrap();
    let resolved = storage.resolve_message_token(&handle).unwrap();
    let path =
        super::archive::archive_file_path(&archive, &mailspace.config.name, "task", &resolved);
    let rendered = fs::read_to_string(&path).unwrap();
    assert!(rendered.contains("already sealed"), "{rendered}");

    let second = mailspace.export_archive().unwrap();
    assert_eq!(second.scanned, 1);
    assert_eq!(second.written, 0);
    assert_eq!(second.unchanged, 1);
    assert_eq!(fs::read_to_string(&path).unwrap(), rendered);
}

#[test]
fn export_archive_includes_done_work_items_but_not_done_mail() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    let send = |role: &str, kind: Option<&str>, subject: &str, body: &str| {
        mailspace
            .send(SendRequest {
                from: "ceo".into(),
                to: vec!["cto".into()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: subject.into(),
                body: body.into(),
                role: role.into(),
                kind: kind.map(str::to_owned),
                reply_to: None,
                depends_on: Vec::new(),
            })
            .unwrap()
            .delivered[0]
            .handle
            .clone()
    };

    let task = send(
        "tasks",
        Some("task"),
        "completed task",
        "preserve this task history",
    );
    mailspace
        .move_task("cto", &task, "done", None, Some("clean_pass"), &[], &[])
        .unwrap();
    let mail = send(
        "inbox",
        None,
        "ordinary done mail",
        "do not classify this as work history",
    );
    mailspace
        .move_item("cto", &mail, "done", None, "item move", None)
        .unwrap();

    let archive = tmp.path().join("vivi-archive");
    fs::create_dir_all(archive.join(".git")).unwrap();
    mailspace
        .set_archive(Some(archive.to_string_lossy().into_owned()))
        .unwrap();
    let report = mailspace.export_archive().unwrap();
    assert_eq!(
        (report.scanned, report.written, report.unchanged),
        (1, 1, 0)
    );

    let storage = mailspace.storage().unwrap();
    let resolved = storage.resolve_message_token(&task).unwrap();
    let path =
        super::archive::archive_file_path(&archive, &mailspace.config.name, "task", &resolved);
    let rendered = fs::read_to_string(path).unwrap();
    assert!(rendered.contains("role = \"done\""), "{rendered}");
    assert!(
        rendered.contains("preserve this task history"),
        "{rendered}"
    );
    assert!(!archive.join(&mailspace.config.name).join("mail").exists());
}

#[test]
fn task_move_keeps_handle_stable() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    let sent = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "task".into(),
            body: "do it".into(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let handle = sent.delivered[0].handle.clone();

    let done = mailspace
        .move_task("cto", &handle, "done", None, None, &[], &[])
        .unwrap();
    let open = mailspace.list("cto", "tasks").unwrap();
    let closed = mailspace.list("cto", "done").unwrap();

    assert_eq!(done, handle);
    assert!(open.is_empty());
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].handle, handle);
}

#[test]
fn task_done_records_verdict_and_tips() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    let sent = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "task".into(),
            body: "do it".into(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let handle = sent.delivered[0].handle.clone();

    let repos = vec!["examples".into(), "hosts".into()];
    let tips = vec!["e968cc3".into(), "0de5c36".into()];
    mailspace
        .move_task(
            "cto",
            &handle,
            "done",
            Some("P2: minor"),
            Some("residual"),
            &repos,
            &tips,
        )
        .unwrap();

    let storage = mailspace.storage().unwrap();
    let resolved = storage.resolve_message_token(&handle).unwrap();
    let metadata = storage.item_metadata(&resolved).unwrap();
    assert_eq!(metadata.get("verdict"), Some(&"residual".into()));
    assert_eq!(metadata.get("repo:examples"), Some(&"e968cc3".into()));
    assert_eq!(metadata.get("repo:hosts"), Some(&"0de5c36".into()));
}

#[test]
fn lifecycle_note_on_self_addressed_item_leaves_no_inbox_twin() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    let sent = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["ceo".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "parked idea".into(),
            body: "someday".into(),
            role: "wants".into(),
            kind: Some("want".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let handle = sent.delivered[0].handle.clone();

    mailspace
        .move_item(
            "ceo",
            &handle,
            "done",
            Some("closed for now"),
            "want done",
            None,
        )
        .unwrap();

    // The actor already knows their own note: no unread inbox twin appears;
    // the sent copy keeps the thread record.
    let inbox = mailspace.list("ceo", "inbox").unwrap();
    assert!(inbox.is_empty(), "{inbox:?}");
    let sent_records = mailspace.list("ceo", "sent").unwrap();
    assert_eq!(sent_records.len(), 2, "{sent_records:?}");
    assert!(
        sent_records
            .iter()
            .any(|record| record.subject.starts_with("Re: parked idea")),
        "{sent_records:?}"
    );
}

#[test]
fn lifecycle_note_delivers_receipt_to_parent_sender() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    let sent = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "task".into(),
            body: "do it".into(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let handle = sent.delivered[0].handle.clone();

    mailspace
        .move_task("cto", &handle, "done", Some("shipped"), None, &[], &[])
        .unwrap();

    let ceo_inbox = mailspace.list("ceo", "inbox").unwrap();
    assert_eq!(ceo_inbox.len(), 1, "{ceo_inbox:?}");
    assert!(ceo_inbox[0].subject.starts_with("Re:"), "{ceo_inbox:?}");
    let cto_inbox = mailspace.list("cto", "inbox").unwrap();
    assert!(cto_inbox.is_empty(), "{cto_inbox:?}");
}

#[test]
fn rename_identity_keeps_historical_mail_and_old_alias_working() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    let sent = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "task".into(),
            body: "do it".into(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let handle = sent.delivered[0].handle.clone();

    let new_address = mailspace.rename_identity("cto", "cro").unwrap();
    assert_eq!(new_address, mailspace.address_for("cro"));
    assert_eq!(mailspace.config.identities.len(), 2);
    let renamed = mailspace
        .config
        .identities
        .iter()
        .find(|identity| identity.name == "cro")
        .unwrap();
    assert_eq!(renamed.aliases, vec!["cto".to_string()]);

    // Historical mail (stored under the old identity name) is untouched
    // but still shows up when listing under the new name.
    let open = mailspace.list("cro", "tasks").unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].account, "cto");

    // The old name still resolves for future commands.
    assert_eq!(mailspace.resolve_identity("cto").unwrap(), "cro");

    let done = mailspace
        .move_task("cro", &handle, "done", None, None, &[], &[])
        .unwrap();
    assert_eq!(done, handle);
    assert!(mailspace.list("cro", "tasks").unwrap().is_empty());
    assert_eq!(mailspace.list("cro", "done").unwrap().len(), 1);

    let status = mailspace.status().unwrap();
    let cro_status = status
        .identities
        .iter()
        .find(|identity| identity.identity == "cro")
        .unwrap();
    assert_eq!(cro_status.done, 1);
}

#[test]
fn rename_identity_rejects_unknown_or_colliding_names() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();

    let err = mailspace.rename_identity("missing", "vp").unwrap_err();
    assert!(err.to_string().contains("unknown local role"));

    let err = mailspace.rename_identity("ceo", "cto").unwrap_err();
    assert!(err.to_string().contains("already exists"));
}

#[test]
fn add_role_applies_all_fields_and_rejects_duplicate_name() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();

    let update = RoleUpdate {
        kind: Some(Some("hand".into())),
        status: Some("active".into()),
        harness: Some(Some("subagent".into())),
        provider: Some(Some("zai".into())),
        add_labels: vec!["floater".into()],
        ..RoleUpdate::default()
    };
    let address = mailspace.add_role("hand-1", update).unwrap();
    assert!(address.contains("hand-1@"), "{address}");

    let view = mailspace.role_view("hand-1").unwrap();
    assert_eq!(view.kind.as_deref(), Some("hand"));
    assert_eq!(view.harness.as_deref(), Some("subagent"));
    assert_eq!(view.provider.as_deref(), Some("zai"));
    assert!(view.labels.iter().any(|label| label == "floater"));

    // A duplicate add must error, not silently no-op.
    let err = mailspace
        .add_role("hand-1", RoleUpdate::default())
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("already exists"), "{msg}");
    assert!(msg.contains("role set"), "{msg}");

    // The rejected add must not have mutated the original role.
    let unchanged = mailspace.role_view("hand-1").unwrap();
    assert_eq!(unchanged.kind.as_deref(), Some("hand"));
}

#[test]
fn explicit_thread_skips_unrelated_messages() {
    let (_tmp, mailspace, handle) = fixture_with_noise(80);
    let messages = mailspace.thread(&handle, false, 50, 50).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind.as_deref(), Some("need"));
    assert_eq!(messages[0].subject, "target need");
    assert!(messages[0].body.contains("need body"));
}

#[test]
fn explicit_thread_stays_cheap_with_many_unrelated_messages() {
    let (_tmp, mailspace, handle) = fixture_with_noise(200);
    // Warm SQLite/page cache so the sample measures path cost, not cold open.
    let _ = mailspace.thread(&handle, false, 50, 50).unwrap();
    let optimized = sample_ms(|| {
        let messages = mailspace.thread(&handle, false, 50, 50).unwrap();
        assert_eq!(messages.len(), 1);
    });
    // Full-candidate scan still used by inference; prove it is the old bottleneck.
    let full_scan = sample_ms(|| {
        let messages = mailspace.thread(&handle, true, 50, 50).unwrap();
        assert_eq!(messages.len(), 1);
    });
    assert!(
        optimized < full_scan / 3.0 || optimized < 40.0,
        "optimized={optimized:.1}ms full_scan={full_scan:.1}ms"
    );
    assert!(
        optimized < full_scan,
        "optimized={optimized:.1}ms should beat full_scan={full_scan:.1}ms"
    );
}

fn fixture_with_noise(noise: usize) -> (tempfile::TempDir, Mailspace, String) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("alice").unwrap();
    mailspace.add_identity("bob").unwrap();
    for i in 0..noise {
        mailspace
            .send(SendRequest {
                from: "alice".into(),
                to: vec!["bob".into()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: format!("noise {i}"),
                body: format!("noise body {i}"),
                role: "inbox".into(),
                kind: None,
                reply_to: None,
                depends_on: Vec::new(),
            })
            .unwrap();
    }
    let sent = mailspace
        .send(SendRequest {
            from: "alice".into(),
            to: vec!["bob".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "target need".into(),
            body: "need body to show".into(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    (tmp, mailspace, sent.delivered[0].handle.clone())
}

fn sample_ms(mut op: impl FnMut()) -> f64 {
    let mut total = 0.0;
    for _ in 0..3 {
        let start = Instant::now();
        op();
        total += start.elapsed().as_secs_f64() * 1000.0;
    }
    total / 3.0
}

#[test]
fn deliver_raw_rejects_unresolved_recipients() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();

    // External address — resolve_identity rejects before delivery.
    let eml = b"From: stranger@example.com\r\nTo: nobody@example.com\r\nSubject: hi\r\n\r\nbody";
    let err = mailspace.deliver_raw(eml, "inbox").unwrap_err();
    assert!(err.to_string().contains("not allowed"));
}

#[test]
fn deliver_raw_multi_recipient_is_atomic_on_success() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("alice").unwrap();
    mailspace.add_identity("bob").unwrap();
    mailspace.add_identity("carol").unwrap();

    let to = mailspace.address_for("alice");
    let cc1 = mailspace.address_for("bob");
    let cc2 = mailspace.address_for("carol");
    let eml = format!(
        "From: sender@example.com\r\n\
To: {to}\r\n\
Cc: {cc1}, {cc2}\r\n\
Subject: multi\r\n\r\nbody"
    );
    let delivered = mailspace
        .deliver_raw(eml.as_bytes(), "inbox")
        .unwrap()
        .delivered;
    let delivered_names: BTreeSet<String> = delivered.iter().map(|d| d.identity.clone()).collect();
    assert_eq!(
        delivered_names,
        ["alice", "bob", "carol"]
            .into_iter()
            .map(String::from)
            .collect()
    );

    // Every recipient has a message row and at least one delivery event.
    for identity in &["alice", "bob", "carol"] {
        let inbox = mailspace.list(identity, "inbox").unwrap();
        assert_eq!(inbox.len(), 1, "{identity} should have one inbox message");
    }

    // Verify events were logged atomically alongside message rows.
    let storage = mailspace.storage().unwrap();
    let events = storage.list_mailspace_events_after(0).unwrap();
    let delivered_events: Vec<_> = events
        .iter()
        .filter(|e| e.command == "mail deliver")
        .collect();
    assert_eq!(
        delivered_events.len(),
        3,
        "all three recipients should have delivery events"
    );
}

#[test]
fn deliver_raw_batch_rollback_leaves_no_partial_state() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("alice").unwrap();
    mailspace.add_identity("bob").unwrap();
    mailspace.add_identity("carol").unwrap();

    let eml = b"From: sender@example.com\r\n\
To: alice@example.com\r\n\
Cc: bob@example.com, carol@example.com\r\n\
Subject: rollback\r\n\r\nbody";

    let recipients = ["alice", "bob", "carol"];
    let requests: Vec<_> = recipients
        .iter()
        .map(|r| MessageIngestRequest {
            account: r.to_string(),
            local_role: "inbox".into(),
            read_state: false,
            starred: false,
            message_id_hint: None,
            presented_id: None,
            seed_hint: format!("raw-delivery\0{r}\0{}", eml.len()),
            remote: None,
        })
        .collect();

    let storage = mailspace.storage().unwrap();

    // Inject failure after 2 message rows — proving the entire batch rolls back.
    let err = storage
        .deliver_raw_batch_fail_after(
            &requests,
            eml,
            "mail deliver",
            "delivered",
            "inbox",
            "rollback",
            2,
        )
        .unwrap_err();
    assert!(err.to_string().contains("injected batch failure"));

    // No partial message rows for ANY recipient (not even the first two).
    let all_messages = storage.list_messages_by_role("inbox").unwrap();
    assert!(
        all_messages.is_empty(),
        "no message rows should survive rollback, got {}",
        all_messages.len()
    );

    // No partial delivery events for ANY recipient.
    let events = storage.list_mailspace_events_after(0).unwrap();
    assert!(
        events.is_empty(),
        "no events should survive rollback, got {}",
        events.len()
    );
}

#[test]
fn deliver_raw_batch_success_then_list_has_events() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("alice").unwrap();
    mailspace.add_identity("bob").unwrap();

    let eml = b"From: sender@example.com\r\n\
To: alice@example.com\r\n\
Cc: bob@example.com\r\n\
Subject: batch\r\n\r\nbody";

    let recipients = ["alice", "bob"];
    let requests: Vec<_> = recipients
        .iter()
        .map(|r| MessageIngestRequest {
            account: r.to_string(),
            local_role: "inbox".into(),
            read_state: false,
            starred: false,
            message_id_hint: None,
            presented_id: None,
            seed_hint: format!("raw-delivery\0{r}\0{}", eml.len()),
            remote: None,
        })
        .collect();

    let storage = mailspace.storage().unwrap();
    let stored = storage
        .deliver_raw_batch(
            &requests,
            eml,
            "mail deliver",
            "delivered",
            "inbox",
            "batch",
        )
        .unwrap();
    assert_eq!(stored.len(), 2);

    // Messages present for both recipients.
    let messages = storage.list_messages_by_role("inbox").unwrap();
    assert_eq!(messages.len(), 2);

    // Events present for both recipients.
    let events = storage.list_mailspace_events_after(0).unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e.command == "mail deliver"));
}

#[test]
fn legacy_identity_toml_loads_as_role_with_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    Mailspace::init(Some(root)).unwrap();
    std::fs::write(
        root.join(".vivi/mailspace.toml"),
        "name = \"demo\"\n\n[[identities]]\nname = \"hand-1\"\naliases = []\n",
    )
    .unwrap();
    let mut mailspace = Mailspace::discover(Some(root)).unwrap();
    let view = mailspace.role_view("hand-1").unwrap();
    assert_eq!(view.name, "hand-1");
    assert_eq!(view.status, "active");
    assert!(view.kind.is_none());
    assert!(view.provider.is_none());
    assert!(!view.has_charter);

    mailspace
        .set_role(
            "hand-1",
            RoleUpdate {
                kind: Some(Some("hand".into())),
                provider: Some(Some("zai".into())),
                model: Some(Some("glm-5.2".into())),
                thinking: Some(Some("low".into())),
                harness: Some(Some("subagent".into())),
                ..Default::default()
            },
        )
        .unwrap();
    mailspace
        .set_charter("hand-1", "Implement packages; report to mind.\n")
        .unwrap();
    let view = mailspace.role_view("hand-1").unwrap();
    assert_eq!(view.kind.as_deref(), Some("hand"));
    assert_eq!(view.provider.as_deref(), Some("zai"));
    assert_eq!(view.model.as_deref(), Some("glm-5.2"));
    assert_eq!(view.thinking.as_deref(), Some("low"));
    assert_eq!(view.harness.as_deref(), Some("subagent"));
    assert!(view.charter.contains("report to mind"));
}

#[test]
fn schedule_report_skips_outbound_query_without_cadence() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("hand").unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace
        .send(SendRequest {
            from: "hand".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "work".into(),
            body: "body".into(),
            role: "inbox".into(),
            kind: None,
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let report = mailspace.schedule_report("hand").unwrap();
    assert_eq!(report.state, crate::role_schedule::ScheduleState::None);
    assert!(report.last_signal_handle.is_none());
}

#[test]
fn schedule_report_uses_latest_non_memo_when_cadence_set() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace
        .add_role(
            "hand",
            RoleUpdate {
                cadence: Some(Some("15m".into())),
                ..Default::default()
            },
        )
        .unwrap();
    mailspace
        .send(SendRequest {
            from: "hand".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "cycle".into(),
            body: "done".into(),
            role: "inbox".into(),
            kind: None,
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    mailspace
        .save_memo("hand", "private note", "not a signal", &[])
        .unwrap();
    let report = mailspace.schedule_report("hand").unwrap();
    assert_eq!(report.state, crate::role_schedule::ScheduleState::Ok);
    assert_ne!(report.last_signal_kind.as_deref(), Some("memos"));
    assert!(report.last_signal_handle.is_some());
}

#[test]
fn thread_outline_lists_the_anchor_then_every_reply() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    let sent = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "settle the frob design".into(),
            body: "root body".into(),
            role: "wants".into(),
            kind: Some("want".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let root = sent.delivered[0].handle.clone();

    for n in 0..3 {
        mailspace
            .send(SendRequest {
                from: "cto".into(),
                to: vec!["ceo".into()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: format!("Re: settle the frob design ({n})"),
                body: format!("reply {n}"),
                role: "inbox".into(),
                kind: None,
                reply_to: Some(root.clone()),
                depends_on: Vec::new(),
            })
            .unwrap();
    }

    let outline = mailspace.thread_outline(&root).unwrap();
    assert_eq!(outline.len(), 4);
    assert_eq!(outline[0].handle, root);
    assert_eq!(outline[0].subject, "settle the frob design");
    // A reply's content files under two message rows (recipient inbox and
    // sender sent); the outline resolves by content, so compare subjects.
    let listed: BTreeSet<String> = outline[1..].iter().map(|row| row.subject.clone()).collect();
    let expected: BTreeSet<String> = (0..3)
        .map(|n| format!("Re: settle the frob design ({n})"))
        .collect();
    assert_eq!(listed, expected);
}

#[test]
fn thread_outline_stays_complete_where_the_capped_thread_stops() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    let sent = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "long design".into(),
            body: "root body".into(),
            role: "wants".into(),
            kind: Some("want".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let root = sent.delivered[0].handle.clone();
    for n in 0..51 {
        mailspace
            .send(SendRequest {
                from: "cto".into(),
                to: vec!["ceo".into()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: format!("Re: long design ({n})"),
                body: format!("reply {n}"),
                role: "inbox".into(),
                kind: None,
                reply_to: Some(root.clone()),
                depends_on: Vec::new(),
            })
            .unwrap();
    }

    let outline = mailspace.thread_outline(&root).unwrap();
    assert_eq!(outline.len(), 52);
    assert_eq!(outline[0].handle, root);
    assert_eq!(mailspace.thread(&root, false, 50, 50).unwrap().len(), 50);
}

#[test]
fn task_from_need_binds_the_unit_and_writes_a_parent_header() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity("hand").unwrap();
    let item = |role: &str, kind: &str, subject: &str| {
        mailspace
            .send(SendRequest {
                from: "mind".into(),
                to: vec!["hand".into()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: subject.into(),
                body: "body".into(),
                role: role.into(),
                kind: Some(kind.into()),
                reply_to: None,
                depends_on: Vec::new(),
            })
            .unwrap()
    };
    // mind holds its own sent copy of the need; the node is minted for the
    // delivered copy hand holds.
    let need = item("needs", "need", "lower me");
    let result = mailspace
        .task_from_source(SourceTaskRequest {
            source_handle: need.sent.clone(),
            actor: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            subject: "unit 1".into(),
            body: "do the first part".into(),
        })
        .unwrap();
    assert_eq!(result.source_kind, "need");
    let task = result.delivered[0].handle.clone();

    let (need_rfc, task_bytes, source_kind) = {
        let storage = mailspace.storage().unwrap();
        let need_id = storage.resolve_message_token(&need.sent).unwrap();
        let need_rfc = mail_parser::MessageParser::default()
            .parse(&storage.read_message(&need_id).unwrap())
            .unwrap()
            .message_id()
            .unwrap()
            .to_string();
        let task_id = storage.resolve_message_token(&task).unwrap();
        let bytes = String::from_utf8(storage.read_message(&task_id).unwrap()).unwrap();
        let metadata = storage.item_metadata(&task_id).unwrap();
        (need_rfc, bytes, metadata.get("source_kind").cloned())
    };
    let parent = crate::message::normalize_message_id(&need_rfc).unwrap();
    assert!(
        task_bytes.contains(&format!("X-Vivi-Parent: <{parent}>")),
        "{task_bytes}"
    );
    assert_eq!(source_kind.as_deref(), Some("need"));

    let show = mailspace.graph_show("backlog").unwrap();
    let need_node = show
        .nodes
        .iter()
        .find(|n| n.source_id == need.delivered[0].handle)
        .expect("need node");
    let task_node = show
        .nodes
        .iter()
        .find(|n| n.source_id == task)
        .expect("task node");
    assert_eq!(
        task_node.subgraph.as_deref(),
        Some(need_node.source_id.as_str())
    );
    assert_eq!(need_node.state, "open");

    let want = item("wants", "want", "a want");
    let want_task = mailspace
        .task_from_source(SourceTaskRequest {
            source_handle: want.sent,
            actor: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            subject: "task from want".into(),
            body: "still allowed".into(),
        })
        .unwrap();
    assert_eq!(want_task.source_kind, "want");
}
