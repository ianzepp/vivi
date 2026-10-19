use super::*;
use crate::mailspace::{Mailspace, SendRequest};
use crate::storage::Storage;
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
            body: "do it\nfrom: main\nbranch: factory/x\n".into(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
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

#[test]
fn read_only_wal_events_skip_when_shm_is_missing_without_creating_it() {
    let tmp = tempfile::tempdir().unwrap();
    let _storage = Storage::open_mailspace(tmp.path()).unwrap();
    drop(_storage);
    let db = tmp.path().join("mail.sqlite");
    let shm = tmp.path().join("mail.sqlite-shm");
    let wal = tmp.path().join("mail.sqlite-wal");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(conn);
    let _ = fs::remove_file(&shm);
    let _ = fs::remove_file(&wal);
    let before = fs::read(&db).unwrap();

    let error = read(&db, 8).err().unwrap().to_string();
    assert!(error.contains("mail.sqlite-shm"), "{error}");
    assert!(!shm.exists(), "the monitor created the missing sidecar");
    assert_eq!(fs::read(&db).unwrap(), before);
}
