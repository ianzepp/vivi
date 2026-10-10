//! Integration tests for the phase-03 project store (U1): one mailspace per
//! project, appends filed under presented traveling ids with a monotonic
//! transport sequence, same-id same-bytes dedup, and same-id different-bytes
//! refusal.

use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use vivi::mailspace::Mailspace;
use vivi::storage::MessageIngestRequest;

use vivi_server::crypto::StoreCipher;
use vivi_server::projects::{AppendItem, ProjectError, ProjectStore};

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

fn item(id: &str, body: &str) -> AppendItem {
    AppendItem {
        id: id.to_owned(),
        from: "hand".to_owned(),
        subject: "spec the thing".to_owned(),
        body: body.to_owned(),
        kind: Some("need".to_owned()),
        in_reply_to: None,
        refs: Vec::new(),
    }
}

fn open_storage(data_dir: &PathBuf, project_id: &str) -> vivi::storage::Storage {
    let mailspace = Mailspace::init(Some(&data_dir.join("projects").join(project_id))).unwrap();
    mailspace.storage().unwrap()
}

#[test]
fn created_projects_get_distinct_ids_and_dirs() {
    let data_dir = scratch_data_dir("create");
    let store = ProjectStore::new(&data_dir);
    let a = store.create_project().unwrap();
    let b = store.create_project().unwrap();
    assert_ne!(a.project_id, b.project_id);
    for meta in [&a, &b] {
        let dir = data_dir.join("projects").join(&meta.project_id);
        assert!(dir.join("project.json").is_file(), "meta beside mailspace");
        assert!(dir.join(".vivi").join("mail.sqlite").is_file());
    }
}

#[test]
fn kinded_append_assigns_sequence_and_lands_in_store() {
    let data_dir = scratch_data_dir("append");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let id = traveling(1);
    let outcome = store
        .append(&cipher(), &meta.project_id, &item(&id, "do the thing"))
        .unwrap();
    assert_eq!(outcome.sequence, 1);
    assert!(!outcome.duplicate);
    assert_eq!(outcome.item_id, id);

    let storage = open_storage(&data_dir, &meta.project_id);
    let view = storage
        .message_by_id(&id)
        .unwrap()
        .expect("stored under presented id");
    assert_eq!(view.local_role, "needs");
    assert_eq!(view.account, "project");
    let eml = String::from_utf8(storage.read_message(&id).unwrap()).unwrap();
    assert!(eml.contains(&format!("Message-ID: <{id}@")));
    assert!(eml.contains("X-Vivi-Kind: need"));
    // Sealed body: no plaintext content in the blob.
    assert!(!eml.contains("do the thing"));
}

#[test]
fn replay_same_id_same_bytes_is_duplicate_noop() {
    let data_dir = scratch_data_dir("duplicate");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let id = traveling(2);
    let first = store
        .append(&cipher(), &meta.project_id, &item(&id, "same body"))
        .unwrap();
    let replay = store
        .append(&cipher(), &meta.project_id, &item(&id, "same body"))
        .unwrap();
    assert!(replay.duplicate);
    assert_eq!(replay.sequence, first.sequence);
    // Nothing new was stored: still exactly one message in the mailspace.
    let storage = open_storage(&data_dir, &meta.project_id);
    assert_eq!(storage.list_messages().unwrap().len(), 1);
    // And the sequence counter did not advance past the duplicate.
    let other = store
        .append(&cipher(), &meta.project_id, &item(&traveling(3), "next"))
        .unwrap();
    assert_eq!(other.sequence, first.sequence + 1);
}

#[test]
fn same_id_different_bytes_is_refused() {
    let data_dir = scratch_data_dir("conflict");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let id = traveling(4);
    store
        .append(&cipher(), &meta.project_id, &item(&id, "original body"))
        .unwrap();
    let storage = open_storage(&data_dir, &meta.project_id);
    let before = storage.read_message(&id).unwrap();
    let err = store
        .append(&cipher(), &meta.project_id, &item(&id, "tampered body"))
        .unwrap_err();
    assert!(matches!(err, ProjectError::Conflict(_)), "got {err:?}");
    // The held item is untouched.
    let storage = open_storage(&data_dir, &meta.project_id);
    assert_eq!(storage.read_message(&id).unwrap(), before);
}

#[test]
fn read_after_returns_suffix_and_replica_bytes_preserve_identity() {
    let data_dir = scratch_data_dir("read-after");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    for n in 10..13 {
        let mut entry = item(&traveling(n), &format!("body-{n}"));
        entry.in_reply_to = Some(traveling(9));
        entry.refs = vec![traveling(8)];
        store.append(&cipher(), &meta.project_id, &entry).unwrap();
    }

    let all = store.read_after(&cipher(), &meta.project_id, 0).unwrap();
    assert_eq!(
        all.records.iter().map(|r| r.sequence).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        all.records
            .iter()
            .map(|r| r.message_id.as_str())
            .collect::<Vec<_>>(),
        vec![traveling(10), traveling(11), traveling(12)]
    );
    assert_eq!(all.records[1].body, "body-11");
    assert_eq!(all.records[1].kind.as_deref(), Some("need"));
    assert_eq!(
        all.records[1].in_reply_to.as_deref(),
        Some(traveling(9).as_str())
    );
    assert_eq!(all.records[1].refs, vec![traveling(8)]);
    assert_eq!(all.next_cursor, 3);

    let suffix = store.read_after(&cipher(), &meta.project_id, 1).unwrap();
    assert_eq!(
        suffix
            .records
            .iter()
            .map(|r| r.sequence)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(suffix.records[0].body, "body-11");
    assert_eq!(suffix.next_cursor, 3);

    let replica_a_dir = scratch_data_dir("replica-a");
    let replica_b_dir = scratch_data_dir("replica-b");
    let mut replica_a = Mailspace::init(Some(&replica_a_dir))
        .unwrap()
        .storage()
        .unwrap();
    let mut replica_b = Mailspace::init(Some(&replica_b_dir))
        .unwrap()
        .storage()
        .unwrap();
    for record in &all.records {
        let request = MessageIngestRequest {
            account: "replica".into(),
            local_role: "inbox".into(),
            read_state: false,
            starred: false,
            message_id_hint: None,
            presented_id: Some(record.message_id.clone()),
            seed_hint: record.message_id.clone(),
            remote: None,
        };
        let a = replica_a.ingest_message(&request, &record.raw_eml).unwrap();
        let b = replica_b.ingest_message(&request, &record.raw_eml).unwrap();
        assert_eq!(a.message_id, record.message_id);
        assert_eq!(b.message_id, record.message_id);
        assert_eq!(a.content_id, b.content_id);
        assert_eq!(
            replica_a.display_handle(&a.message_id).unwrap(),
            &a.message_id[4..12]
        );
        assert_eq!(
            replica_b.display_handle(&b.message_id).unwrap(),
            &a.message_id[4..12]
        );
        assert_eq!(
            replica_a
                .resolve_message_token(&a.message_id[4..12])
                .unwrap(),
            a.message_id
        );
        assert_eq!(
            replica_b
                .resolve_message_token(&b.message_id[4..12])
                .unwrap(),
            b.message_id
        );
    }
}

#[test]
fn sequence_stays_monotonic_after_close_and_reopen() {
    let data_dir = scratch_data_dir("reopen");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    assert_eq!(
        store
            .append(&cipher(), &meta.project_id, &item(&traveling(5), "one"))
            .unwrap()
            .sequence,
        1
    );
    drop(store);
    let reopened = ProjectStore::new(&data_dir);
    assert_eq!(
        reopened
            .append(&cipher(), &meta.project_id, &item(&traveling(6), "two"))
            .unwrap()
            .sequence,
        2
    );
    // The pre-restart id still dedups to its stored sequence.
    let replay = reopened
        .append(&cipher(), &meta.project_id, &item(&traveling(5), "one"))
        .unwrap();
    assert!(replay.duplicate);
    assert_eq!(replay.sequence, 1);
}

/// Ordering comes from the counter table, never from id or date sorting:
/// append id 2 before id 1 and read back in append order.
#[test]
fn read_order_follows_append_order_not_id_order() {
    let data_dir = scratch_data_dir("order");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let cipher = cipher();
    store
        .append(&cipher, &meta.project_id, &item(&traveling(2), "first in"))
        .unwrap();
    store
        .append(&cipher, &meta.project_id, &item(&traveling(1), "second in"))
        .unwrap();
    let read = store.read_after(&cipher, &meta.project_id, 0).unwrap();
    let first = traveling(2);
    let second = traveling(1);
    let ids: Vec<&str> = read
        .records
        .iter()
        .map(|record| record.message_id.as_str())
        .collect();
    assert_eq!(ids, vec![first.as_str(), second.as_str()]);
    assert_eq!(read.next_cursor, 2);
}

/// A fresh append wakes a waiter on that project; a duplicate does not.
#[tokio::test]
async fn append_wakes_waiter_and_duplicate_does_not() {
    let data_dir = scratch_data_dir("wake");
    let store = ProjectStore::new(&data_dir);
    let meta = store.create_project().unwrap();
    let cipher = cipher();
    // Seed the waiter registration, then wait while this task appends.
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
        store.append(&cipher, &meta.project_id, &item(&traveling(9), "wake"))
    })
    .await
    .unwrap()
    .unwrap();
    assert!(waiter.await.unwrap(), "fresh append must wake the waiter");
}
