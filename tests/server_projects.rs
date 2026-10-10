//! Integration tests for the phase-03 project store (U1): one mailspace per
//! project, appends filed under presented traveling ids with a monotonic
//! transport sequence, same-id same-bytes dedup, and same-id different-bytes
//! refusal.

use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use vivi::mailspace::Mailspace;

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
    format!("msg_{n:024x}")
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
