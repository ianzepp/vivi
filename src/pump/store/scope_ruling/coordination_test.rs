//! Tests for the atomic slice-coordination commits in [`super`].

use rusqlite::Connection;
use tempfile::TempDir;

use super::super::{ScopeCoordination, ScopeRequest};
use super::{ScopeCoordinationOutcome, ScopeCoordinationResult};
use crate::pump::flow::need_scope::{RulingDecision, ScopeIssue};
use crate::pump::flow::scope::{Resolution, ScopeState};
use crate::pump::store::{DB_FILENAME, PumpStore};

fn open() -> (TempDir, PumpStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    (dir, store)
}

fn reopen(dir: &TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("reopen")
}

fn db_path(dir: &TempDir) -> String {
    dir.path().join(DB_FILENAME).to_string_lossy().into_owned()
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
        paths: vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
        reason: "the slice must touch the files".to_string(),
        design: false,
        decision: RulingDecision::Grant,
        issue: None,
    }
}

fn seed_scope(store: &PumpStore, task: &str) {
    store
        .insert_slice_scope(task, 2, &["src/kept.rs".to_string()])
        .expect("seed slice scope");
}

fn widen_result(free: &[&str]) -> ScopeCoordinationResult {
    ScopeCoordinationResult {
        resolution: Resolution::Widen(strings(free)),
        held: Vec::new(),
    }
}

fn wait_result(holders: &[&str], held: &[&str]) -> ScopeCoordinationResult {
    ScopeCoordinationResult {
        resolution: Resolution::WaitOn(strings(holders)),
        held: strings(held),
    }
}

fn rework_result(reason: &str) -> ScopeCoordinationResult {
    ScopeCoordinationResult {
        resolution: Resolution::Rework(reason.to_string()),
        held: Vec::new(),
    }
}

fn strings(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| (*path).to_string()).collect()
}

fn counted(store: &PumpStore, task: &str) -> u32 {
    store
        .scope_coordination_snapshot(task, 2)
        .expect("snapshot")
        .requests
}

#[test]
fn widen_commit_is_atomic_and_the_duplicate_adopts() {
    let (dir, store) = open();
    seed_scope(&store, "t");
    store
        .prepare_scope_request(&request("slice-t-a1", "t"))
        .expect("prepare");
    let candidate = widen_result(&["src/a.rs"]);
    let outcome = store
        .commit_scope_coordination(&request("slice-t-a1", "t"), &candidate)
        .expect("commit");
    assert_eq!(
        outcome,
        ScopeCoordinationOutcome::Committed(widen_result(&["src/a.rs"]))
    );
    assert_eq!(
        store.slice_scope("t").expect("scope"),
        Some(strings(&["src/kept.rs", "src/a.rs"]))
    );
    assert_eq!(counted(&store, "t"), 1);
    assert_eq!(
        store.scope_state("t").expect("state"),
        Some(ScopeState::Clear),
        "widen leaves the state alone"
    );
    assert!(store.scope_holders("t").expect("holders").is_empty());

    let again = store
        .commit_scope_coordination(&request("slice-t-a1", "t"), &candidate)
        .expect("commit again");
    assert_eq!(
        again,
        ScopeCoordinationOutcome::Adopted(widen_result(&["src/a.rs"]))
    );
    assert_eq!(counted(&store, "t"), 1);
    assert_eq!(
        store.slice_scope("t").expect("scope"),
        Some(strings(&["src/kept.rs", "src/a.rs"]))
    );
    drop(store);
    let reopened = reopen(&dir);
    let replay = reopened
        .commit_scope_coordination(&request("slice-t-a1", "t"), &candidate)
        .expect("adopt after reopen");
    assert_eq!(
        replay.resolution(),
        &widen_result(&["src/a.rs"]),
        "the recorded result survives reopen"
    );
    assert_eq!(counted(&reopened, "t"), 1);
}

#[test]
fn a_different_candidate_still_adopts_the_recorded_one() {
    let (_dir, store) = open();
    seed_scope(&store, "t");
    store
        .prepare_scope_request(&request("slice-t-a1", "t"))
        .expect("prepare");
    store
        .commit_scope_coordination(&request("slice-t-a1", "t"), &widen_result(&["src/a.rs"]))
        .expect("commit");
    let replay = store
        .commit_scope_coordination(&request("slice-t-a1", "t"), &rework_result("other"))
        .expect("adopts anyway");
    assert_eq!(
        replay.resolution(),
        &widen_result(&["src/a.rs"]),
        "the recorded resolution wins"
    );
    assert_eq!(counted(&store, "t"), 1);
    assert_eq!(
        store.scope_state("t").expect("state"),
        Some(ScopeState::Clear),
        "adopting a rework candidate changes no state"
    );
}

#[test]
fn wait_on_persists_holders_held_paths_and_state() {
    let (dir, store) = open();
    seed_scope(&store, "t");
    store
        .prepare_scope_request(&request("slice-t-a1", "t"))
        .expect("prepare");
    let candidate = wait_result(&["t-b", "t-c"], &["src/a.rs"]);
    assert_eq!(
        store
            .commit_scope_coordination(&request("slice-t-a1", "t"), &candidate)
            .expect("commit"),
        ScopeCoordinationOutcome::Committed(wait_result(&["t-b", "t-c"], &["src/a.rs"]))
    );
    assert_eq!(
        store.scope_holders("t").expect("holders"),
        strings(&["t-b", "t-c", "path:src/a.rs"])
    );
    assert_eq!(
        store.scope_state("t").expect("state"),
        Some(ScopeState::Wait)
    );
    assert_eq!(counted(&store, "t"), 1);
    assert_eq!(
        store
            .waiting_scope_request("t")
            .expect("find")
            .expect("waiting row")
            .source,
        "slice-t-a1",
        "the plain wait-on word stays readable"
    );
    let replay = store
        .commit_scope_coordination(&request("slice-t-a1", "t"), &candidate)
        .expect("adopt");
    assert_eq!(
        replay.resolution(),
        &wait_result(&["t-b", "t-c"], &["src/a.rs"]),
        "holders and held paths replay the external mail writes"
    );
    assert_eq!(counted(&store, "t"), 1);
    drop(store);
    let reopened = reopen(&dir);
    assert_eq!(
        reopened.scope_holders("t").expect("holders"),
        strings(&["t-b", "t-c", "path:src/a.rs"]),
        "the holder record is persisted"
    );
    assert_eq!(
        reopened
            .commit_scope_coordination(&request("slice-t-a1", "t"), &candidate)
            .expect("adopt after reopen")
            .resolution(),
        &wait_result(&["t-b", "t-c"], &["src/a.rs"])
    );
}

#[test]
fn rework_is_recorded_with_its_state_and_counter() {
    let (_dir, store) = open();
    seed_scope(&store, "t");
    store
        .prepare_scope_request(&request("slice-t-a1", "t"))
        .expect("prepare");
    assert_eq!(
        store
            .commit_scope_coordination(
                &request("slice-t-a1", "t"),
                &rework_result("second blocked-scope request")
            )
            .expect("commit"),
        ScopeCoordinationOutcome::Committed(rework_result("second blocked-scope request"))
    );
    assert_eq!(
        store.scope_state("t").expect("state"),
        Some(ScopeState::Rework)
    );
    assert_eq!(
        store.slice_scope("t").expect("scope"),
        Some(strings(&["src/kept.rs"])),
        "rework widens nothing"
    );
    assert!(store.scope_holders("t").expect("holders").is_empty());
    assert_eq!(
        store
            .commit_scope_coordination(
                &request("slice-t-a1", "t"),
                &rework_result("second blocked-scope request")
            )
            .expect("adopt"),
        ScopeCoordinationOutcome::Adopted(rework_result("second blocked-scope request"))
    );
    assert_eq!(counted(&store, "t"), 1);

    store
        .prepare_scope_request(&request("slice-t-a1-s1", "t"))
        .expect("prepare second source");
    store
        .commit_scope_coordination(&request("slice-t-a1-s1", "t"), &widen_result(&["src/b.rs"]))
        .expect("commit second source");
    assert_eq!(counted(&store, "t"), 2, "each source counts once");
    assert_eq!(
        store.slice_scope("t").expect("scope"),
        Some(strings(&["src/kept.rs", "src/b.rs"]))
    );
}

#[test]
fn mismatching_identity_and_unprepared_sources_are_refused() {
    let (_dir, store) = open();
    seed_scope(&store, "t");
    assert!(
        store
            .commit_scope_coordination(&request("slice-t-a1", "t"), &widen_result(&["src/a.rs"]))
            .is_err(),
        "no prepared journal row"
    );
    store
        .prepare_scope_request(&request("slice-t-a1", "t"))
        .expect("prepare");
    let other_attempt = ScopeRequest {
        attempt: 2,
        ..request("slice-t-a1", "t")
    };
    assert!(
        store
            .commit_scope_coordination(&other_attempt, &widen_result(&["src/a.rs"]))
            .is_err()
    );
    let other_task = request("slice-t-a1", "other");
    assert!(
        store
            .commit_scope_coordination(&other_task, &widen_result(&["src/a.rs"]))
            .is_err()
    );
    assert_eq!(counted(&store, "t"), 0, "a refusal never counts");
    assert_eq!(
        store.slice_scope("t").expect("scope"),
        Some(strings(&["src/kept.rs"]))
    );
    let row = store.scope_request("slice-t-a1").expect("read").unwrap();
    assert_eq!(row.coordination, None);
}

#[test]
fn a_scope_failure_rolls_the_whole_commit_back() {
    let (_dir, store) = open();
    store
        .prepare_scope_request(&request("slice-t-a1", "t"))
        .expect("prepare");
    assert!(
        store
            .commit_scope_coordination(&request("slice-t-a1", "t"), &widen_result(&["src/a.rs"]))
            .is_err(),
        "the task has no slice_scope row"
    );
    let row = store.scope_request("slice-t-a1").expect("read").unwrap();
    assert_eq!(row.coordination, None, "the journal records nothing");
}

#[test]
fn a_midway_failure_restores_every_field() {
    let (dir, store) = open();
    seed_scope(&store, "t");
    store
        .prepare_scope_request(&request("slice-t-a1", "t"))
        .expect("prepare first");
    store
        .commit_scope_coordination(&request("slice-t-a1", "t"), &widen_result(&["src/a.rs"]))
        .expect("first commit");
    let counted_before = counted(&store, "t");
    let scope_before = store.slice_scope("t").expect("scope").unwrap();
    drop(store);

    let trigger = Connection::open(db_path(&dir)).expect("open trigger connection");
    trigger
        .execute_batch(
            "CREATE TRIGGER fail_first_coordination \
             BEFORE UPDATE ON scope_journal \
             WHEN NEW.coordination_json IS NOT NULL AND OLD.coordination IS NULL \
             BEGIN SELECT RAISE(ABORT, 'test-fail'); END;",
        )
        .expect("create failing trigger");

    let store = reopen(&dir);
    store
        .prepare_scope_request(&request("slice-t-a1-s1", "t"))
        .expect("prepare second");
    let candidate = wait_result(&["t-b"], &["src/b.rs"]);
    assert!(
        store
            .commit_scope_coordination(&request("slice-t-a1-s1", "t"), &candidate)
            .is_err()
    );
    assert_eq!(counted(&store, "t"), counted_before, "the bump rolled back");
    assert_eq!(
        store.slice_scope("t").expect("scope").unwrap(),
        scope_before,
        "the widen rolled back"
    );
    assert!(
        store.scope_holders("t").expect("holders").is_empty(),
        "the holder record rolled back"
    );
    assert_eq!(
        store.scope_state("t").expect("state"),
        Some(ScopeState::Clear),
        "the state rolled back"
    );
    let row = store.scope_request("slice-t-a1-s1").expect("read").unwrap();
    assert_eq!(row.coordination, None);
    drop(store);
    trigger
        .execute_batch("DROP TRIGGER fail_first_coordination;")
        .expect("drop the failing trigger");
    drop(trigger);
    let store = reopen(&dir);
    assert_eq!(
        store
            .commit_scope_coordination(&request("slice-t-a1-s1", "t"), &candidate)
            .expect("the same commit succeeds without the trigger"),
        ScopeCoordinationOutcome::Committed(wait_result(&["t-b"], &["src/b.rs"]))
    );
}

#[test]
fn the_snapshot_reads_without_counting() {
    let (_dir, store) = open();
    seed_scope(&store, "t");
    let first = store.scope_coordination_snapshot("t", 2).expect("snapshot");
    assert_eq!(first.requests, 0);
    assert!(first.others.is_empty(), "no flow rows name other slices");
    let again = store
        .scope_coordination_snapshot("t", 2)
        .expect("snapshot again");
    assert_eq!(again, first, "reading counts nothing");
    store
        .prepare_scope_request(&request("slice-t-a1", "t"))
        .expect("prepare");
    store
        .commit_scope_coordination(&request("slice-t-a1", "t"), &widen_result(&["src/a.rs"]))
        .expect("commit");
    assert_eq!(counted(&store, "t"), 1);
    assert!(
        store.scope_coordination_snapshot("gone", 2).is_err(),
        "a task without a scope row has no snapshot"
    );
}

#[test]
fn an_unsettaled_word_only_record_is_refused_not_replayed() {
    let (_dir, store) = open();
    seed_scope(&store, "t");
    store
        .prepare_scope_request(&request("slice-t-a1", "t"))
        .expect("prepare");
    store
        .record_scope_coordination("slice-t-a1", ScopeCoordination::WaitOn)
        .expect("record the word only");
    assert!(
        store
            .commit_scope_coordination(&request("slice-t-a1", "t"), &wait_result(&["t-b"], &[]))
            .is_err(),
        "a word without a typed payload cannot be adopted"
    );
    assert_eq!(counted(&store, "t"), 0);
}

#[test]
fn the_typed_payload_round_trips_through_json() {
    let candidate = wait_result(&["t-b"], &["src/a.rs"]);
    let text = serde_json::to_string(&candidate).expect("encode");
    let decoded: ScopeCoordinationResult = serde_json::from_str(&text).expect("decode");
    assert_eq!(decoded, candidate);
    let bare: ScopeCoordinationResult =
        serde_json::from_str(r#"{"resolution":{"rework":"why"}}"#).expect("held defaults");
    assert_eq!(bare, rework_result("why"));
}
