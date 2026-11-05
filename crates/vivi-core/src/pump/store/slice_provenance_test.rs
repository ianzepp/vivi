use super::SliceProvenance;
use crate::pump::store::{DB_FILENAME, PumpStore, SCHEMA_VERSION};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use tempfile::tempdir;

fn row() -> SliceProvenance {
    SliceProvenance {
        original_hand: "hand-1".into(),
        need: "need-1".into(),
        run: 2,
        task: "task-1".into(),
        target: "vivi".into(),
        dispatch_base: "dispatch-sha".into(),
        authorization_base: "accepted-sha".into(),
        predecessor_hand: Some("prior-hand".into()),
        source_hand: Some("source-hand".into()),
        handoff: Some("handoff-1".into()),
        candidate: None,
        job_id: "stable-job".into(),
        preparation_error: Some("ambiguous input retained".into()),
        verification_error: "verification pending".into(),
    }
}

#[test]
fn immutable_provenance_replays_and_survives_state_save_and_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let store = PumpStore::open(&path).unwrap();
    let initial = store.schema_version().unwrap();
    store.record_slice_provenance(&row()).unwrap();
    store.save(&crate::pump::state::State::default()).unwrap();
    assert_eq!(
        store
            .slice_provenance("hand-1")
            .unwrap()
            .unwrap()
            .authorization_base,
        "accepted-sha"
    );
    drop(store);
    let reopened = PumpStore::open(&path).unwrap();
    assert_eq!(reopened.schema_version().unwrap(), initial);
    assert_eq!(initial, SCHEMA_VERSION);
    reopened.record_slice_provenance(&row()).unwrap();
    assert_eq!(
        reopened
            .slice_preparation("stable-job")
            .unwrap()
            .unwrap()
            .dispatch_base,
        "dispatch-sha"
    );
    assert_eq!(
        reopened
            .slice_provenance_related(Some("task-1"), None)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn immutable_fields_refuse_mismatch_and_candidate_is_write_once() {
    let dir = tempdir().unwrap();
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).unwrap();
    let original = row();
    store.record_slice_provenance(&original).unwrap();
    let mut mismatch = original.clone();
    mismatch.authorization_base = "changed".into();
    assert!(store.record_slice_provenance(&mismatch).is_err());
    store
        .complete_slice_candidate("hand-1", "candidate-sha")
        .unwrap();
    store
        .complete_slice_candidate("hand-1", "candidate-sha")
        .unwrap();
    assert!(
        store
            .complete_slice_candidate("hand-1", "other-sha")
            .is_err()
    );
    assert_eq!(
        store
            .slice_provenance("hand-1")
            .unwrap()
            .unwrap()
            .candidate
            .as_deref(),
        Some("candidate-sha")
    );
}

#[test]
fn read_only_access_does_not_create_provenance_tables() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    drop(PumpStore::open(&path).unwrap());
    let before = Connection::open(&path).unwrap();
    before
        .execute_batch("DROP TABLE slice_provenance;")
        .unwrap();
    drop(before);
    let read = PumpStore::open_read_only(&path).unwrap().unwrap();
    assert!(read.slice_provenance("missing").is_err());
    drop(read);
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let found: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='slice_provenance'",
            [],
            |r| r.get(0),
        )
        .optional()
        .unwrap();
    assert_eq!(found, None);
}
