use crate::pump::store::{DB_FILENAME, PumpStore, SCHEMA_VERSION, SliceProvenance};
use rusqlite::Connection;
use tempfile::tempdir;

fn row() -> SliceProvenance {
    SliceProvenance {
        hand: "hand-1".into(),
        need: "need-1".into(),
        run: 2,
        task: "task-1".into(),
        target: "vivi".into(),
        dispatch_base: "dispatch".into(),
        authorization_base: "authorization".into(),
        predecessor_hand: Some("prior".into()),
        source_hand: Some("source".into()),
        handoff: Some("handoff".into()),
        candidate: None,
        verification_failure: None,
        preparation_failure: String::new(),
    }
}

#[test]
fn provenance_reopens_replays_and_survives_state_save() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    {
        let s = PumpStore::open(&path).unwrap();
        let version = s.schema_version().unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        s.record_slice_provenance(&row()).unwrap();
        s.save(&Default::default()).unwrap();
    }
    let s = PumpStore::open(&path).unwrap();
    assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
    s.record_slice_provenance(&row()).unwrap();
    let got = s.slice_provenance_by_hand("hand-1").unwrap().unwrap();
    assert_eq!(got.dispatch_base, "dispatch");
    assert_eq!(got.authorization_base, "authorization");
    assert_eq!(
        s.slice_provenance_by_task("task-1").unwrap(),
        Some(got.clone())
    );
    assert_eq!(s.slice_provenance_by_source("source").unwrap(), [got]);
}
#[test]
fn identity_mismatch_is_refused_and_candidate_is_write_once() {
    let dir = tempdir().unwrap();
    let s = PumpStore::open(&dir.path().join(DB_FILENAME)).unwrap();
    s.record_slice_provenance(&row()).unwrap();
    let mut changed = row();
    changed.dispatch_base = "different".into();
    assert!(s.record_slice_provenance(&changed).is_err());
    s.complete_slice_candidate("hand-1", "commit-a").unwrap();
    s.complete_slice_candidate("hand-1", "commit-a").unwrap();
    assert!(s.complete_slice_candidate("hand-1", "commit-b").is_err());
}
#[test]
fn stable_job_replay_reuses_provenance() {
    let dir = tempdir().unwrap();
    let s = PumpStore::open(&dir.path().join(DB_FILENAME)).unwrap();
    s.record_slice_provenance(&row()).unwrap();
    assert_eq!(
        s.prepare_slice_job("job-stable", "hand-1").unwrap(),
        "hand-1"
    );
    assert_eq!(
        s.prepare_slice_job("job-stable", "hand-1").unwrap(),
        "hand-1"
    );
}
#[test]
fn read_only_open_does_not_create_flow_tables() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    Connection::open(&path).unwrap();
    let s = PumpStore::open_read_only(&path).unwrap().unwrap();
    drop(s);
    let conn = Connection::open(&path).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='slice_provenance'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}
