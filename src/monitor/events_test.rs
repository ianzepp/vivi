use super::*;
use crate::mailspace::{Mailspace, SendRequest};
use std::fs;

const DISPATCH: &str = r#"{"pool":"here","need":"need1234","kind":"fix","slot":"vivarium-here-slot-1","machine":"burgus","job":"3cc45261-3-a1","model":"zai/glm-5.3-flash","from":"mind","branch":"unit/3cc45261-3-a1","start":"1792421950abc","budget":1800,"at":1792421950,"read_only":false}"#;
const COLLECT: &str = r#"{"job":"3cc45261-3-a1","pool":"here","slot":"vivarium-here-slot-1","outcome":"failed","agent_rc":"1"}"#;

fn space_with_task() -> (tempfile::TempDir, Mailspace, String) {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(tmp.path())).unwrap();
    space.add_identity("mind").unwrap();
    space.add_identity("hand").unwrap();
    let sent = space
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Fix the edge".into(),
            body: crate::letter::fixture::task_body_with(
                &crate::letter::work::TaskFront {
                    from: Some("main".into()),
                    branch: Some("factory/x".into()),
                    ..Default::default()
                },
                "do it\n",
            ),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap();
    let handle = sent.delivered[0].handle.clone();
    (tmp, space, handle)
}

#[test]
fn a_dispatched_task_reads_open_with_task_info() {
    let (_tmp, space, handle) = space_with_task();
    space
        .record_pool_note(&handle, "pool_dispatched", DISPATCH, "dispatched")
        .unwrap();
    let events = read(&space.dir.join("mail.sqlite"), 8).unwrap();
    assert_eq!(events.open.len(), 1);
    assert_eq!(events.open[0].task, handle);
    assert_eq!(events.open[0].record.model, "zai/glm-5.3-flash");
    assert_eq!(events.open[0].record.pool, "here");
    assert_eq!(events.open[0].record.need.as_deref(), Some("need1234"));
    assert_eq!(events.open[0].record.kind.as_deref(), Some("fix"));
    assert_eq!(events.open[0].record.slot, "vivarium-here-slot-1");
    let info = events.task_info.get(&handle).unwrap();
    assert_eq!(info.subject, "Fix the edge");
    assert_eq!(info.role, "hand");
    assert!(events.finished.is_empty());
}

#[test]
fn a_legacy_dispatch_uses_only_a_recorded_need_source_handle() {
    let (_tmp, space, handle) = space_with_task();
    let mut record: serde_json::Value = serde_json::from_str(DISPATCH).unwrap();
    record.as_object_mut().unwrap().remove("need");
    record.as_object_mut().unwrap().remove("kind");
    space
        .record_pool_note(
            &handle,
            "pool_dispatched",
            &record.to_string(),
            "dispatched",
        )
        .unwrap();
    let storage = space.storage().unwrap();
    let id = storage.resolve_message_token(&handle).unwrap();
    storage
        .set_item_metadata(
            &id,
            &std::collections::BTreeMap::from([
                ("source_handle".into(), "need5678".into()),
                ("source_kind".into(), "need".into()),
            ]),
        )
        .unwrap();
    drop(storage);

    let events = read(&space.dir.join("mail.sqlite"), 8).unwrap();
    assert_eq!(events.open[0].record.need, None);
    assert_eq!(events.open[0].record.kind, None);
    assert_eq!(events.task_info[&handle].need.as_deref(), Some("need5678"));
    assert_eq!(events.task_info[&handle].kind, None);
}

#[test]
fn a_collected_dispatch_reads_finished_with_result_keys() {
    let (_tmp, space, handle) = space_with_task();
    space
        .record_pool_note(&handle, "pool_dispatched", DISPATCH, "dispatched")
        .unwrap();
    space
        .record_pool_note(&handle, "pool_collected", COLLECT, "failed")
        .unwrap();
    let events = read(&space.dir.join("mail.sqlite"), 8).unwrap();
    assert!(events.open.is_empty(), "the collect closes the dispatch");
    assert_eq!(events.finished.len(), 1);
    let done = &events.finished[0];
    assert_eq!(done.task, handle);
    assert_eq!(done.subject, "Fix the edge");
    assert_eq!(done.role, "hand");
    assert!(done.outcome.contains("failed"), "{}", done.outcome);
    assert!(done.outcome.contains("agent_rc=1"), "{}", done.outcome);
    assert_eq!(done.model, "zai/glm-5.3-flash");
    assert_eq!(done.pool, "here");
    assert_eq!(done.need.as_deref(), Some("need1234"));
    assert_eq!(done.kind.as_deref(), Some("fix"));
    assert_eq!(done.slot, "vivarium-here-slot-1");
    assert!(!done.read_only);
    assert!(!done.ended_at.is_empty());
}

#[test]
fn read_missing_db_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let events = read(&tmp.path().join("absent.sqlite"), 8).unwrap();
    assert!(events.open.is_empty());
    assert!(events.task_info.is_empty());
    assert!(events.finished.is_empty());
}

/// Record a legacy dispatch (need/kind stripped) plus recorded need
/// metadata on `handle`. Every writer stays open; close them before reading.
fn record_legacy_dispatch_with_need_metadata(
    space: &Mailspace,
    handle: &str,
) -> std::path::PathBuf {
    let db = space.dir.join("mail.sqlite");
    let mut record: serde_json::Value = serde_json::from_str(DISPATCH).unwrap();
    record.as_object_mut().unwrap().remove("need");
    record.as_object_mut().unwrap().remove("kind");
    space
        .record_pool_note(handle, "pool_dispatched", &record.to_string(), "dispatched")
        .unwrap();
    let storage = space.storage().unwrap();
    let id = storage.resolve_message_token(handle).unwrap();
    storage
        .set_item_metadata(
            &id,
            &std::collections::BTreeMap::from([
                ("source_handle".into(), "need5678".into()),
                ("source_kind".into(), "need".into()),
            ]),
        )
        .unwrap();
    drop(storage);
    db
}

#[test]
fn owner_reads_dispatch_without_shm() {
    let (_tmp, space, handle) = space_with_task();
    let db = record_legacy_dispatch_with_need_metadata(&space, &handle);
    let shm = db.with_file_name("mail.sqlite-shm");
    let wal = db.with_file_name("mail.sqlite-wal");
    drop(space);
    // No writer is live from here on: fold the WAL into the main database,
    // then start the owner read without a WAL index. The index is never
    // removed beneath a live writer.
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(conn);
    let _ = fs::remove_file(&shm);
    let _ = fs::remove_file(&wal);
    assert!(
        !shm.exists(),
        "the closed store must start without an index"
    );
    let before = fs::read(&db).unwrap();

    let events = read(&db, 8).unwrap();
    assert_eq!(events.open.len(), 1);
    let record = &events.open[0].record;
    assert_eq!(record.need, None, "the legacy record carries no need");
    assert_eq!(record.kind, None, "the legacy record carries no kind");
    assert_eq!(record.pool, "here");
    assert_eq!(record.slot, "vivarium-here-slot-1");
    assert_eq!(record.model, "zai/glm-5.3-flash");
    assert_eq!(
        events.task_info[&handle].need.as_deref(),
        Some("need5678"),
        "the fallback supplies the recorded need"
    );
    assert_eq!(
        fs::read(&db).unwrap(),
        before,
        "the owner read changed the database"
    );
}

#[test]
fn a_non_file_database_path_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("mail.sqlite");
    fs::create_dir(&db).unwrap();
    let error = read(&db, 8).err().unwrap().to_string();
    assert!(error.contains("not a regular file"), "{error}");
}

#[test]
fn an_unreadable_database_header_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("mail.sqlite");
    fs::write(&db, b"SQLite").unwrap();
    let error = read(&db, 8).err().unwrap().to_string();
    assert!(error.contains("could not read the header"), "{error}");
    assert_eq!(fs::read(&db).unwrap(), b"SQLite");
}

#[cfg(unix)]
#[test]
fn an_uninspectable_database_path_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let blocker = tmp.path().join("blocker");
    fs::write(&blocker, b"not a directory").unwrap();
    let db = blocker.join("mail.sqlite");
    let error = read(&db, 8).err().unwrap().to_string();
    assert!(
        error.contains("read-only inspection unavailable"),
        "{error}"
    );
}
