//! Tests for the durable retry intents and generation budgets in
//! [`crate::pump::store::step_retry`].

use tempfile::tempdir;

use crate::pump::jobs::JobSpec;
use crate::pump::state::State;
use crate::pump::store::step_execution::ExecutionKey;
use crate::pump::store::step_retry::{
    REDISPATCH_BUDGET, RetryIntent, RetryIntentRow, RetryReservation,
};
use crate::pump::store::{DB_FILENAME, PumpStore};

const HAND: &str = "hand";
const MERGE: &str = "merge";

fn open() -> (tempfile::TempDir, PumpStore) {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    (dir, store)
}

fn reopen(dir: &tempfile::TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("reopen")
}

fn key(need: &str, run: u32, task: &str, phase: &str) -> ExecutionKey {
    ExecutionKey::new(need, run, task, phase)
}

fn hand(need: &str, run: u32, task: &str) -> ExecutionKey {
    key(need, run, task, HAND)
}

fn intent(key: &ExecutionKey, generation: u32, failed: &str, replacement: &str) -> RetryIntent {
    RetryIntent {
        key: key.clone(),
        generation,
        execution: format!(
            "{}:r{}:{}:{}:g{}",
            key.need, key.run, key.task, key.phase, generation
        ),
        failed_job: failed.into(),
        failure_class: "provider".into(),
        evidence: " 502 bad gateway".into(),
        replacement_job: replacement.into(),
        spec: JobSpec::new(replacement, "slice", "hand", "subject", "body"),
        deadline: 1_000,
    }
}

fn reserved(result: RetryReservation) -> RetryIntentRow {
    match result {
        RetryReservation::Reserved(row) => row,
        other => panic!("expected a fresh reservation, got {other:?}"),
    }
}

fn existing(result: RetryReservation) -> RetryIntentRow {
    match result {
        RetryReservation::Existing(row) => row,
        other => panic!("expected an existing reservation, got {other:?}"),
    }
}

fn exhausted(result: RetryReservation) -> Vec<RetryIntentRow> {
    match result {
        RetryReservation::Exhausted(history) => history,
        other => panic!("expected exhaustion, got {other:?}"),
    }
}

#[test]
fn reservation_replays_unchanged_and_keeps_one_budget_slot() {
    let (_dir, store) = open();
    let k = hand("n1", 1, "t1");
    let first = intent(&k, 1, "job-a", "job-a-r1");
    let row = reserved(store.reserve_retry(&first).expect("reserve"));
    assert_eq!(row.ordinal, 1);
    assert_eq!(row.spec.id, "job-a-r1");
    assert_eq!(row.deadline, 1_000);
    assert!(row.is_incomplete());

    let replay = existing(store.reserve_retry(&first).expect("replay"));
    assert_eq!(replay, row);

    let mut drifted = first.clone();
    drifted.spec.body = "a completely different brief".into();
    drifted.deadline = 9_999;
    drifted.evidence = "different evidence".into();
    drifted.failure_class = "timeout".into();
    let immutable = existing(store.reserve_retry(&drifted).expect("replay"));
    assert_eq!(immutable, row);
    assert_eq!(immutable.spec.body, "body");
    assert_eq!(immutable.deadline, 1_000);

    assert_eq!(store.retry_intents(&k, 1).expect("history").len(), 1);
}

#[test]
fn budget_exhausts_after_two_reservations_per_generation() {
    assert_eq!(REDISPATCH_BUDGET, 2);
    let (_dir, store) = open();
    let k = hand("n1", 1, "t1");
    let first = reserved(
        store
            .reserve_retry(&intent(&k, 1, "job-a", "job-a-r1"))
            .expect("reserve 1"),
    );
    let second = reserved(
        store
            .reserve_retry(&intent(&k, 1, "job-b", "job-b-r1"))
            .expect("reserve 2"),
    );
    assert_eq!((first.ordinal, second.ordinal), (1, 2));

    let history = exhausted(
        store
            .reserve_retry(&intent(&k, 1, "job-c", "job-c-r1"))
            .expect("reserve 3"),
    );
    assert_eq!(history, vec![first.clone(), second.clone()]);

    let replay = existing(
        store
            .reserve_retry(&intent(&k, 1, "job-a", "job-a-r1"))
            .expect("replay"),
    );
    assert_eq!(replay, first);
}

#[test]
fn fresh_generations_and_other_phases_hold_independent_budgets() {
    let (_dir, store) = open();
    let k = hand("n1", 1, "t1");
    let g1 = store.register_execution(&k).expect("register");
    assert_eq!(g1.generation, 1);

    let _ = reserved(
        store
            .reserve_retry(&intent(&k, 1, "job-a", "job-a-r1"))
            .expect("reserve 1"),
    );
    let _ = reserved(
        store
            .reserve_retry(&intent(&k, 1, "job-b", "job-b-r1"))
            .expect("reserve 2"),
    );
    exhausted(
        store
            .reserve_retry(&intent(&k, 1, "job-c", "job-c-r1"))
            .expect("reserve 3"),
    );

    let merge = key("n1", 1, "t1", MERGE);
    let merge_row = reserved(
        store
            .reserve_retry(&intent(&merge, 1, "job-a", "merge-job-a-r1"))
            .expect("reserve merge"),
    );
    assert_eq!(merge_row.ordinal, 1);

    let g2 = store.consume_retry(&g1.execution).expect("consume");
    assert_eq!(g2.generation, 2);
    let fresh = reserved(
        store
            .reserve_retry(&intent(&k, 2, "job-a", "job-a2-r1"))
            .expect("reserve generation 2"),
    );
    assert_eq!(fresh.ordinal, 1);

    let other = hand("n2", 1, "t1");
    let other_row = reserved(
        store
            .reserve_retry(&intent(&other, 1, "job-a", "n2-job-a-r1"))
            .expect("reserve other need"),
    );
    assert_eq!(other_row.ordinal, 1);
}

#[test]
fn progress_cancellation_and_settlement_are_idempotent_stamps() {
    let (_dir, store) = open();
    let k = hand("n1", 1, "t1");
    let row = reserved(
        store
            .reserve_retry(&intent(&k, 1, "job-a", "job-a-r1"))
            .expect("reserve"),
    );
    assert_eq!(
        store.incomplete_retry_intents().expect("incomplete"),
        vec![row.clone()]
    );

    store
        .mark_retry_materialized(&row.replacement_job, 1_500)
        .expect("materialize");
    store
        .mark_retry_materialized(&row.replacement_job, 2_500)
        .expect("materialize again");
    let materialized = store
        .retry_intent(&row.replacement_job)
        .expect("lookup")
        .expect("some");
    assert_eq!(materialized.materialized_at, Some(1_500));
    assert!(materialized.is_incomplete());

    store
        .settle_retry_intent(&row.replacement_job, 3_000)
        .expect("settle");
    store
        .settle_retry_intent(&row.replacement_job, 4_000)
        .expect("settle again");
    let settled = store
        .retry_intent(&row.replacement_job)
        .expect("lookup")
        .expect("some");
    assert_eq!(settled.settled_at, Some(3_000));
    assert!(!settled.is_incomplete());
    assert!(
        store
            .incomplete_retry_intents()
            .expect("incomplete")
            .is_empty()
    );

    let cancelled = reserved(
        store
            .reserve_retry(&intent(&k, 1, "job-b", "job-b-r1"))
            .expect("reserve 2"),
    );
    store
        .cancel_retry_intent(&cancelled.replacement_job, 5_000)
        .expect("cancel");
    assert!(
        !store
            .retry_intent(&cancelled.replacement_job)
            .expect("lookup")
            .expect("some")
            .is_incomplete()
    );

    assert!(
        store
            .mark_retry_materialized("no-such-replacement", 1)
            .is_err()
    );
}

#[test]
fn intents_survive_state_save_and_reopen_with_their_budget() {
    let (dir, mut store) = open();
    let k = hand("n1", 1, "t1");
    let first = reserved(
        store
            .reserve_retry(&intent(&k, 1, "job-a", "job-a-r1"))
            .expect("reserve 1"),
    );
    let second = reserved(
        store
            .reserve_retry(&intent(&k, 1, "job-b", "job-b-r1"))
            .expect("reserve 2"),
    );
    store
        .mark_retry_materialized(&first.replacement_job, 1_500)
        .expect("materialize");
    let mut first = first;
    first.materialized_at = Some(1_500);

    store.save(&State::default()).expect("save");
    drop(store);
    let store = reopen(&dir);

    let history = store.retry_intents(&k, 1).expect("history");
    assert_eq!(history.len(), 2);
    assert_eq!(history, vec![first.clone(), second.clone()]);
    assert_eq!(history[0].materialized_at, Some(1_500));

    let replay = existing(
        store
            .reserve_retry(&intent(&k, 1, "job-a", "job-a-r1"))
            .expect("replay"),
    );
    assert_eq!(replay, first);
    exhausted(
        store
            .reserve_retry(&intent(&k, 1, "job-c", "job-c-r1"))
            .expect("reserve 3"),
    );
}
