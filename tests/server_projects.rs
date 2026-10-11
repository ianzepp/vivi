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

// ---- append time, paged and filtered list, lookup by handle ----

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use chrono::{TimeZone, Utc};
use vivi_server::projects::{ListQuery, PAGE_BYTE_CAP, monotone_stamp};

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
    let page = store.read_page(&cipher(), project, query, who).unwrap();
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
    store.append(&cipher(), &meta.project_id, &first).unwrap();
    // The clock steps backward: the stamp holds at the previous value.
    clock.store(400, Ordering::SeqCst);
    store
        .append(&cipher(), &meta.project_id, &need(&traveling(2), "second"))
        .unwrap();
    clock.store(2_000, Ordering::SeqCst);
    store
        .append(&cipher(), &meta.project_id, &need(&traveling(3), "third"))
        .unwrap();
    // A replay (same id, same bytes) changes nothing, whatever the clock says.
    clock.store(9_000, Ordering::SeqCst);
    let replay = store.append(&cipher(), &meta.project_id, &first).unwrap();
    assert!(replay.duplicate);

    let page = store
        .read_after_scoped(&cipher(), &meta.project_id, 0, &hand)
        .unwrap();
    let stamps: Vec<_> = page
        .records
        .iter()
        .map(|r| r.appended_at.clone().unwrap())
        .collect();
    assert_eq!(
        stamps,
        vec![stamp_of(1_000), stamp_of(1_000), stamp_of(2_000)]
    );
    assert_eq!(page.records.len(), 3, "the replay stored no row");
}

#[test]
fn an_index_without_appended_at_loads_and_after_excludes_those_rows() {
    let (store, clock, data_dir) = stepped_store("old-index");
    let meta = store.create_project().unwrap();
    clock.store(5_000, Ordering::SeqCst);
    for n in 1..=2 {
        store
            .append(&cipher(), &meta.project_id, &need(&traveling(n), "old"))
            .unwrap();
    }
    // Rewrite the index as it was before stamps existed.
    let path = project_dir(&data_dir, &meta.project_id).join("items.json");
    let mut index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for row in index["records"].as_array_mut().unwrap() {
        assert!(row.as_object_mut().unwrap().remove("appended_at").is_some());
    }
    std::fs::write(&path, serde_json::to_vec(&index).unwrap()).unwrap();

    let hand = viewer("hand");
    let page = store
        .read_after_scoped(&cipher(), &meta.project_id, 0, &hand)
        .unwrap();
    assert_eq!(page.records.len(), 2);
    assert!(page.records.iter().all(|r| r.appended_at.is_none()));

    // A new row is stamped; an `after` window keeps only stamped rows.
    clock.store(6_000, Ordering::SeqCst);
    store
        .append(&cipher(), &meta.project_id, &need(&traveling(3), "new"))
        .unwrap();
    let after = ListQuery::parse(0, None, Some("1970-01-01T00:00:00Z"), None, None).unwrap();
    let (ids, cursor, more) = page_ids(&store, &meta.project_id, &after, &hand);
    assert_eq!(ids, vec![traveling(3)]);
    assert_eq!((cursor, more), (3, false));
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
        store.append(&cipher(), &meta.project_id, &item).unwrap();
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
        store.append(&cipher(), &meta.project_id, &item).unwrap();
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
                &cipher(),
                &meta.project_id,
                &letter(&id, "hand", None, Some("need"), raw),
            )
            .unwrap();
    }
    let hand = viewer("hand");
    // Default limit (500) is far above the byte-bounded page.
    let query = ListQuery::since(0);
    let first = store
        .read_page(&cipher(), &meta.project_id, &query, &hand)
        .unwrap();
    let expected = PAGE_BYTE_CAP / size + 1;
    assert_eq!(first.records.len(), expected, "stops once past the cap");
    assert!(first.has_more);
    assert_eq!(first.next_cursor, expected as u64);
    let rest = store
        .read_page(
            &cipher(),
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
        store.append(&cipher(), project, item).unwrap();
    }
    let handles = |hs: &[&str]| hs.iter().map(|h| h.to_string()).collect::<Vec<_>>();
    let carol = viewer("carol");

    // Hit, case-insensitive; a handle with two ids returns both in sequence
    // order; an unknown and a hidden handle are both missing.
    let found = store
        .lookup_handles(
            &cipher(),
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
            .lookup_handles(&cipher(), project, &handles(&["deadbeef"]), &who)
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
            store.lookup_handles(&cipher(), project, &bad, &carol),
            Err(ProjectError::BadRequest(_))
        ));
    }
    let found = store
        .lookup_handles(&cipher(), project, &max, &carol)
        .unwrap();
    assert!(found.records.is_empty());
    assert_eq!(found.missing.len(), 100);
}
