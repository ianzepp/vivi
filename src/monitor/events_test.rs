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

fn send_task(space: &mut Mailspace, subject: &str) -> String {
    let sent = space
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: crate::letter::fixture::task_body_with(
                &crate::letter::work::TaskFront::default(),
                "do it\n",
            ),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap();
    sent.delivered[0].handle.clone()
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn owner_reads_wal_events_without_shm() {
    let source = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(source.path())).unwrap();
    space.add_identity("mind").unwrap();
    space.add_identity("hand").unwrap();
    let marked = send_task(&mut space, "Fix the edge");
    let legacy = send_task(&mut space, "Mend the seam");
    let mut legacy_record: serde_json::Value = serde_json::from_str(DISPATCH).unwrap();
    legacy_record.as_object_mut().unwrap().remove("need");
    legacy_record.as_object_mut().unwrap().remove("kind");
    space
        .record_pool_note(&marked, "pool_dispatched", DISPATCH, "dispatched")
        .unwrap();
    space
        .record_pool_note(
            &legacy,
            "pool_dispatched",
            &legacy_record.to_string(),
            "dispatched",
        )
        .unwrap();
    {
        let storage = space.storage().unwrap();
        let id = storage.resolve_message_token(&legacy).unwrap();
        storage
            .set_item_metadata(
                &id,
                &std::collections::BTreeMap::from([
                    ("source_handle".into(), "need5678".into()),
                    ("source_kind".into(), "need".into()),
                ]),
            )
            .unwrap();
    }

    // Copy the stable database and its committed WAL into an isolated
    // directory without the shared-memory sidecar while the mailspace's
    // cached connection is still open, so no close-time checkpoint can move
    // the dispatches out of the WAL.
    let source_db = space.dir.join("mail.sqlite");
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("mail.sqlite");
    fs::copy(&source_db, &db).unwrap();
    fs::copy(
        space.dir.join("mail.sqlite-wal"),
        tmp.path().join("mail.sqlite-wal"),
    )
    .unwrap();
    drop(space);
    let wal_bytes = fs::read(tmp.path().join("mail.sqlite-wal")).unwrap();
    assert!(
        contains_bytes(&wal_bytes, b"need1234"),
        "the dispatch must be committed only in the copied WAL"
    );
    assert!(
        !contains_bytes(&fs::read(&db).unwrap(), b"need1234"),
        "the dispatch must not be checkpointed into the copied database"
    );
    assert!(
        !tmp.path().join("mail.sqlite-shm").exists(),
        "the fixture ships without the shared-memory sidecar"
    );
    let before = fs::read(&db).unwrap();

    let events = read(&db, 8).unwrap();
    assert_eq!(events.open.len(), 2);
    let open_marked = events.open.iter().find(|d| d.task == marked).unwrap();
    assert_eq!(open_marked.record.need.as_deref(), Some("need1234"));
    assert_eq!(open_marked.record.kind.as_deref(), Some("fix"));
    let open_legacy = events.open.iter().find(|d| d.task == legacy).unwrap();
    assert_eq!(open_legacy.record.need, None);
    assert_eq!(open_legacy.record.kind, None);
    assert_eq!(events.task_info[&marked].need.as_deref(), Some("need1234"));
    assert_eq!(
        events.task_info[&legacy].need.as_deref(),
        Some("need5678"),
        "the strict metadata fallback must answer after owner-side index creation"
    );
    assert_eq!(
        fs::read(&db).unwrap(),
        before,
        "owner-side reads must not rewrite the database"
    );

    // With the monitor's own connection open, the authorized sidecar is
    // inspectable, owned like the database, and SQL writes stay rejected.
    let shm = tmp.path().join("mail.sqlite-shm");
    let conn = open_read_only(&db).unwrap();
    assert!(shm.exists(), "the authorized sidecar must exist");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let shm_uid = fs::metadata(&shm).unwrap().uid();
        assert_eq!(
            shm_uid,
            fs::metadata(&db).unwrap().uid(),
            "sidecar and database share one owner"
        );
    }
    let error = conn
        .execute_batch("CREATE TABLE tamper(x)")
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("readonly"), "{error}");
    drop(conn);
}
