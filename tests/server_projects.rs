//! Integration tests for the phase-03 project store: a real vivi mailspace
//! per project holding each letter verbatim under its presented traveling
//! id, with a transport index for the monotonic sequence, content-addressed
//! duplicate handling, envelope-row visibility, and crash convergence
//! between the mailspace and the index.

use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};

use vivi::storage::Storage;
use vivi_server::projects::{AppendItem, ListQuery, ProjectError, ProjectStore, Scan, Viewer};

fn scratch_data_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-projects-u1-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Traveling id shape: `msg_` plus 24 lowercase hex.
fn traveling(n: u64) -> String {
    format!("msg_{n:08x}{:016x}", 0)
}

fn spki_pem(seed: u8) -> String {
    const ED25519_SPKI_PREFIX: &[u8] = &[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut der = ED25519_SPKI_PREFIX.to_vec();
    der.extend_from_slice(
        SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .as_bytes(),
    );
    format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
        STANDARD.encode(der)
    )
}

/// A letter whose bytes are unique per `(id, body)`.
fn eml(id: &str, body: &str) -> Vec<u8> {
    format!("From: hand@p.local\nTo: x@p.local\nMessage-ID: <{id}@p.local>\n\n{body}").into_bytes()
}

fn vouch_eml(id: &str, name: &str, seed: u8) -> Vec<u8> {
    let body = serde_json::json!({"vouch": {"name": name, "public_key_pem": spki_pem(seed)}});
    eml(id, &body.to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A viewer with no mailspace prefix: sees its own mail and shared items.
fn viewer(name: &str) -> Viewer<'_> {
    Viewer { name, prefix: None }
}

/// A viewer whose registered mailspace prefix is `prefix`.
fn viewer_at<'a>(name: &'a str, prefix: &'a str) -> Viewer<'a> {
    Viewer {
        name,
        prefix: Some(prefix),
    }
}

fn letter(id: &str, from: &str, to: Option<&str>, kind: Option<&str>, raw: Vec<u8>) -> AppendItem {
    AppendItem::new(id, from, to, kind, raw).unwrap()
}

/// A need from "hand" with a body unique to `body`.
fn need(id: &str, body: &str) -> AppendItem {
    letter(id, "hand", None, Some("need"), eml(id, body))
}

fn project_dir(data_dir: &Path, project_id: &str) -> PathBuf {
    data_dir.join("projects").join(project_id)
}

/// Where the project mailspace keeps a letter's bytes.
fn blob_path(data_dir: &Path, project_id: &str, content_sha256: &str) -> PathBuf {
    project_dir(data_dir, project_id)
        .join(".vivi")
        .join("blobs")
        .join(&content_sha256[..2])
        .join(&content_sha256[2..4])
        .join(format!("{content_sha256}.eml"))
}

/// The project mailspace, opened as any vivi client would.
fn mailspace(data_dir: &Path, project_id: &str) -> Storage {
    Storage::open_mailspace(&project_dir(data_dir, project_id).join(".vivi")).unwrap()
}

/// The project's transport index.
fn transport(data_dir: &Path, project_id: &str) -> rusqlite::Connection {
    rusqlite::Connection::open(project_dir(data_dir, project_id).join("transport.sqlite")).unwrap()
}

fn transport_rows(data_dir: &Path, project_id: &str) -> i64 {
    transport(data_dir, project_id)
        .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn created_projects_get_distinct_ids_and_a_real_mailspace() {
    let data_dir = scratch_data_dir("create");
    let store = ProjectStore::new(&data_dir);
    let a = store.create_project().unwrap();
    let b = store.create_project().unwrap();
    assert_ne!(a.project_id, b.project_id);
    for meta in [&a, &b] {
        let dir = project_dir(&data_dir, &meta.project_id);
        assert!(dir.join("project.json").is_file());
        assert!(dir.join(".vivi").join("mailspace.toml").is_file());
        assert!(dir.join(".vivi").join("mail.sqlite").is_file());
        assert!(dir.join("transport.sqlite").is_file());
        assert!(
            !dir.join("letters").exists(),
            "no sealed letter directory remains"
        );
    }
}

#[test]
fn append_assigns_sequence_and_files_the_letter_verbatim_in_the_mailspace() {
    let data_dir = scratch_data_dir("append");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let id = traveling(1);
    let item = need(&id, "do the plaintext-marker thing");
    let outcome = store.append(&meta.project_id, &item).unwrap();
    assert_eq!(outcome.sequence, 1);
    assert!(!outcome.duplicate);
    assert_eq!(outcome.message_id, id);
    let raw = eml(&id, "do the plaintext-marker thing");
    let hash = sha256_hex(&raw);
    assert_eq!(outcome.content_sha256, hash);

    // The mailspace holds the exact bytes, in the clear, under the hash.
    let blob = blob_path(&data_dir, &meta.project_id, &hash);
    assert_eq!(std::fs::read(&blob).unwrap(), raw);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&blob).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o077, 0, "owner-only, got {mode:o}");
    }

    // The mailspace index answers for it without reading the letter.
    let rows = mailspace(&data_dir, &meta.project_id)
        .list_messages_by_account_role(&meta.project_id, "needs")
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].message_id, id);
    assert_eq!(rows[0].content_id, hash);
    assert_eq!(rows[0].from_addr, "hand@p.local");
}

/// Every letter a viewer may see, whatever the chunk the index is walked
/// in, in append order: the same letters a single wide page serves, and with
/// `WorkAndEvents` only the work items and event letters among them.
#[test]
fn a_scan_visits_every_visible_letter_across_chunk_boundaries() {
    let data_dir = scratch_data_dir("scan-chunks");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let project = &meta.project_id;
    let event = |n: u64, of: u64, kind: Option<&str>| {
        let (id, item) = (traveling(n), traveling(of));
        let raw = format!(
            "From: hand@p.local\nTo: a@p.local\nMessage-ID: <{id}@p.local>\n\
             In-Reply-To: <{item}@p.local>\nX-Vivi-Event: done\n\
             X-Vivi-Event-Of: <{item}@p.local>\n\nclosed\n"
        )
        .into_bytes();
        letter(&id, "hand", Some("a@p.local"), kind, raw)
    };
    let mail = |n: u64, from: &str, to: &str| {
        let id = traveling(n);
        letter(&id, from, Some(to), None, eml(&id, &format!("mail {n}")))
    };
    let items = [
        need(&traveling(1), "an item"),
        mail(2, "hand", "a@p.local"),
        event(3, 1, Some("mail")),
        letter(
            &traveling(4),
            "hand",
            None,
            Some("task"),
            eml(&traveling(4), "a task"),
        ),
        event(6, 4, Some("task")),
        mail(7, "stranger", "hand@p.local"),
        need(&traveling(8), "another item"),
        mail(9, "hand", "b@p.local"),
        event(10, 8, None),
    ];
    for item in &items {
        store.append(project, item).unwrap();
    }
    let reader = viewer("stranger");
    // The stranger holds no prefix: the work items, the event letters (by
    // their header, under any envelope kind), and its own mail. Other
    // members' private mail (2, 9) is never visited.
    let everything: Vec<String> = [1, 3, 4, 6, 7, 8, 10].map(traveling).into();
    let work_and_events: Vec<String> = [1, 3, 4, 6, 8, 10].map(traveling).into();

    let page = store
        .read_page(project, &ListQuery::since(0), &reader)
        .unwrap();
    let paged: Vec<String> = page.records.into_iter().map(|r| r.message_id).collect();
    assert_eq!(paged, everything, "the page is the baseline");

    for chunk in [1, 2, 3, 4, 7, 8, 500] {
        for (scan, expected) in [
            (Scan::All, &everything),
            (Scan::WorkAndEvents, &work_and_events),
        ] {
            let mut seen = Vec::new();
            store
                .scan_visible_in_chunks(project, &reader, scan, chunk, |record| {
                    seen.push((record.sequence, record.message_id));
                })
                .unwrap();
            let ids: Vec<String> = seen.iter().map(|(_, id)| id.clone()).collect();
            assert_eq!(&ids, expected, "chunk {chunk}, {scan:?}");
            assert!(
                seen.windows(2).all(|pair| pair[0].0 < pair[1].0),
                "append order, chunk {chunk}, {scan:?}"
            );
        }
    }
    // An empty project is a scan of nothing.
    let empty = store.create_project().unwrap();
    store
        .scan_visible(&empty.project_id, &reader, Scan::All, |_| {
            panic!("an empty project holds no letter")
        })
        .unwrap();
}

#[test]
fn an_event_letter_files_as_mail_in_the_mailspace_not_as_work() {
    let data_dir = scratch_data_dir("event-role");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let item_id = traveling(1);
    store
        .append(&meta.project_id, &need(&item_id, "an item"))
        .unwrap();
    // The event rides the need's kind, as a client pushes it.
    let event_id = traveling(2);
    let raw = format!(
        "From: hand@p.local\nTo: a@p.local\nMessage-ID: <{event_id}@p.local>\n\
         In-Reply-To: <{item_id}@p.local>\nX-Vivi-Event: done\n\
         X-Vivi-Event-Of: <{item_id}@p.local>\n\nclosed\n"
    )
    .into_bytes();
    let event = AppendItem::new(&event_id, "hand", Some("a@p.local"), Some("need"), raw).unwrap();
    store.append(&meta.project_id, &event).unwrap();
    // The current client sends it as kind mail.
    let mail_id = traveling(4);
    let raw = format!(
        "From: hand@p.local\nTo: a@p.local\nMessage-ID: <{mail_id}@p.local>\n\
         In-Reply-To: <{item_id}@p.local>\nX-Vivi-Event: reopened\n\
         X-Vivi-Event-Of: <{item_id}@p.local>\n\nreopened\n"
    )
    .into_bytes();
    let mail = AppendItem::new(&mail_id, "hand", Some("a@p.local"), Some("mail"), raw).unwrap();
    store.append(&meta.project_id, &mail).unwrap();
    // A letter that merely names the header in its body is still work.
    let decoy_id = traveling(3);
    let decoy = format!(
        "From: hand@p.local\nTo: a@p.local\nMessage-ID: <{decoy_id}@p.local>\n\n\
         X-Vivi-Event: done\n"
    )
    .into_bytes();
    let decoy = AppendItem::new(&decoy_id, "hand", Some("a@p.local"), Some("need"), decoy).unwrap();
    store.append(&meta.project_id, &decoy).unwrap();

    let storage = mailspace(&data_dir, &meta.project_id);
    let needs: Vec<_> = storage
        .list_messages_by_account_role(&meta.project_id, "needs")
        .unwrap()
        .into_iter()
        .map(|row| row.message_id)
        .collect();
    assert_eq!(needs, [item_id, decoy_id]);
    let inbox: Vec<_> = storage
        .list_messages_by_account_role(&meta.project_id, "inbox")
        .unwrap()
        .into_iter()
        .map(|row| row.message_id)
        .collect();
    assert_eq!(inbox, [event_id, mail_id]);
}

#[test]
fn item_validation_rejects_bad_id_empty_oversize_kind_and_recipient() {
    let ok = |to: Option<&str>, kind: Option<&str>| {
        AppendItem::new(&traveling(1), "hand", to, kind, b"x".to_vec())
    };
    assert!(ok(None, None).is_ok());
    assert!(ok(Some("Bob.s-1_x@3f9a1c7e.local"), Some("  WANT ")).is_ok());
    assert!(ok(Some("postmaster"), Some("mail")).is_ok());
    assert!(ok(Some("PostMaster"), None).is_ok());
    assert!(ok(None, Some("")).is_ok());
    // A blank header is no recipient.
    assert!(ok(Some("  "), None).is_ok());
    for bad_to in [
        "has space@x.local",
        "bob",
        "a@",
        "@b",
        "postmaster, a@x.local",
        &"a".repeat(65),
    ] {
        assert!(
            matches!(ok(Some(bad_to), None), Err(ProjectError::BadRequest(_))),
            "to {bad_to:?}"
        );
    }
    assert!(matches!(
        ok(None, Some("memo")),
        Err(ProjectError::BadRequest(_))
    ));
    for bad_id in ["", "plain-1", "msg_ABCDEF000000000000000000", "msg_0000"] {
        assert!(matches!(
            AppendItem::new(bad_id, "hand", None, None, b"x".to_vec()),
            Err(ProjectError::BadRequest(_))
        ));
    }
    assert!(matches!(
        AppendItem::new(&traveling(1), "hand", None, None, Vec::new()),
        Err(ProjectError::BadRequest(_))
    ));
    assert!(
        AppendItem::new(&traveling(1), "hand", None, None, vec![b'x'; 64 * 1024]).is_ok(),
        "exactly 64 KiB is allowed"
    );
    assert!(matches!(
        AppendItem::new(&traveling(1), "hand", None, None, vec![b'x'; 64 * 1024 + 1]),
        Err(ProjectError::TooLarge)
    ));
}

#[test]
fn replay_same_id_same_bytes_is_duplicate_noop() {
    let data_dir = scratch_data_dir("duplicate");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let id = traveling(2);
    let first = store
        .append(&meta.project_id, &need(&id, "same body"))
        .unwrap();
    let replay = store
        .append(&meta.project_id, &need(&id, "same body"))
        .unwrap();
    assert!(replay.duplicate);
    assert_eq!(replay.sequence, first.sequence);
    assert_eq!(replay.content_sha256, first.content_sha256);
    // Nothing new was stored: one mailspace row, one transport row, and the
    // counter did not advance.
    assert_eq!(
        mailspace(&data_dir, &meta.project_id)
            .list_messages()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(transport_rows(&data_dir, &meta.project_id), 1);
    let other = store
        .append(&meta.project_id, &need(&traveling(3), "next"))
        .unwrap();
    assert_eq!(other.sequence, first.sequence + 1);
}

#[test]
fn same_bytes_under_another_id_is_a_duplicate_of_the_original() {
    let data_dir = scratch_data_dir("same-bytes");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let bytes = eml(&traveling(1), "shared bytes");
    let first = store
        .append(
            &meta.project_id,
            &letter(&traveling(1), "hand", None, None, bytes.clone()),
        )
        .unwrap();
    let alias = store
        .append(
            &meta.project_id,
            &letter(&traveling(9), "hand", None, None, bytes),
        )
        .unwrap();
    assert!(alias.duplicate);
    assert_eq!(alias.message_id, traveling(1));
    assert_eq!(alias.sequence, first.sequence);
    let read = store
        .read_after_scoped(&meta.project_id, 0, &viewer("hand"))
        .unwrap();
    assert_eq!(read.records.len(), 1, "the alias id was never stored");
}

#[test]
fn same_bytes_from_another_member_do_not_reveal_the_original() {
    let data_dir = scratch_data_dir("same-bytes-oracle");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let bytes = eml(&traveling(1), "private bytes");
    let original = store
        .append(
            &meta.project_id,
            &letter(
                &traveling(1),
                "alice",
                Some("bob@aaaaaaaa.local"),
                None,
                bytes.clone(),
            ),
        )
        .unwrap();
    // "carol" holds the exact bytes but is neither sender nor recipient. She
    // gets an ordinary fresh append under her own id, not the original's.
    let probe = store
        .append(
            &meta.project_id,
            &letter(
                &traveling(9),
                "carol",
                Some("carol@bbbbbbbb.local"),
                None,
                bytes,
            ),
        )
        .unwrap();
    assert!(!probe.duplicate);
    assert_eq!(probe.message_id, traveling(9));
    assert_ne!(probe.sequence, original.sequence);
    let carol = store
        .read_after_scoped(&meta.project_id, 0, &viewer("carol"))
        .unwrap();
    assert_eq!(carol.records.len(), 1);
    assert_eq!(carol.records[0].message_id, traveling(9));
}

#[test]
fn same_id_different_bytes_is_refused_and_writes_nothing() {
    let data_dir = scratch_data_dir("conflict");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let id = traveling(4);
    store
        .append(&meta.project_id, &need(&id, "original body"))
        .unwrap();
    let err = store
        .append(&meta.project_id, &need(&id, "tampered body"))
        .unwrap_err();
    assert!(matches!(err, ProjectError::Conflict(_)), "got {err:?}");
    assert_eq!(
        mailspace(&data_dir, &meta.project_id)
            .list_messages()
            .unwrap()
            .len(),
        1,
        "the refused letter left no mailspace row"
    );
    assert_eq!(transport_rows(&data_dir, &meta.project_id), 1);
    let read = store
        .read_after_scoped(&meta.project_id, 0, &viewer("hand"))
        .unwrap();
    assert_eq!(read.records.len(), 1);
    assert_eq!(read.records[0].raw_eml, eml(&id, "original body"));
}

#[test]
fn read_returns_suffix_with_exact_bytes_and_envelope() {
    let data_dir = scratch_data_dir("read-after");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    for n in 10..13 {
        let id = traveling(n);
        let mut raw = eml(&id, &format!("body-{n}"));
        // Non-UTF-8 bytes and CRLF must survive untouched.
        raw.extend_from_slice(b"\r\n\xff\xfe\x00");
        let item = letter(&id, "hand", Some("bob@aaaaaaaa.local"), Some("need"), raw);
        store.append(&meta.project_id, &item).unwrap();
    }

    let all = store
        .read_after_scoped(&meta.project_id, 0, &viewer("hand"))
        .unwrap();
    assert_eq!(
        all.records.iter().map(|r| r.sequence).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    let second = &all.records[1];
    assert_eq!(second.message_id, traveling(11));
    assert_eq!(second.from, "hand");
    assert_eq!(second.to.as_deref(), Some("bob@aaaaaaaa.local"));
    assert_eq!(second.kind.as_deref(), Some("need"));
    assert!(second.raw_eml.ends_with(b"\r\n\xff\xfe\x00"));
    assert_eq!(second.content_sha256, sha256_hex(&second.raw_eml));
    assert_eq!(all.next_cursor, 3);

    let suffix = store
        .read_after_scoped(&meta.project_id, 1, &viewer("hand"))
        .unwrap();
    assert_eq!(
        suffix
            .records
            .iter()
            .map(|r| r.sequence)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(suffix.next_cursor, 3);
}

#[test]
fn sequence_stays_monotonic_after_close_and_reopen() {
    let data_dir = scratch_data_dir("reopen");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let first = store
        .append(&meta.project_id, &need(&traveling(5), "one"))
        .unwrap();
    assert_eq!(first.sequence, 1);
    drop(store);
    let reopened = ProjectStore::new(&data_dir);
    let second = reopened
        .append(&meta.project_id, &need(&traveling(6), "two"))
        .unwrap();
    assert_eq!(second.sequence, 2);
    // The pre-restart id still dedups to its stored sequence.
    let replay = reopened
        .append(&meta.project_id, &need(&traveling(5), "one"))
        .unwrap();
    assert!(replay.duplicate);
    assert_eq!(replay.sequence, 1);
    let read = reopened
        .read_after_scoped(&meta.project_id, 0, &viewer("hand"))
        .unwrap();
    assert_eq!(read.records[0].raw_eml, eml(&traveling(5), "one"));
}

/// A crash between the mailspace write and the transport row leaves the
/// letter in the mailspace and no row; a replay of the append converges on
/// the same state a clean append would have produced.
#[test]
fn mailspace_letter_without_a_transport_row_converges_on_replay() {
    let data_dir = scratch_data_dir("orphan");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    store
        .append(&meta.project_id, &need(&traveling(1), "kept"))
        .unwrap();
    let lost = need(&traveling(2), "lost to the crash");
    let outcome = store.append(&meta.project_id, &lost).unwrap();
    assert_eq!(outcome.sequence, 2);

    // Roll the transport index back to just before the second append: the
    // letter stays in the mailspace.
    let conn = transport(&data_dir, &meta.project_id);
    conn.execute("DELETE FROM recipients WHERE sequence = 2", [])
        .unwrap();
    conn.execute("DELETE FROM items WHERE sequence = 2", [])
        .unwrap();
    drop(conn);
    assert_eq!(
        mailspace(&data_dir, &meta.project_id)
            .list_messages()
            .unwrap()
            .len(),
        2,
        "the letter is still in the mailspace"
    );
    let hidden = store
        .read_after_scoped(&meta.project_id, 0, &viewer("hand"))
        .unwrap();
    assert_eq!(hidden.records.len(), 1, "no row, so not served");

    let replay = store.append(&meta.project_id, &lost).unwrap();
    assert!(!replay.duplicate, "the row was never written");
    assert_eq!(replay.sequence, 2);
    assert_eq!(replay.content_sha256, outcome.content_sha256);
    let read = store
        .read_after_scoped(&meta.project_id, 0, &viewer("hand"))
        .unwrap();
    assert_eq!(read.records.len(), 2);
    assert_eq!(
        read.records[1].raw_eml,
        eml(&traveling(2), "lost to the crash")
    );
    assert_eq!(
        mailspace(&data_dir, &meta.project_id)
            .list_messages()
            .unwrap()
            .len(),
        2,
        "the replay did not file the letter twice"
    );
}

/// A transport row whose letter is gone is a storage error, never a skip.
#[test]
fn missing_blob_fails_the_read() {
    let data_dir = scratch_data_dir("missing-blob");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    store
        .append(&meta.project_id, &need(&traveling(1), "one"))
        .unwrap();
    let second = store
        .append(&meta.project_id, &need(&traveling(2), "two"))
        .unwrap();
    std::fs::remove_file(blob_path(
        &data_dir,
        &meta.project_id,
        &second.content_sha256,
    ))
    .unwrap();
    let err = store
        .read_after_scoped(&meta.project_id, 0, &viewer("hand"))
        .unwrap_err();
    assert!(matches!(err, ProjectError::Storage(_)), "got {err:?}");
}

/// A blob that reads but does not hash to its row (another letter's bytes
/// copied over it) is a storage error too.
#[test]
fn swapped_blob_fails_the_hash_check() {
    let data_dir = scratch_data_dir("swapped-blob");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let first = store
        .append(&meta.project_id, &need(&traveling(1), "one"))
        .unwrap();
    let second = store
        .append(&meta.project_id, &need(&traveling(2), "two"))
        .unwrap();
    let first_blob = blob_path(&data_dir, &meta.project_id, &first.content_sha256);
    let second_blob = blob_path(&data_dir, &meta.project_id, &second.content_sha256);
    let mut perms = std::fs::metadata(&first_blob).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&first_blob, perms).unwrap();
    std::fs::copy(&second_blob, &first_blob).unwrap();
    let err = store
        .read_after_scoped(&meta.project_id, 0, &viewer("hand"))
        .unwrap_err();
    assert!(matches!(err, ProjectError::Storage(_)), "got {err:?}");
}

/// Ordering comes from the sequence counter, never from id sorting: append
/// id 2 before id 1 and read back in append order.
#[test]
fn read_order_follows_append_order_not_id_order() {
    let data_dir = scratch_data_dir("order");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    store
        .append(&meta.project_id, &need(&traveling(2), "first in"))
        .unwrap();
    store
        .append(&meta.project_id, &need(&traveling(1), "second in"))
        .unwrap();
    let read = store
        .read_after_scoped(&meta.project_id, 0, &viewer("hand"))
        .unwrap();
    let ids: Vec<&str> = read.records.iter().map(|r| r.message_id.as_str()).collect();
    assert_eq!(ids, vec![traveling(2), traveling(1)]);
    assert_eq!(read.next_cursor, 2);
}

/// Scoped read (decided item 7 plus the governance carve-out): private mail
/// reaches only its sender and recipient, work items and postmaster
/// governance mail reach every member, and a filtered-out item does not
/// advance the cursor.
#[test]
fn read_after_scoped_filters_mail_but_keeps_work_and_governance() {
    let data_dir = scratch_data_dir("scoped");
    let store = ProjectStore::new(&data_dir);
    // The creator is "hand", so the governance vouch below passes the
    // admitted-sender check.
    let meta = store
        .create_project_with_creator(&spki_pem(1), "hand")
        .unwrap();

    // seq 1: plain mail alice -> bob.
    let first = letter(
        &traveling(20),
        "alice",
        Some("bob@aaaaaaaa.local"),
        None,
        eml(&traveling(20), "for bob only"),
    );
    store.append(&meta.project_id, &first).unwrap();
    // seq 2: unaddressed work item, member-visible by kind.
    store
        .append(&meta.project_id, &need(&traveling(21), "a need"))
        .unwrap();
    // seq 3: governance mail to postmaster, member-visible by the carve-out.
    let gov = letter(
        &traveling(22),
        "hand",
        Some("PostMaster"),
        None,
        vouch_eml(&traveling(22), "bob", 2),
    );
    store.append(&meta.project_id, &gov).unwrap();
    // seq 4: plain mail alice -> bob again.
    let second = letter(
        &traveling(23),
        "alice",
        Some("bob@aaaaaaaa.local"),
        Some("mail"),
        eml(&traveling(23), "again for bob"),
    );
    store.append(&meta.project_id, &second).unwrap();

    let ids = |viewer: Viewer<'_>, since: u64| {
        let page = store
            .read_after_scoped(&meta.project_id, since, &viewer)
            .unwrap();
        let ids: Vec<String> = page.records.iter().map(|r| r.message_id.clone()).collect();
        (ids, page.next_cursor)
    };

    // The recipient sees the mail addressed to them plus all shared items.
    let (bob_ids, bob_cursor) = ids(viewer_at("bob", "aaaaaaaa"), 0);
    assert_eq!(
        bob_ids,
        vec![traveling(20), traveling(21), traveling(22), traveling(23)]
    );
    assert_eq!(bob_cursor, 4);

    // A bystander sees the work item and governance mail but neither
    // private mail; the cursor reflects returned records only (3, not 4).
    let (carol_ids, carol_cursor) = ids(viewer_at("carol", "bbbbbbbb"), 0);
    assert_eq!(carol_ids, vec![traveling(21), traveling(22)]);
    assert_eq!(carol_cursor, 3);

    // The sender still sees their own sent mail.
    assert_eq!(ids(viewer("alice"), 0).0.len(), 4);

    // Resuming after the last returned record: the remaining letter is
    // hidden from carol, so the result is empty and the cursor stays put.
    let (tail, tail_cursor) = ids(viewer_at("carol", "bbbbbbbb"), 3);
    assert!(tail.is_empty());
    assert_eq!(tail_cursor, 3);
}

/// An item event letter in mail dress.
fn event_mail(id: &str, from: &str, to: &str, of: &str, header: &str) -> AppendItem {
    let raw = format!(
        "From: {from}@aaaaaaaa.local\nTo: {to}\nMessage-ID: <{id}@aaaaaaaa.local>\n\
         In-Reply-To: <{of}@aaaaaaaa.local>\n{header}\
         X-Vivi-Event-Of: <{of}@aaaaaaaa.local>\n\nnote\n"
    )
    .into_bytes();
    letter(id, from, Some(to), None, raw)
}

/// A letter that carries `X-Vivi-Event` reaches every member whatever its
/// envelope kind: decided by the header, so a client that sends every event
/// as `mail` is still served to the members who hold the item. A letter that
/// only names the header in its body, or a payload header
/// (`X-Vivi-Event-Of`) without the marker, stays private mail.
#[test]
fn an_event_letter_sent_as_mail_is_visible_to_every_member() {
    let data_dir = scratch_data_dir("event-visible");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let item = traveling(30);
    store
        .append(&meta.project_id, &need(&item, "an item"))
        .unwrap();

    // seq 2: a mail-kind event from alice addressed to bob only.
    let done = traveling(31);
    store
        .append(
            &meta.project_id,
            &event_mail(
                &done,
                "alice",
                "bob@bbbbbbbb.local",
                &item,
                "X-Vivi-Event: done\n",
            ),
        )
        .unwrap();
    // seq 3: a kind this server does not know is still carried by header.
    let later = traveling(32);
    store
        .append(
            &meta.project_id,
            &event_mail(
                &later,
                "alice",
                "bob@bbbbbbbb.local",
                &item,
                "X-Vivi-Event: from-a-later-client\n",
            ),
        )
        .unwrap();
    // seq 4: private mail naming the header only in its body.
    let decoy = traveling(33);
    store
        .append(
            &meta.project_id,
            &letter(
                &decoy,
                "alice",
                Some("bob@bbbbbbbb.local"),
                None,
                format!(
                    "From: alice@aaaaaaaa.local\nTo: bob@bbbbbbbb.local\n\
                     Message-ID: <{decoy}@aaaaaaaa.local>\n\nX-Vivi-Event: done\n"
                )
                .into_bytes(),
            ),
        )
        .unwrap();
    // seq 5: private mail carrying a payload header but not the marker.
    let near = traveling(34);
    store
        .append(
            &meta.project_id,
            &letter(
                &near,
                "alice",
                Some("bob@bbbbbbbb.local"),
                None,
                format!(
                    "From: alice@aaaaaaaa.local\nTo: bob@bbbbbbbb.local\n\
                     Message-ID: <{near}@aaaaaaaa.local>\nX-Vivi-Event-Of: <{item}@aaaaaaaa.local>\n\
                     \nsecret\n"
                )
                .into_bytes(),
            ),
        )
        .unwrap();

    let ids = |viewer: Viewer<'_>| -> Vec<String> {
        store
            .read_after_scoped(&meta.project_id, 0, &viewer)
            .unwrap()
            .records
            .into_iter()
            .map(|record| record.message_id)
            .collect()
    };
    // carol is neither the sender nor an addressee: she sees the work item
    // and both event letters, not the private mail.
    assert_eq!(
        ids(viewer_at("carol", "cccccccc")),
        vec![item.clone(), done.clone(), later.clone()]
    );
    // A viewer with no registered prefix is served them as well.
    assert_eq!(ids(viewer("dave")), vec![item.clone(), done.clone(), later]);
    // The addressee still sees the private mail; the sender sees everything.
    assert_eq!(ids(viewer_at("bob", "bbbbbbbb")).len(), 5);
    assert_eq!(ids(viewer("alice")).len(), 5);

    // The same rule decides a read by id and a lookup by handle.
    let carol = viewer_at("carol", "cccccccc");
    assert!(
        store
            .read_one_scoped(&meta.project_id, &done, &carol)
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .read_one_scoped(&meta.project_id, &decoy, &carol)
            .unwrap()
            .is_none()
    );
    let found = store
        .lookup_handles(
            &meta.project_id,
            &[done[4..12].to_owned(), decoy[4..12].to_owned()],
            &carol,
        )
        .unwrap();
    assert_eq!(found.records.len(), 1);
    assert_eq!(found.missing, vec![decoy[4..12].to_owned()]);
}

/// An index written before the `event` column existed is upgraded on its
/// first open: a mail letter held that carries `X-Vivi-Event` becomes
/// member-visible, and one that does not stays private.
#[test]
fn an_index_without_the_event_column_is_upgraded_and_backfilled() {
    let data_dir = scratch_data_dir("event-backfill");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let item = traveling(40);
    store
        .append(&meta.project_id, &need(&item, "an item"))
        .unwrap();
    let done = traveling(41);
    store
        .append(
            &meta.project_id,
            &event_mail(
                &done,
                "alice",
                "bob@bbbbbbbb.local",
                &item,
                "X-Vivi-Event: done\n",
            ),
        )
        .unwrap();
    let private = traveling(42);
    store
        .append(
            &meta.project_id,
            &letter(
                &private,
                "alice",
                Some("bob@bbbbbbbb.local"),
                None,
                eml(&private, "private"),
            ),
        )
        .unwrap();
    // Take the index back to the schema that predates the column.
    transport(&data_dir, &meta.project_id)
        .execute_batch("ALTER TABLE items DROP COLUMN event")
        .unwrap();

    let carol = viewer_at("carol", "cccccccc");
    let seen: Vec<String> = ProjectStore::new(&data_dir)
        .read_after_scoped(&meta.project_id, 0, &carol)
        .unwrap()
        .records
        .into_iter()
        .map(|record| record.message_id)
        .collect();
    assert_eq!(seen, vec![item, done]);
    // The upgrade is permanent and a second open leaves it alone.
    let again = ProjectStore::new(&data_dir)
        .read_after_scoped(&meta.project_id, 0, &carol)
        .unwrap();
    assert_eq!(again.records.len(), 2);
    let marked: i64 = transport(&data_dir, &meta.project_id)
        .query_row("SELECT COUNT(*) FROM items WHERE event = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(marked, 1);
}

/// A viewer is never served a letter they cannot see: a corrupt blob behind
/// an invisible row does not affect their read.
#[test]
fn invisible_letters_are_not_read() {
    let data_dir = scratch_data_dir("invisible");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let private = letter(
        &traveling(1),
        "alice",
        Some("bob@aaaaaaaa.local"),
        None,
        eml(&traveling(1), "private"),
    );
    let outcome = store.append(&meta.project_id, &private).unwrap();
    std::fs::write(
        blob_path(&data_dir, &meta.project_id, &outcome.content_sha256),
        b"not the letter",
    )
    .unwrap();
    let carol = store
        .read_after_scoped(&meta.project_id, 0, &viewer("carol"))
        .unwrap();
    assert!(carol.records.is_empty());
    assert!(
        store
            .read_after_scoped(&meta.project_id, 0, &viewer_at("bob", "aaaaaaaa"))
            .is_err()
    );
}

/// A fresh append wakes a waiter on that project.
#[tokio::test]
async fn append_wakes_waiter() {
    let data_dir = scratch_data_dir("wake");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let project_id = meta.project_id.clone();
    let waiter = {
        let store = store.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            store
                .wake()
                .wait(&project_id, std::time::Duration::from_secs(5))
                .await
        })
    };
    tokio::task::yield_now().await;
    tokio::task::spawn_blocking(move || {
        store.append(&meta.project_id, &need(&traveling(9), "wake"))
    })
    .await
    .unwrap()
    .unwrap();
    assert!(waiter.await.unwrap(), "fresh append must wake the waiter");
}

/// A duplicate changes nothing and wakes nothing.
#[tokio::test]
async fn duplicate_append_does_not_wake_waiter() {
    let data_dir = scratch_data_dir("wake-dup");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let item = need(&traveling(9), "wake");
    store.append(&meta.project_id, &item).unwrap();
    let project_id = meta.project_id.clone();
    let waiter = {
        let store = store.clone();
        tokio::spawn(async move {
            store
                .wake()
                .wait(&project_id, std::time::Duration::from_millis(200))
                .await
        })
    };
    tokio::task::yield_now().await;
    let replay = tokio::task::spawn_blocking(move || store.append(&meta.project_id, &item))
        .await
        .unwrap()
        .unwrap();
    assert!(replay.duplicate);
    assert!(!waiter.await.unwrap(), "a duplicate must not wake");
}

/// The stamp is set by the caller from the authenticated sender and read
/// back from the index row; an unstamped letter reads as `None`.
#[test]
fn from_prefix_is_recorded_at_append() {
    let data_dir = scratch_data_dir("from-prefix");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let stamped = need(&traveling(1), "stamped").with_from_prefix(Some("3f9a1c7e"));
    store.append(&meta.project_id, &stamped).unwrap();
    store
        .append(&meta.project_id, &need(&traveling(2), "plain"))
        .unwrap();
    let read = store
        .read_after_scoped(&meta.project_id, 0, &viewer("hand"))
        .unwrap();
    assert_eq!(read.records[0].from_prefix.as_deref(), Some("3f9a1c7e"));
    assert_eq!(read.records[1].from_prefix, None);
}

// ---- append time, paged and filtered list, lookup by handle ----

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use chrono::{TimeZone, Utc};
use vivi_server::projects::{PAGE_BYTE_CAP, monotone_stamp};

/// A store whose clock reads `seconds` (unix), which the test steps.
fn stepped_store(name: &str) -> (ProjectStore, Arc<AtomicI64>, PathBuf) {
    let data_dir = scratch_data_dir(name);
    let seconds = Arc::new(AtomicI64::new(0));
    let clock = seconds.clone();
    let store = ProjectStore::with_clock(&data_dir, move || {
        Utc.timestamp_opt(clock.load(Ordering::SeqCst), 0).unwrap()
    });
    (store, seconds, data_dir)
}

fn stamp_of(unix: i64) -> String {
    Utc.timestamp_opt(unix, 0)
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// A traveling id whose handle is `handle` (8 hex) and whose tail is `tail`.
fn id_with(handle: &str, tail: u64) -> String {
    format!("msg_{handle}{tail:016x}")
}

/// All records visible to `who` for `query`, as `(message_id, sequence)`.
fn page_ids(
    store: &ProjectStore,
    project: &str,
    query: &ListQuery,
    who: &Viewer<'_>,
) -> (Vec<String>, u64, bool) {
    let page = store.read_page(project, query, who).unwrap();
    (
        page.records.iter().map(|r| r.message_id.clone()).collect(),
        page.next_cursor,
        page.has_more,
    )
}

#[test]
fn monotone_stamp_never_goes_behind_the_previous_row() {
    let at = |s: i64| Utc.timestamp_opt(s, 0).unwrap();
    assert_eq!(monotone_stamp(None, at(100)), stamp_of(100));
    assert_eq!(monotone_stamp(Some(&stamp_of(50)), at(100)), stamp_of(100));
    assert_eq!(monotone_stamp(Some(&stamp_of(100)), at(100)), stamp_of(100));
    // The clock stepped back: the previous stamp wins.
    assert_eq!(monotone_stamp(Some(&stamp_of(200)), at(100)), stamp_of(200));
}

#[test]
fn append_stamps_time_monotonically_and_a_replay_keeps_the_stamp() {
    let (store, clock, _) = stepped_store("stamp");
    let meta = store.create_project().unwrap();
    let hand = viewer("hand");

    clock.store(1_000, Ordering::SeqCst);
    let first = need(&traveling(1), "first");
    store.append(&meta.project_id, &first).unwrap();
    // The clock steps backward: the stamp holds at the previous value.
    clock.store(400, Ordering::SeqCst);
    store
        .append(&meta.project_id, &need(&traveling(2), "second"))
        .unwrap();
    clock.store(2_000, Ordering::SeqCst);
    store
        .append(&meta.project_id, &need(&traveling(3), "third"))
        .unwrap();
    // A replay (same id, same bytes) changes nothing, whatever the clock says.
    clock.store(9_000, Ordering::SeqCst);
    let replay = store.append(&meta.project_id, &first).unwrap();
    assert!(replay.duplicate);

    let page = store.read_after_scoped(&meta.project_id, 0, &hand).unwrap();
    let stamps: Vec<_> = page.records.iter().map(|r| r.appended_at.clone()).collect();
    assert_eq!(
        stamps,
        vec![stamp_of(1_000), stamp_of(1_000), stamp_of(2_000)]
    );
    assert_eq!(page.records.len(), 3, "the replay stored no row");
}

/// Eight letters at known times, kinds, and recipients, appended by "hand".
/// Returns the store, project id, and the ids in sequence order.
fn windowed_project(name: &str) -> (ProjectStore, String, Vec<String>) {
    let (store, clock, _) = stepped_store(name);
    let meta = store.create_project().unwrap();
    // (kind, to, unix time)
    let rows: [(Option<&str>, Option<&str>, i64); 8] = [
        (Some("need"), None, 100),
        (Some("task"), None, 200),
        (None, Some("a@aaaaaaaa.local"), 300),
        (Some("want"), Some("a@aaaaaaaa.local"), 400),
        (None, Some("B@bbbbbbbb.local, a@aaaaaaaa.local"), 500),
        (Some("need"), Some("a@bbbbbbbb.local"), 600),
        (None, Some("a@aaaaaaaa.example"), 700),
        (None, Some("a@aaaaaaaa.local"), 800),
    ];
    let mut ids = Vec::new();
    for (n, (kind, to, at)) in rows.iter().enumerate() {
        clock.store(*at, Ordering::SeqCst);
        let id = traveling(n as u64 + 1);
        let item = letter(&id, "hand", *to, *kind, eml(&id, &format!("row {n}")));
        store.append(&meta.project_id, &item).unwrap();
        ids.push(id);
    }
    (store, meta.project_id, ids)
}

#[test]
fn each_filter_alone_and_combined_only_narrows() {
    let (store, project, ids) = windowed_project("filters");
    // "hand" is the sender, so every letter is visible to it.
    let hand = viewer("hand");
    let run = |limit: Option<&str>, after: Option<&str>, kind: Option<&str>, to: Option<&str>| {
        let query = ListQuery::parse(0, limit, after, kind, to).unwrap();
        page_ids(&store, &project, &query, &hand)
    };
    let pick = |ns: &[usize]| ns.iter().map(|n| ids[*n].clone()).collect::<Vec<_>>();

    // kind: lists, case-insensitive, `mail` is no kind.
    assert_eq!(run(None, None, Some("need"), None).0, pick(&[0, 5]));
    assert_eq!(run(None, None, Some("task, WANT"), None).0, pick(&[1, 3]));
    assert_eq!(run(None, None, Some("mail"), None).0, pick(&[2, 4, 6, 7]));
    assert_eq!(
        run(None, None, Some("need,mail"), None).0,
        pick(&[0, 2, 4, 5, 6, 7])
    );
    // after: at or after the instant (300 is included).
    assert_eq!(
        run(None, Some(&stamp_of(300)), None, None).0,
        pick(&[2, 3, 4, 5, 6, 7])
    );
    assert_eq!(run(None, Some(&stamp_of(301)), None, None).0.len(), 5);
    // A sub-second `after` rounds up, so it never admits an earlier row.
    assert_eq!(
        run(None, Some("1970-01-01T00:05:00.5Z"), None, None).0,
        pick(&[3, 4, 5, 6, 7])
    );
    // An offset time is the same instant.
    assert_eq!(
        run(None, Some("1970-01-01T01:05:00+01:00"), None, None).0,
        pick(&[2, 3, 4, 5, 6, 7])
    );
    // to: exact address (any case), and a bare prefix names `<prefix>.local`.
    assert_eq!(
        run(None, None, None, Some("A@AAAAAAAA.local")).0,
        pick(&[2, 3, 4, 7])
    );
    assert_eq!(
        run(None, None, None, Some("aaaaaaaa")).0,
        pick(&[2, 3, 4, 7]),
        "the .example domain is not a mailspace"
    );
    assert_eq!(run(None, None, None, Some("BBBBBBBB")).0, pick(&[4, 5]));
    assert_eq!(
        run(None, None, None, Some("b@bbbbbbbb.local")).0,
        pick(&[4]),
        "an exact address matches within a recipient list"
    );
    assert!(run(None, None, None, Some("cccccccc")).0.is_empty());
    // limit.
    let (page, cursor, more) = run(Some("3"), None, None, None);
    assert_eq!(page, pick(&[0, 1, 2]));
    assert_eq!((cursor, more), (3, true));
    // Combined: AND of everything.
    let (page, cursor, more) = run(
        Some("1"),
        Some(&stamp_of(400)),
        Some("mail,want"),
        Some("aaaaaaaa"),
    );
    assert_eq!(page, pick(&[3]));
    assert_eq!((cursor, more), (4, true));
    let (page, cursor, more) = run(None, Some(&stamp_of(400)), Some("mail"), Some("aaaaaaaa"));
    assert_eq!(page, pick(&[4, 7]));
    assert_eq!((cursor, more), (8, false));
    // Nothing matched: the cursor is `since`.
    let query = ListQuery::parse(2, None, None, Some("task"), None).unwrap();
    let (page, cursor, more) = page_ids(&store, &project, &query, &hand);
    assert!(page.is_empty());
    assert_eq!((cursor, more), (2, false));
}

#[test]
fn filters_never_widen_visibility() {
    let (store, project, ids) = windowed_project("filters-visibility");
    // A bystander with no prefix sees only the work items (rows 0, 1, 3, 5),
    // even when a filter names the mail's recipient.
    let carol = viewer("carol");
    let query = ListQuery::parse(0, None, None, None, Some("aaaaaaaa")).unwrap();
    let (page, _, _) = page_ids(&store, &project, &query, &carol);
    assert_eq!(page, vec![ids[3].clone()], "only the want to that prefix");
    let query = ListQuery::parse(0, None, None, Some("mail"), None).unwrap();
    assert!(page_ids(&store, &project, &query, &carol).0.is_empty());
    // The prefix's own member sees the mail naming it, and no other mail.
    let member = viewer_at("alice", "aaaaaaaa");
    let query = ListQuery::parse(0, None, None, Some("mail"), None).unwrap();
    assert_eq!(
        page_ids(&store, &project, &query, &member).0,
        vec![ids[2].clone(), ids[4].clone(), ids[7].clone()]
    );
}

#[test]
fn bad_list_parameters_are_refused() {
    let bad = |limit, after, kind, to| {
        assert!(
            matches!(
                ListQuery::parse(0, limit, after, kind, to),
                Err(ProjectError::BadRequest(_))
            ),
            "{limit:?} {after:?} {kind:?} {to:?}"
        );
    };
    bad(Some("0"), None, None, None);
    bad(Some("-1"), None, None, None);
    bad(Some("abc"), None, None, None);
    bad(Some(""), None, None, None);
    bad(None, Some("yesterday"), None, None);
    bad(None, Some("2026-10-11"), None, None);
    bad(None, None, Some("memo"), None);
    bad(None, None, Some("need,"), None);
    bad(None, None, None, Some("bob"));
    bad(None, None, None, Some("3f9a1c7"));
    bad(None, None, None, Some("postmaster"));
    // Over the maximum is clamped, not refused.
    assert!(ListQuery::parse(0, Some("5000"), None, None, None).is_ok());
}

#[test]
fn paging_with_hidden_and_filtered_rows_interleaved() {
    let (store, _, _) = stepped_store("paging");
    let meta = store.create_project().unwrap();
    // 30 letters: every third is private mail for another mailspace, so the
    // viewer sees 20. Pages of 6 must be full until the last.
    let mut visible = Vec::new();
    for n in 1..=30u64 {
        let id = traveling(n);
        let item = if n % 3 == 0 {
            letter(
                &id,
                "alice",
                Some("x@cccccccc.local"),
                None,
                eml(&id, "private"),
            )
        } else {
            visible.push(id.clone());
            letter(&id, "alice", None, Some("need"), eml(&id, "shared"))
        };
        store.append(&meta.project_id, &item).unwrap();
    }
    let carol = viewer("carol");
    let mut cursor = 0;
    let mut seen = Vec::new();
    let mut pages = 0;
    loop {
        let query = ListQuery::parse(cursor, Some("6"), None, None, None).unwrap();
        let (ids, next, more) = page_ids(&store, &meta.project_id, &query, &carol);
        pages += 1;
        if more {
            assert_eq!(ids.len(), 6, "a page is never short because of hidden rows");
        }
        assert!(next >= cursor);
        seen.extend(ids);
        cursor = next;
        if !more {
            break;
        }
    }
    assert_eq!(seen, visible);
    assert_eq!(pages, 4, "20 visible letters in pages of 6");
    // The filtered variant pages the same way: `task` matches nothing.
    let query = ListQuery::parse(0, Some("6"), None, Some("task"), None).unwrap();
    let (ids, next, more) = page_ids(&store, &meta.project_id, &query, &carol);
    assert!(ids.is_empty());
    assert_eq!((next, more), (0, false));
}

#[test]
fn a_page_stops_once_its_letters_exceed_the_byte_cap() {
    let (store, _, _) = stepped_store("byte-cap");
    let meta = store.create_project().unwrap();
    let total = 150u64;
    let mut size = 0;
    for n in 1..=total {
        let id = traveling(n);
        let mut raw = eml(&id, "x");
        raw.resize(60_000, b'x');
        size = raw.len();
        store
            .append(
                &meta.project_id,
                &letter(&id, "hand", None, Some("need"), raw),
            )
            .unwrap();
    }
    let hand = viewer("hand");
    // Default limit (500) is far above the byte-bounded page.
    let query = ListQuery::since(0);
    let first = store.read_page(&meta.project_id, &query, &hand).unwrap();
    let expected = PAGE_BYTE_CAP / size + 1;
    assert_eq!(first.records.len(), expected, "stops once past the cap");
    assert!(first.has_more);
    assert_eq!(first.next_cursor, expected as u64);
    let rest = store
        .read_page(
            &meta.project_id,
            &ListQuery::since(first.next_cursor),
            &hand,
        )
        .unwrap();
    assert_eq!(first.records.len() + rest.records.len(), total as usize);
    assert!(!rest.has_more);
}

#[test]
fn lookup_by_handle_hits_misses_multiples_and_hidden() {
    let (store, _, _) = stepped_store("lookup");
    let meta = store.create_project().unwrap();
    let project = meta.project_id.as_str();
    let work = id_with("4b8275cc", 1);
    let twin_a = id_with("aaaa0001", 1);
    let twin_b = id_with("aaaa0001", 2);
    let private = id_with("deadbeef", 1);
    let appends = [
        letter(&work, "alice", None, Some("need"), eml(&work, "w")),
        letter(&twin_a, "alice", None, Some("task"), eml(&twin_a, "a")),
        letter(&twin_b, "alice", None, Some("want"), eml(&twin_b, "b")),
        letter(
            &private,
            "alice",
            Some("x@dddddddd.local"),
            None,
            eml(&private, "p"),
        ),
    ];
    for item in &appends {
        store.append(project, item).unwrap();
    }
    let handles = |hs: &[&str]| hs.iter().map(|h| h.to_string()).collect::<Vec<_>>();
    let carol = viewer("carol");

    // Hit, case-insensitive; a handle with two ids returns both in sequence
    // order; an unknown and a hidden handle are both missing.
    let found = store
        .lookup_handles(
            project,
            &handles(&["AAAA0001", "4B8275CC", "00000000", "deadbeef", "00000000"]),
            &carol,
        )
        .unwrap();
    let got: Vec<_> = found.records.iter().map(|r| r.message_id.clone()).collect();
    assert_eq!(got, vec![work.clone(), twin_a, twin_b]);
    assert_eq!(found.missing, vec!["00000000", "deadbeef"]);
    assert_eq!(found.records[0].sequence, 1);

    // The sender sees the private one; the recipient prefix's member too.
    for who in [viewer("alice"), viewer_at("dan", "dddddddd")] {
        let found = store
            .lookup_handles(project, &handles(&["deadbeef"]), &who)
            .unwrap();
        assert_eq!(found.records.len(), 1);
        assert!(found.missing.is_empty());
    }

    // Count and shape limits.
    let none: Vec<String> = Vec::new();
    let many: Vec<String> = (0..101).map(|n| format!("{n:08x}")).collect();
    let max: Vec<String> = (0..100).map(|n| format!("{n:08x}")).collect();
    for bad in [
        none,
        many,
        handles(&["xyz"]),
        handles(&["4b8275c"]),
        handles(&["4b8275cc0"]),
        handles(&["4b8275cg"]),
    ] {
        assert!(matches!(
            store.lookup_handles(project, &bad, &carol),
            Err(ProjectError::BadRequest(_))
        ));
    }
    let found = store.lookup_handles(project, &max, &carol).unwrap();
    assert!(found.records.is_empty());
    assert_eq!(found.missing.len(), 100);
}

#[test]
fn the_epoch_is_minted_once_stable_across_reopen_and_distinct_per_project() {
    let data_dir = scratch_data_dir("epoch");
    let store = ProjectStore::new(&data_dir);
    let a = store.create_project().unwrap();
    let b = store.create_project().unwrap();
    let epoch = store.epoch(&a.project_id).unwrap();
    assert!(epoch >= 1);
    assert_eq!(a.epoch, Some(epoch));
    assert_eq!(store.epoch(&a.project_id).unwrap(), epoch);
    assert_ne!(store.epoch(&b.project_id).unwrap(), epoch);
    let reopened = ProjectStore::new(&data_dir);
    assert_eq!(reopened.epoch(&a.project_id).unwrap(), epoch);
    let page = reopened
        .read_page(&a.project_id, &ListQuery::since(0), &viewer("creator"))
        .unwrap();
    assert_eq!(page.epoch, epoch);
}

#[test]
fn a_project_from_before_epochs_gets_one_on_first_use_and_keeps_it() {
    let data_dir = scratch_data_dir("epoch-legacy");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let json_path = project_dir(&data_dir, &meta.project_id).join("project.json");
    std::fs::write(
        &json_path,
        format!(r#"{{"project_id":"{}"}}"#, meta.project_id),
    )
    .unwrap();
    let minted = store.epoch(&meta.project_id).unwrap();
    assert!(minted >= 1);
    assert!(
        std::fs::read_to_string(&json_path)
            .unwrap()
            .contains("epoch")
    );
    assert_eq!(
        ProjectStore::new(&data_dir)
            .epoch(&meta.project_id)
            .unwrap(),
        minted
    );
}
