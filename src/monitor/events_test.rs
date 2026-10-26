use super::*;
use crate::mailspace::{Mailspace, SendRequest};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

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
    assert_eq!(
        fs::read_dir(tmp.path()).unwrap().count(),
        0,
        "an absent database must not be created nor sidecarred"
    );
}

#[test]
fn read_refuses_a_non_file_database_path_without_creating_anything() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = tmp.path().join("mail.sqlite");
    fs::create_dir(&fake).unwrap();
    let error = read(&fake, 8).err().unwrap().to_string();
    assert!(error.contains("not a file"), "{error}");
    assert_eq!(
        fs::read_dir(tmp.path()).unwrap().count(),
        1,
        "nothing may be created beside the non-file path"
    );
}

#[test]
#[cfg(unix)]
fn read_refuses_an_unreadable_database_without_creating_anything() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("mail.sqlite");
    fs::write(&db, b"not even a header").unwrap();
    fs::set_permissions(&db, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(
        read(&db, 8).is_err(),
        "an unreadable database is an error, not empty history"
    );
    assert_eq!(
        fs::read_dir(tmp.path()).unwrap().count(),
        1,
        "the refusal must create no database or sidecar"
    );
}

/// Copy a quiescent mailspace database and its committed WAL into a fresh
/// isolated directory, without the writer's `-shm` index. The writer is
/// still open while the copies are taken; nothing live is unlinked, and the
/// source directory is abandoned whole.
fn wal_only_copy(space: &Mailspace) -> (tempfile::TempDir, std::path::PathBuf) {
    let source_db = space.dir.join("mail.sqlite");
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("mail.sqlite");
    fs::copy(&source_db, &db).unwrap();
    fs::copy(
        source_db.with_file_name("mail.sqlite-wal"),
        dir.path().join("mail.sqlite-wal"),
    )
    .unwrap();
    assert!(
        !dir.path().join("mail.sqlite-shm").exists(),
        "the copy must not carry the writer's -shm index"
    );
    (dir, db)
}

/// The copied pair must carry `marker` only in the WAL: never checkpointed
/// into the copied database bytes.
fn assert_marker_is_wal_only(db: &std::path::Path, marker: &str) {
    let db_bytes = fs::read(db).unwrap();
    let wal_bytes = fs::read(db.with_file_name("mail.sqlite-wal")).unwrap();
    assert!(
        !contains(&db_bytes, marker),
        "{marker} must not be checkpointed into the copied database"
    );
    assert!(
        contains(&wal_bytes, marker),
        "{marker} must live in the copied WAL"
    );
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// The owner read succeeds where the strict policy refused: a dispatch
/// committed only in the copied WAL is observed, through a sidecar the open
/// itself recreated under this process's ownership.
#[test]
#[cfg(unix)]
fn owner_read_observes_dispatch_committed_only_in_the_copied_wal() {
    let (_tmp, space, handle) = space_with_task();
    space
        .record_pool_note(&handle, "pool_dispatched", DISPATCH, "dispatched")
        .unwrap();
    let (copy_dir, db) = wal_only_copy(&space);
    assert_marker_is_wal_only(&db, "need1234");
    drop(space);

    let events = read(&db, 8).unwrap();
    assert_eq!(
        events.open.len(),
        1,
        "the WAL-only dispatch must be observed"
    );
    assert_eq!(events.open[0].task, handle);
    assert_eq!(events.open[0].record.need.as_deref(), Some("need1234"));
    assert_eq!(events.open[0].record.model, "zai/glm-5.3-flash");
    assert!(
        copy_dir.path().join("mail.sqlite-shm").exists(),
        "the owned read recreated the -shm sidecar"
    );
}

/// The legacy-metadata fallback runs for an owner with no other reader
/// maintaining `-shm`: the recorded `source_kind=need`/`source_handle`
/// parent resolves out of WAL-backed metadata, and the database and WAL
/// bytes are unchanged.
#[test]
#[cfg(unix)]
fn owner_metadata_fallback_reads_wal_without_shm() {
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
                ("source_handle".to_string(), "needwal99".to_string()),
                ("source_kind".to_string(), "need".to_string()),
            ]),
        )
        .unwrap();

    let (copy_dir, db) = wal_only_copy(&space);
    assert_marker_is_wal_only(&db, "needwal99");
    drop(storage);
    drop(space);

    let before_db = fs::read(&db).unwrap();
    let before_wal = fs::read(db.with_file_name("mail.sqlite-wal")).unwrap();

    let mut task_info = std::collections::BTreeMap::from([(
        handle.clone(),
        TaskInfo {
            subject: "Fix the edge".into(),
            role: "hand".into(),
            ..Default::default()
        },
    )]);
    add_task_need_metadata(&mut task_info, copy_dir.path()).unwrap();

    assert_eq!(
        task_info[&handle].need.as_deref(),
        Some("needwal99"),
        "the WAL-backed metadata parent must resolve"
    );
    assert_eq!(
        fs::read(&db).unwrap(),
        before_db,
        "database bytes must be unchanged"
    );
    assert_eq!(
        fs::read(db.with_file_name("mail.sqlite-wal")).unwrap(),
        before_wal,
        "WAL bytes must be unchanged"
    );
    assert!(
        copy_dir.path().join("mail.sqlite-shm").exists(),
        "the owned fallback recreated the -shm sidecar"
    );
}
