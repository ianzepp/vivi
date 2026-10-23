use super::*;

use crate::mailspace::{Mailspace, SendRequest};
use std::fs;

#[test]
fn report_read_returns_unavailable_when_wal_shm_is_missing_without_creating_it() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(".vivi");
    drop(Storage::open_mailspace(&dir).unwrap());
    let db = dir.join("mail.sqlite");
    let shm = dir.join("mail.sqlite-shm");
    let wal = dir.join("mail.sqlite-wal");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(conn);
    let _ = fs::remove_file(&shm);
    let _ = fs::remove_file(&wal);
    let before = fs::read(&db).unwrap();

    let error = read(tmp.path()).err().unwrap().to_string();
    assert!(error.contains("mail.sqlite-shm"), "{error}");
    assert!(
        !shm.exists(),
        "the report reader created the missing sidecar"
    );
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[test]
fn reopening_a_need_resets_its_active_lead_time_window() {
    let temp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(temp.path())).unwrap();
    space.add_identity("mind").unwrap();
    let sent = space
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "A need".into(),
            body: crate::letter::fixture::need_body(&["done"], "do it"),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap();
    let need = sent.delivered[0].handle.clone();
    let initial = need_window(&space.storage().unwrap(), &need).unwrap();
    space
        .move_item("mind", &need, "done", None, "need done", None)
        .unwrap();
    space
        .move_item("mind", &need, "needs", None, "need reopen", None)
        .unwrap();

    let reopened = need_window(&space.storage().unwrap(), &need).unwrap();
    let reopen_at = {
        let storage = space.storage().unwrap();
        let id = storage.resolve_message_token(&need).unwrap();
        storage
            .list_mailspace_events(&id)
            .unwrap()
            .iter()
            .rev()
            .find(|event| event.command == "need reopen")
            .and_then(|event| crate::pump::attempt_read::opened_epoch(&event.occurred_at))
    };
    assert_eq!(reopened.opened_at, reopen_at);
    assert!(reopened.opened_at >= initial.opened_at);
    assert!(!reopened.done);
    assert_eq!(reopened.done_at, None);

    space
        .move_item("mind", &need, "done", None, "need done", None)
        .unwrap();
    let completed = need_window(&space.storage().unwrap(), &need).unwrap();
    assert!(completed.done);
    assert_eq!(completed.opened_at, reopened.opened_at);
    assert!(completed.done_at.is_some());
}

#[test]
fn a_reopen_without_a_valid_timestamp_has_unavailable_lead_time() {
    let temp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(temp.path())).unwrap();
    space.add_identity("mind").unwrap();
    let sent = space
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "A need".into(),
            body: crate::letter::fixture::need_body(&["done"], "do it"),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap();
    let need = sent.delivered[0].handle.clone();
    space
        .move_item("mind", &need, "done", None, "need done", None)
        .unwrap();
    space
        .move_item("mind", &need, "needs", None, "need reopen", None)
        .unwrap();
    let reopen_event_id = {
        let storage = space.storage().unwrap();
        let id = storage.resolve_message_token(&need).unwrap();
        storage
            .list_mailspace_events(&id)
            .unwrap()
            .into_iter()
            .find(|event| event.command == "need reopen")
            .unwrap()
            .event_id
    };
    let connection = rusqlite::Connection::open(space.dir.join("mail.sqlite")).unwrap();
    connection
        .execute(
            "UPDATE mailspace_events SET occurred_at = 'invalid' WHERE event_id = ?1",
            [reopen_event_id],
        )
        .unwrap();
    drop(connection);

    let reopened = need_window(&space.storage().unwrap(), &need).unwrap();
    assert!(!reopened.done);
    assert_eq!(reopened.opened_at, None);
    assert_eq!(reopened.done_at, None);
}
