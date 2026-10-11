//! Integration tests for the phase-03 project store: an append-only log of
//! sealed letters per project, filed under presented traveling ids with a
//! monotonic transport sequence, content-addressed duplicate handling,
//! envelope-row visibility, and crash convergence between blob and index.

use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};

use vivi_server::crypto::StoreCipher;
use vivi_server::projects::{AppendItem, ProjectError, ProjectStore, Viewer};

fn scratch_data_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-projects-u1-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cipher() -> StoreCipher {
    StoreCipher::from_base64(&STANDARD.encode([0x42u8; 32])).unwrap()
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

#[test]
fn created_projects_get_distinct_ids_and_letter_dirs_without_a_mailspace() {
    let data_dir = scratch_data_dir("create");
    let store = ProjectStore::new(&data_dir);
    let a = store.create_project().unwrap();
    let b = store.create_project().unwrap();
    assert_ne!(a.project_id, b.project_id);
    for meta in [&a, &b] {
        let dir = project_dir(&data_dir, &meta.project_id);
        assert!(dir.join("project.json").is_file());
        assert!(dir.join("letters").is_dir());
        assert!(
            !dir.join(".vivi").exists(),
            "no vivi mailspace for projects"
        );
    }
}

#[test]
fn append_assigns_sequence_and_seals_the_letter_whole() {
    let data_dir = scratch_data_dir("append");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let id = traveling(1);
    let item = need(&id, "do the plaintext-marker thing");
    let outcome = store.append(&cipher(), &meta.project_id, &item).unwrap();
    assert_eq!(outcome.sequence, 1);
    assert!(!outcome.duplicate);
    assert_eq!(outcome.message_id, id);
    let hash = sha256_hex(&eml(&id, "do the plaintext-marker thing"));
    assert_eq!(outcome.content_sha256, hash);

    let blob = project_dir(&data_dir, &meta.project_id)
        .join("letters")
        .join(format!("{hash}.sealed"));
    let sealed = std::fs::read(&blob).unwrap();
    assert!(
        !String::from_utf8_lossy(&sealed).contains("plaintext-marker"),
        "blob holds no plaintext"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&blob).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
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
        .append(&cipher(), &meta.project_id, &need(&id, "same body"))
        .unwrap();
    let replay = store
        .append(&cipher(), &meta.project_id, &need(&id, "same body"))
        .unwrap();
    assert!(replay.duplicate);
    assert_eq!(replay.sequence, first.sequence);
    assert_eq!(replay.content_sha256, first.content_sha256);
    // Nothing new was stored: one blob, and the counter did not advance.
    let blobs = std::fs::read_dir(project_dir(&data_dir, &meta.project_id).join("letters"))
        .unwrap()
        .count();
    assert_eq!(blobs, 1);
    let other = store
        .append(&cipher(), &meta.project_id, &need(&traveling(3), "next"))
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
            &cipher(),
            &meta.project_id,
            &letter(&traveling(1), "hand", None, None, bytes.clone()),
        )
        .unwrap();
    let alias = store
        .append(
            &cipher(),
            &meta.project_id,
            &letter(&traveling(9), "hand", None, None, bytes),
        )
        .unwrap();
    assert!(alias.duplicate);
    assert_eq!(alias.message_id, traveling(1));
    assert_eq!(alias.sequence, first.sequence);
    let read = store
        .read_after_scoped(&cipher(), &meta.project_id, 0, &viewer("hand"))
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
            &cipher(),
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
            &cipher(),
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
        .read_after_scoped(&cipher(), &meta.project_id, 0, &viewer("carol"))
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
        .append(&cipher(), &meta.project_id, &need(&id, "original body"))
        .unwrap();
    let err = store
        .append(&cipher(), &meta.project_id, &need(&id, "tampered body"))
        .unwrap_err();
    assert!(matches!(err, ProjectError::Conflict(_)), "got {err:?}");
    let blobs = std::fs::read_dir(project_dir(&data_dir, &meta.project_id).join("letters"))
        .unwrap()
        .count();
    assert_eq!(blobs, 1, "the refused letter left no blob");
    let read = store
        .read_after_scoped(&cipher(), &meta.project_id, 0, &viewer("hand"))
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
        store.append(&cipher(), &meta.project_id, &item).unwrap();
    }

    let all = store
        .read_after_scoped(&cipher(), &meta.project_id, 0, &viewer("hand"))
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
        .read_after_scoped(&cipher(), &meta.project_id, 1, &viewer("hand"))
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
        .append(&cipher(), &meta.project_id, &need(&traveling(5), "one"))
        .unwrap();
    assert_eq!(first.sequence, 1);
    drop(store);
    let reopened = ProjectStore::new(&data_dir);
    let second = reopened
        .append(&cipher(), &meta.project_id, &need(&traveling(6), "two"))
        .unwrap();
    assert_eq!(second.sequence, 2);
    // The pre-restart id still dedups to its stored sequence.
    let replay = reopened
        .append(&cipher(), &meta.project_id, &need(&traveling(5), "one"))
        .unwrap();
    assert!(replay.duplicate);
    assert_eq!(replay.sequence, 1);
    let read = reopened
        .read_after_scoped(&cipher(), &meta.project_id, 0, &viewer("hand"))
        .unwrap();
    assert_eq!(read.records[0].raw_eml, eml(&traveling(5), "one"));
}

/// A crash after the blob write and before the index write leaves an orphan
/// blob and no row; a replay of the append converges on the same state a
/// clean append would have produced.
#[test]
fn orphan_blob_without_index_row_converges_on_replay() {
    let data_dir = scratch_data_dir("orphan");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    store
        .append(&cipher(), &meta.project_id, &need(&traveling(1), "kept"))
        .unwrap();
    let lost = need(&traveling(2), "lost to the crash");
    let outcome = store.append(&cipher(), &meta.project_id, &lost).unwrap();
    assert_eq!(outcome.sequence, 2);

    // Roll the index back to just before the second append: the blob stays.
    let dir = project_dir(&data_dir, &meta.project_id);
    let index_path = dir.join("items.json");
    let mut index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&index_path).unwrap()).unwrap();
    index["records"].as_array_mut().unwrap().truncate(1);
    index["next_sequence"] = 1.into();
    std::fs::write(&index_path, serde_json::to_vec(&index).unwrap()).unwrap();
    assert!(
        dir.join("letters")
            .join(format!("{}.sealed", outcome.content_sha256))
            .is_file()
    );
    let hidden = store
        .read_after_scoped(&cipher(), &meta.project_id, 0, &viewer("hand"))
        .unwrap();
    assert_eq!(hidden.records.len(), 1, "no row, so not served");

    let replay = store.append(&cipher(), &meta.project_id, &lost).unwrap();
    assert!(!replay.duplicate, "the row was never written");
    assert_eq!(replay.sequence, 2);
    assert_eq!(replay.content_sha256, outcome.content_sha256);
    let read = store
        .read_after_scoped(&cipher(), &meta.project_id, 0, &viewer("hand"))
        .unwrap();
    assert_eq!(read.records.len(), 2);
    assert_eq!(
        read.records[1].raw_eml,
        eml(&traveling(2), "lost to the crash")
    );
}

/// An indexed row whose blob is gone is a storage error, never a skip.
#[test]
fn missing_blob_fails_the_read() {
    let data_dir = scratch_data_dir("missing-blob");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    store
        .append(&cipher(), &meta.project_id, &need(&traveling(1), "one"))
        .unwrap();
    let second = store
        .append(&cipher(), &meta.project_id, &need(&traveling(2), "two"))
        .unwrap();
    let letters = project_dir(&data_dir, &meta.project_id).join("letters");
    std::fs::remove_file(letters.join(format!("{}.sealed", second.content_sha256))).unwrap();
    let err = store
        .read_after_scoped(&cipher(), &meta.project_id, 0, &viewer("hand"))
        .unwrap_err();
    assert!(matches!(err, ProjectError::Storage(_)), "got {err:?}");
}

/// A blob that unseals but does not hash to its row (another letter's blob
/// copied over it) is a storage error too.
#[test]
fn swapped_blob_fails_the_hash_check() {
    let data_dir = scratch_data_dir("swapped-blob");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let first = store
        .append(&cipher(), &meta.project_id, &need(&traveling(1), "one"))
        .unwrap();
    let second = store
        .append(&cipher(), &meta.project_id, &need(&traveling(2), "two"))
        .unwrap();
    let letters = project_dir(&data_dir, &meta.project_id).join("letters");
    std::fs::copy(
        letters.join(format!("{}.sealed", second.content_sha256)),
        letters.join(format!("{}.sealed", first.content_sha256)),
    )
    .unwrap();
    let err = store
        .read_after_scoped(&cipher(), &meta.project_id, 0, &viewer("hand"))
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
    let cipher = cipher();
    store
        .append(&cipher, &meta.project_id, &need(&traveling(2), "first in"))
        .unwrap();
    store
        .append(&cipher, &meta.project_id, &need(&traveling(1), "second in"))
        .unwrap();
    let read = store
        .read_after_scoped(&cipher, &meta.project_id, 0, &viewer("hand"))
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
    let cipher = cipher();
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
    store.append(&cipher, &meta.project_id, &first).unwrap();
    // seq 2: unaddressed work item, member-visible by kind.
    store
        .append(&cipher, &meta.project_id, &need(&traveling(21), "a need"))
        .unwrap();
    // seq 3: governance mail to postmaster, member-visible by the carve-out.
    let gov = letter(
        &traveling(22),
        "hand",
        Some("PostMaster"),
        None,
        vouch_eml(&traveling(22), "bob", 2),
    );
    store.append(&cipher, &meta.project_id, &gov).unwrap();
    // seq 4: plain mail alice -> bob again.
    let second = letter(
        &traveling(23),
        "alice",
        Some("bob@aaaaaaaa.local"),
        Some("mail"),
        eml(&traveling(23), "again for bob"),
    );
    store.append(&cipher, &meta.project_id, &second).unwrap();

    let ids = |viewer: Viewer<'_>, since: u64| {
        let page = store
            .read_after_scoped(&cipher, &meta.project_id, since, &viewer)
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

/// A viewer is never served a letter they cannot see: a corrupt blob behind
/// an invisible row does not affect their read.
#[test]
fn invisible_letters_are_not_unsealed() {
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
    let outcome = store.append(&cipher(), &meta.project_id, &private).unwrap();
    std::fs::write(
        project_dir(&data_dir, &meta.project_id)
            .join("letters")
            .join(format!("{}.sealed", outcome.content_sha256)),
        b"not a sealed letter",
    )
    .unwrap();
    let carol = store
        .read_after_scoped(&cipher(), &meta.project_id, 0, &viewer("carol"))
        .unwrap();
    assert!(carol.records.is_empty());
    assert!(
        store
            .read_after_scoped(
                &cipher(),
                &meta.project_id,
                0,
                &viewer_at("bob", "aaaaaaaa")
            )
            .is_err()
    );
}

/// A fresh append wakes a waiter on that project.
#[tokio::test]
async fn append_wakes_waiter() {
    let data_dir = scratch_data_dir("wake");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let cipher = cipher();
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
        store.append(&cipher, &meta.project_id, &need(&traveling(9), "wake"))
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
    let cipher = cipher();
    let item = need(&traveling(9), "wake");
    store.append(&cipher, &meta.project_id, &item).unwrap();
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
    let replay =
        tokio::task::spawn_blocking(move || store.append(&cipher, &meta.project_id, &item))
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
    store.append(&cipher(), &meta.project_id, &stamped).unwrap();
    store
        .append(&cipher(), &meta.project_id, &need(&traveling(2), "plain"))
        .unwrap();
    let read = store
        .read_after_scoped(&cipher(), &meta.project_id, 0, &viewer("hand"))
        .unwrap();
    assert_eq!(read.records[0].from_prefix.as_deref(), Some("3f9a1c7e"));
    assert_eq!(read.records[1].from_prefix, None);
}
