//! Tests for the scope workflow journal in [`super`].

use tempfile::TempDir;

use super::{ScopeCoordination, ScopeRequest, ScopeRequestOutcome};
use crate::pump::flow::index::StepJobRow;
use crate::pump::flow::need_scope::{RulingDecision, ScopeIssue};
use crate::pump::store::{DB_FILENAME, PumpStore, SCHEMA_VERSION};

/// A journal row's stored identity, asserted through the typed read.
fn stored(store: &PumpStore, job: &str) -> super::ScopeJournalRow {
    store
        .scope_request(job)
        .expect("read scope request")
        .expect("journal row")
}

fn open() -> (TempDir, PumpStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    (dir, store)
}

fn reopen(dir: &TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("reopen")
}

fn request(source: &str, task: &str) -> ScopeRequest {
    ScopeRequest {
        source: source.to_string(),
        need: "n1".to_string(),
        target: "default".to_string(),
        run: 2,
        node: "slice:n1-1".to_string(),
        task: task.to_string(),
        attempt: 1,
        paths: vec!["src/a.rs".to_string()],
        reason: "the slice must touch the file".to_string(),
        design: false,
        decision: RulingDecision::OperatorRequired,
        issue: Some(ScopeIssue::OutsideAllow),
    }
}

fn grant(source: &str, task: &str) -> ScopeRequest {
    ScopeRequest {
        decision: RulingDecision::Grant,
        issue: None,
        ..request(source, task)
    }
}

fn insert_step_job(store: &PumpStore, job: &str, task: &str) {
    store
        .insert_step_job(
            "n1",
            &StepJobRow {
                job: job.to_string(),
                run: 2,
                node: "slice:n1-1".to_string(),
                task: task.to_string(),
                attempt: 1,
            },
        )
        .expect("insert step job");
}

#[test]
fn prepared_request_round_trips_across_reopen() {
    let (dir, store) = open();
    assert!(store.scope_request("slice-t-a1").expect("read").is_none());
    assert_eq!(
        store
            .prepare_scope_request(&request("slice-t-a1", "t"))
            .expect("prepare"),
        ScopeRequestOutcome::Recorded
    );
    let row = stored(&store, "slice-t-a1-c1");
    assert_eq!(row.source, "slice-t-a1");
    assert_eq!(row.need, "n1");
    assert_eq!(row.target, "default");
    assert_eq!(row.run, 2);
    assert_eq!(row.node, "slice:n1-1");
    assert_eq!(row.task, "t");
    assert_eq!(row.attempt, 1);
    assert_eq!(row.paths, ["src/a.rs"]);
    assert_eq!(row.reason, "the slice must touch the file");
    assert!(!row.design);
    assert_eq!(row.decision, RulingDecision::OperatorRequired);
    assert_eq!(row.issue, Some(ScopeIssue::OutsideAllow));
    assert_eq!(row.published_at, None);
    assert_eq!(row.continuation, None);
    assert_eq!(row.round, None);
    assert_eq!(row.coordination, None);
    assert_eq!(store.schema_version().expect("version"), SCHEMA_VERSION);
    drop(store);
    let reopened = reopen(&dir);
    assert_eq!(stored(&reopened, "slice-t-a1"), row);
    assert_eq!(
        reopened
            .prepare_scope_request(&request("slice-t-a1", "t"))
            .expect("prepare again"),
        ScopeRequestOutcome::Adopted
    );
}

#[test]
fn same_source_for_different_work_is_refused() {
    let (_dir, store) = open();
    store
        .prepare_scope_request(&request("slice-t-a1", "t"))
        .expect("prepare");
    let other = request("slice-t-a1-c1", "other");
    assert!(store.prepare_scope_request(&other).is_err());
    let reattempted = ScopeRequest {
        attempt: 2,
        ..request("slice-t-a1-c2", "t")
    };
    assert!(store.prepare_scope_request(&reattempted).is_err());
    assert_eq!(stored(&store, "slice-t-a1").task, "t");
    assert!(store.scope_request("slice-t-a2").expect("read").is_none());
}

#[test]
fn correction_key_adopts_the_original_record() {
    let (_dir, store) = open();
    assert_eq!(
        store
            .prepare_scope_request(&request("slice-t-a1-c1", "t"))
            .expect("prepare"),
        ScopeRequestOutcome::Recorded
    );
    assert_eq!(
        store
            .prepare_scope_request(&request("slice-t-a1-c2", "t"))
            .expect("prepare via a second correction"),
        ScopeRequestOutcome::Adopted
    );
    assert!(
        store
            .scope_request("slice-t-a1-c3")
            .expect("read")
            .is_some()
    );
    assert!(
        store
            .scope_request("slice-other-a1")
            .expect("read")
            .is_none()
    );
}

#[test]
fn unpublished_decision_refreshes_and_published_one_does_not_drift() {
    let (_dir, store) = open();
    store
        .prepare_scope_request(&request("slice-t-a1", "t"))
        .expect("prepare");
    let revised = ScopeRequest {
        paths: vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
        design: true,
        ..request("slice-t-a1", "t")
    };
    assert_eq!(
        store.prepare_scope_request(&revised).expect("refresh"),
        ScopeRequestOutcome::Refreshed
    );
    let row = stored(&store, "slice-t-a1");
    assert_eq!(row.paths, ["src/a.rs", "src/b.rs"]);
    assert!(row.design);
    assert!(
        store
            .acknowledge_scope_publication("slice-t-a1", 100)
            .expect("ack")
    );
    assert!(
        !store
            .acknowledge_scope_publication("slice-t-a1", 200)
            .expect("ack again")
    );
    assert_eq!(stored(&store, "slice-t-a1").published_at, Some(100));
    assert_eq!(
        store
            .prepare_scope_request(&revised)
            .expect("identical after publish"),
        ScopeRequestOutcome::Adopted
    );
    let redone = request("slice-t-a1", "t");
    assert!(store.prepare_scope_request(&redone).is_err());
    assert_eq!(stored(&store, "slice-t-a1").paths, ["src/a.rs", "src/b.rs"]);
    assert_eq!(stored(&store, "slice-t-a1").published_at, Some(100));
    assert!(
        store
            .acknowledge_scope_publication("slice-gone-a1", 5)
            .is_err()
    );
}

#[test]
fn continuation_is_reserved_once_and_stays_stable() {
    let (_dir, store) = open();
    store
        .prepare_scope_request(&request("slice-t-a1", "t"))
        .expect("prepare");
    assert!(
        store
            .reserve_scope_continuation("slice-t-a1", "hand-cont-1")
            .expect("reserve")
    );
    assert!(
        !store
            .reserve_scope_continuation("slice-t-a1-c1", "hand-cont-1")
            .expect("adopt")
    );
    assert!(
        store
            .reserve_scope_continuation("slice-t-a1", "hand-cont-2")
            .is_err()
    );
    assert_eq!(
        stored(&store, "slice-t-a1").continuation.as_deref(),
        Some("hand-cont-1")
    );
    assert!(
        store
            .reserve_scope_continuation("slice-gone-a1", "hand-cont-1")
            .is_err()
    );
}

#[test]
fn rounds_reuse_same_source_and_advance_distinct_sources() {
    let (_dir, store) = open();
    store
        .prepare_scope_request(&request("slice-t-a1", "t"))
        .expect("prepare first");
    let id_for = |round: u32| format!("slice-t-a1-s{round}");
    assert_eq!(
        store
            .reserve_scope_round("slice-t-a1", id_for)
            .expect("round"),
        1
    );
    assert_eq!(
        store
            .reserve_scope_round("slice-t-a1-c1", id_for)
            .expect("again"),
        1
    );
    store
        .prepare_scope_request(&request("slice-t-a1-s1", "t"))
        .expect("prepare second");
    assert_eq!(
        store
            .reserve_scope_round("slice-t-a1-s1", id_for)
            .expect("round"),
        2
    );
    store
        .prepare_scope_request(&request("slice-t-a1-s2", "t"))
        .expect("prepare third");
    assert_eq!(
        store
            .reserve_scope_round("slice-t-a1-s2", id_for)
            .expect("round"),
        3
    );
    assert_eq!(stored(&store, "slice-t-a1-s1").round, Some(2));
    assert_eq!(stored(&store, "slice-t-a1-s2").round, Some(3));
}

#[test]
fn rounds_skip_existing_scope_jobs_and_held_rounds() {
    let (dir, store) = open();
    insert_step_job(&store, "slice-t-a1-s1", "t");
    insert_step_job(&store, "slice-t-a1-s2", "t");
    store
        .prepare_scope_request(&request("slice-t-a1", "t"))
        .expect("prepare");
    let id_for = |round: u32| format!("slice-t-a1-s{round}");
    assert_eq!(
        store
            .reserve_scope_round("slice-t-a1", id_for)
            .expect("round"),
        3
    );
    store
        .prepare_scope_request(&request("slice-t-a1-s3", "t"))
        .expect("prepare second");
    assert_eq!(
        store
            .reserve_scope_round("slice-t-a1-s3", id_for)
            .expect("round"),
        4
    );
    drop(store);
    let reopened = reopen(&dir);
    assert_eq!(
        reopened
            .reserve_scope_round("slice-t-a1", id_for)
            .expect("adopt after reopen"),
        3
    );
}

#[test]
fn waiting_request_is_found_until_coordination_supersedes_it() {
    let (_dir, store) = open();
    store
        .prepare_scope_request(&request("slice-t-a1", "t"))
        .expect("prepare");
    assert!(store.waiting_scope_request("t").expect("find").is_none());
    store
        .prepare_scope_request(&request("slice-t-a1-s1", "t"))
        .expect("prepare second");
    store
        .record_scope_coordination("slice-t-a1", ScopeCoordination::WaitOn)
        .expect("record wait");
    store
        .record_scope_coordination("slice-t-a1-s1", ScopeCoordination::WaitOn)
        .expect("record wait");
    assert_eq!(
        store
            .waiting_scope_request("t")
            .expect("find")
            .expect("waiting row")
            .source,
        "slice-t-a1-s1"
    );
    store
        .record_scope_coordination("slice-t-a1-s1", ScopeCoordination::Widen)
        .expect("record widen");
    assert_eq!(
        store
            .waiting_scope_request("t")
            .expect("find")
            .expect("still waiting")
            .source,
        "slice-t-a1"
    );
    store
        .record_scope_coordination("slice-t-a1", ScopeCoordination::Widen)
        .expect("record widen");
    let row = stored(&store, "slice-t-a1-s1");
    assert_eq!(row.coordination, Some(ScopeCoordination::Widen));
    assert!(store.waiting_scope_request("t").expect("find").is_none());
    assert!(
        store
            .waiting_scope_request("other")
            .expect("find")
            .is_none()
    );
    assert!(
        store
            .record_scope_coordination("slice-gone-a1", ScopeCoordination::Rework)
            .is_err()
    );
    store
        .prepare_scope_request(&grant("slice-u-a1", "u"))
        .expect("prepare grant");
    store
        .record_scope_coordination("slice-u-a1", ScopeCoordination::Rework)
        .expect("record rework");
    assert_eq!(
        stored(&store, "slice-u-a1").coordination,
        Some(ScopeCoordination::Rework)
    );
    assert_eq!(
        store
            .waiting_scope_request("u")
            .expect("find")
            .map(|row| row.source),
        None
    );
}

#[test]
fn latest_hand_step_job_is_the_correction_stripped_base() {
    let (_dir, store) = open();
    assert!(store.latest_hand_step_job("t").expect("read").is_none());
    insert_step_job(&store, "slice-t-a1", "t");
    insert_step_job(&store, "slice-t-a1-s1", "t");
    insert_step_job(&store, "slice-t-a1-s1-c1", "t");
    assert_eq!(
        store.latest_hand_step_job("t").expect("read").as_deref(),
        Some("slice-t-a1-s1")
    );
    assert!(store.latest_hand_step_job("gone").expect("read").is_none());
}
