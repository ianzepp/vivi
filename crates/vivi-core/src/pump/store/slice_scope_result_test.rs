use super::{PumpStore, SliceScopeResult};
use crate::pump::store::DB_FILENAME;
use tempfile::tempdir;

fn decision() -> SliceScopeResult {
    SliceScopeResult {
        original_hand: "hand-a".into(),
        accepted_job: "job-a".into(),
        need: "need-a".into(),
        run: 3,
        task: "task-a".into(),
        target: "vivi".into(),
        source_branch: "branch-a".into(),
        dispatch_base: "base-a".into(),
        authorization_base: "auth-a".into(),
        candidate: "candidate-a".into(),
        original_scope: vec!["src/a".into()],
        changed_paths: vec!["src/a".into(), "src/b".into()],
        additions: vec!["src/b".into()],
        granted_paths: vec!["src/b".into()],
        held_paths: vec![],
        owner_tasks: vec!["task-owner".into()],
        disposition: "approved".into(),
        verification_reason: "verified".into(),
        dependency_conflict: vec![],
    }
}

#[test]
fn scope_result_is_immutable_and_publication_progress_survives_reopen_and_save() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let store = PumpStore::open(&path).unwrap();
    let row = decision();
    store
        .record_scope_decision_with_publication(
            &row,
            &[("note", "{\"id\":\"n1\"}"), ("dependencies", "[]")],
        )
        .unwrap();
    store
        .mark_slice_publication("hand-a", "note", "complete")
        .unwrap();
    store.save(&crate::pump::state::State::default()).unwrap();
    let mut changed = row.clone();
    changed.original_scope.push("src/changed".into());
    assert!(store.record_slice_scope_result(&changed).is_err());
    drop(store);
    let reopened = PumpStore::open(&path).unwrap();
    let saved = reopened.slice_scope_result("hand-a").unwrap().unwrap();
    assert_eq!(saved.original_scope, vec!["src/a"]);
    assert_eq!(saved.authorization_base, "auth-a");
    assert_eq!(saved.granted_paths, vec!["src/b"]);
    let progress = reopened.slice_publication("hand-a").unwrap();
    assert_eq!(progress.len(), 2);
    assert_eq!(progress[0].item, "dependencies");
    assert_eq!(progress[1].progress, "complete");
}

#[test]
fn disposition_vocabulary_is_checked_and_decision_replay_is_deduplicated() {
    let dir = tempdir().unwrap();
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).unwrap();
    let row = decision();
    store.record_slice_scope_result(&row).unwrap();
    store.record_slice_scope_result(&row).unwrap();
    let mut bad = row;
    bad.disposition = "maybe".into();
    assert!(store.record_slice_scope_result(&bad).is_err());
}
