//! Tests for the execution-aware receipt reader in [`super`].

use std::collections::BTreeMap;

use tempfile::tempdir;

use super::{
    AcceptedDispatch, ExecutionQuery, ExecutionReceipt, SavedDispatch, load, receipt, select,
};
use crate::mailspace::PoolEventRow;
use crate::pool::collect::{Collected, JobResult, Outcome};
use crate::pool::record::{DispatchRecord, OpenDispatch, collected_note, dispatch_note};
use crate::pump::store::{DB_FILENAME, PumpStore};

const TASK: &str = "t-1";

fn store(dir: &tempfile::TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open pump store")
}

/// Insert dispatch evidence into the recovery tables through plain SQL, the
/// way the dispatcher's later slices will through the store writers.
fn save_evidence(dir: &tempfile::TempDir, job: &str, started_at: i64, task: &str, pool_job: &str) {
    let conn = rusqlite::Connection::open(dir.path().join(DB_FILENAME)).expect("open sqlite");
    conn.execute(
        "INSERT INTO recovery_dispatch_starts (job, at, evidence_json) VALUES (?1, ?2, '{}')",
        rusqlite::params![job, started_at],
    )
    .expect("write start row");
    conn.execute(
        "INSERT INTO recovery_dispatch_accepts (job, task, pool, slot, pool_job, snapshot_json)
         VALUES (?1, ?2, 'studio', 'slot-1', ?3, NULL)",
        rusqlite::params![job, task, pool_job],
    )
    .expect("write accept row");
}

fn record(pool_job: &str, slot: &str, at: u64) -> DispatchRecord {
    DispatchRecord {
        target: None,
        repository: None,
        pool: "studio".into(),
        need: None,
        kind: None,
        slot: slot.into(),
        machine: "m1".into(),
        job: pool_job.into(),
        model: "m/x".into(),
        from: "main".into(),
        branch: "factory/a".into(),
        start: "a".repeat(40),
        with: Vec::new(),
        budget: 600,
        at,
        read_only: false,
        hardware: String::new(),
        abandoned: 0,
        abandoned_cost_micros: 0,
    }
}

fn dispatched(pool_job: &str, slot: &str, at: u64) -> PoolEventRow {
    PoolEventRow {
        handle: TASK.into(),
        event_type: "pool_dispatched".into(),
        note: dispatch_note(&record(pool_job, slot, at)),
    }
}

fn collected(pool_job: &str, slot: &str, outcome: &Outcome) -> PoolEventRow {
    let open = OpenDispatch {
        task: TASK.into(),
        record: record(pool_job, slot, 0),
    };
    PoolEventRow {
        handle: TASK.into(),
        event_type: "pool_collected".into(),
        note: collected_note(&open, outcome),
    }
}

fn collected_on(task: &str, pool_job: &str, slot: &str, outcome: &str) -> PoolEventRow {
    PoolEventRow {
        handle: task.into(),
        event_type: "pool_collected".into(),
        note: serde_json::json!({
            "job": pool_job,
            "pool": "studio",
            "slot": slot,
            "outcome": outcome,
        })
        .to_string(),
    }
}

fn finished() -> Outcome {
    Outcome::Finished(Collected {
        result: JobResult {
            fields: BTreeMap::new(),
        },
        receipt: String::new(),
        stderr: String::new(),
        log: String::new(),
    })
}

fn accepted(pool_job: &str) -> AcceptedDispatch {
    AcceptedDispatch {
        task: Some(TASK.into()),
        pool: Some("studio".into()),
        slot: Some("slot-1".into()),
        pool_job: Some(pool_job.into()),
    }
}

fn saved_at(started_at: u64, accepted: Option<AcceptedDispatch>) -> SavedDispatch {
    SavedDispatch {
        started_at: Some(started_at),
        accepted,
    }
}

fn query<'a>(
    job: &'a str,
    events: &'a [PoolEventRow],
    briefed: bool,
    prior: &'a [String],
) -> ExecutionQuery<'a> {
    ExecutionQuery {
        job,
        task: TASK,
        events,
        briefed,
        prior_pool_jobs: prior,
    }
}

#[test]
fn a_job_without_saved_evidence_is_absent_and_adopts_nothing() {
    let dir = tempdir().expect("tempdir");
    let store = store(&dir);
    let old = vec![
        dispatched("pj-old", "slot-1", 10),
        collected("pj-old", "slot-1", &finished()),
    ];
    assert_eq!(
        receipt(&store, query("j-1", &old, true, &[])).expect("receipt"),
        ExecutionReceipt::Absent
    );
    let saved = SavedDispatch::default();
    assert_eq!(
        select(&saved, query("j-1", &old, true, &[])),
        ExecutionReceipt::Absent
    );
}

#[test]
fn a_saved_acceptance_selects_its_dispatch_and_result() {
    let events = vec![
        dispatched("pj-1", "slot-1", 10),
        collected("pj-1", "slot-1", &finished()),
    ];
    let saved = saved_at(5, Some(accepted("pj-1")));
    let receipt = select(&saved, query("j-1", &events, true, &[]));
    let ExecutionReceipt::Terminal(result) = receipt else {
        panic!("expected terminal, got {receipt:?}");
    };
    assert_eq!(result.dispatch.pool_job, "pj-1");
    assert_eq!(result.dispatch.task, TASK);
    assert_eq!(result.dispatch.pool, "studio");
    assert_eq!(result.dispatch.slot, "slot-1");
    assert_eq!(result.outcome, "finished");
    assert!(result.note.contains("\"outcome\":\"finished\""));
}

#[test]
fn an_open_saved_dispatch_without_a_result_reads_dispatched() {
    let events = vec![dispatched("pj-1", "slot-1", 10)];
    let saved = saved_at(5, Some(accepted("pj-1")));
    let receipt = select(&saved, query("j-1", &events, true, &[]));
    let ExecutionReceipt::Dispatched(selected) = receipt else {
        panic!("expected dispatched, got {receipt:?}");
    };
    assert_eq!(selected.pool_job, "pj-1");
}

#[test]
fn pool_and_slot_reuse_select_by_the_pool_generated_job() {
    // slot-1 runs two jobs one after the other; each saved acceptance
    // selects its own dispatch and result.
    let events = vec![
        dispatched("pj-1", "slot-1", 10),
        collected("pj-1", "slot-1", &finished()),
        dispatched("pj-2", "slot-1", 500),
        collected("pj-2", "slot-1", &Outcome::Timeout),
    ];
    let first = select(
        &saved_at(0, Some(accepted("pj-1"))),
        query("j-1", &events, true, &[]),
    );
    let second = select(
        &saved_at(0, Some(accepted("pj-2"))),
        query("j-2", &events, true, &[]),
    );
    let ExecutionReceipt::Terminal(first) = first else {
        panic!("expected terminal for pj-1");
    };
    let ExecutionReceipt::Terminal(second) = second else {
        panic!("expected terminal for pj-2");
    };
    assert_eq!(first.dispatch.pool_job, "pj-1");
    assert_eq!(first.outcome, "finished");
    assert_eq!(second.dispatch.pool_job, "pj-2");
    assert_eq!(second.outcome, "timeout");
}

#[test]
fn unrelated_notes_after_the_result_do_not_disturb_selection() {
    let events = vec![
        dispatched("pj-1", "slot-1", 10),
        collected("pj-1", "slot-1", &finished()),
        dispatched("pj-9", "slot-2", 900),
        collected("pj-9", "slot-2", &Outcome::Lost),
        dispatched("pj-8", "slot-1", 950),
    ];
    let saved = saved_at(0, Some(accepted("pj-1")));
    let receipt = select(&saved, query("j-1", &events, true, &[]));
    let ExecutionReceipt::Terminal(result) = receipt else {
        panic!("expected terminal, got {receipt:?}");
    };
    assert_eq!(result.dispatch.pool_job, "pj-1");
    assert_eq!(result.outcome, "finished");
}

#[test]
fn mismatched_saved_records_stay_ambiguous() {
    let events = vec![dispatched("pj-1", "slot-1", 10)];
    let mut wrong_slot = accepted("pj-1");
    wrong_slot.slot = Some("slot-2".into());
    assert!(matches!(
        select(
            &saved_at(0, Some(wrong_slot)),
            query("j-1", &events, true, &[])
        ),
        ExecutionReceipt::Ambiguous(_)
    ));
    let mut wrong_pool = accepted("pj-1");
    wrong_pool.pool = Some("cloud".into());
    assert!(matches!(
        select(
            &saved_at(0, Some(wrong_pool)),
            query("j-1", &events, true, &[])
        ),
        ExecutionReceipt::Ambiguous(_)
    ));
    assert!(matches!(
        select(
            &saved_at(0, Some(accepted("pj-404"))),
            query("j-1", &events, true, &[])
        ),
        ExecutionReceipt::Ambiguous(_)
    ));
    let mut wrong_task = accepted("pj-1");
    wrong_task.task = Some("t-other".into());
    assert!(matches!(
        select(
            &saved_at(0, Some(wrong_task)),
            query("j-1", &events, true, &[])
        ),
        ExecutionReceipt::Ambiguous(_)
    ));
}

#[test]
fn a_result_note_that_disagrees_with_its_dispatch_stays_ambiguous() {
    let events = vec![
        dispatched("pj-1", "slot-1", 10),
        collected_on(TASK, "pj-1", "slot-9", "finished"),
    ];
    let saved = saved_at(0, Some(accepted("pj-1")));
    assert!(matches!(
        select(&saved, query("j-1", &events, true, &[])),
        ExecutionReceipt::Ambiguous(_)
    ));
}

#[test]
fn one_consistent_new_dispatch_is_adopted_without_a_saved_acceptance() {
    // An old dispatch (and its result) predates this execution's start
    // evidence; the one dispatch after it is the consistent new one.
    let events = vec![
        dispatched("pj-old", "slot-1", 5),
        collected("pj-old", "slot-1", &finished()),
        dispatched("pj-new", "slot-1", 50),
    ];
    let saved = saved_at(20, None);
    let receipt = select(&saved, query("j-1", &events, true, &[]));
    let ExecutionReceipt::Dispatched(selected) = receipt else {
        panic!("expected dispatched, got {receipt:?}");
    };
    assert_eq!(selected.pool_job, "pj-new");

    let mut with_result = events.clone();
    with_result.push(collected("pj-new", "slot-1", &finished()));
    let ExecutionReceipt::Terminal(result) = select(&saved, query("j-1", &with_result, true, &[]))
    else {
        panic!("expected terminal");
    };
    assert_eq!(result.dispatch.pool_job, "pj-new");
}

#[test]
fn an_old_dispatch_is_never_adopted_without_evidence_of_its_own() {
    let old = vec![
        dispatched("pj-old", "slot-1", 5),
        collected("pj-old", "slot-1", &finished()),
    ];
    // No saved evidence at all: absent, never dispatched.
    assert_eq!(
        select(&SavedDispatch::default(), query("j-1", &old, true, &[])),
        ExecutionReceipt::Absent
    );
    // Start evidence exists but every thread dispatch predates it: the old
    // dispatch belongs to the earlier execution; this one stays unstarted.
    let saved = saved_at(20, None);
    assert_eq!(
        select(&saved, query("j-1", &old, true, &[])),
        ExecutionReceipt::Unstarted
    );
    // A post-start dispatch that is already a prior identity is not
    // adopted either.
    let reused = vec![dispatched("pj-old", "slot-1", 50)];
    let prior = ["pj-old".to_owned()];
    assert_eq!(
        select(&saved, query("j-1", &reused, true, &prior)),
        ExecutionReceipt::Unstarted
    );
}

#[test]
fn multiple_post_start_dispatches_stay_ambiguous() {
    let events = vec![
        dispatched("pj-a", "slot-1", 50),
        dispatched("pj-b", "slot-2", 60),
    ];
    let saved = saved_at(20, None);
    let receipt = select(&saved, query("j-1", &events, true, &[]));
    let ExecutionReceipt::Ambiguous(reason) = receipt else {
        panic!("expected ambiguous, got {receipt:?}");
    };
    assert!(reason.contains("none can be preferred"));
}

#[test]
fn a_post_start_dispatch_without_the_pump_brief_stays_ambiguous() {
    let events = vec![dispatched("pj-new", "slot-1", 50)];
    let saved = saved_at(20, None);
    assert!(matches!(
        select(&saved, query("j-1", &events, false, &[])),
        ExecutionReceipt::Ambiguous(_)
    ));
}

#[test]
fn notes_of_other_tasks_are_ignored() {
    let events = vec![
        dispatched("pj-1", "slot-1", 10),
        PoolEventRow {
            handle: "t-other".into(),
            event_type: "pool_dispatched".into(),
            note: dispatch_note(&record("pj-ghost", "slot-1", 11)),
        },
        collected("pj-1", "slot-1", &finished()),
    ];
    let saved = saved_at(0, Some(accepted("pj-1")));
    let receipt = select(&saved, query("j-1", &events, true, &[]));
    let ExecutionReceipt::Terminal(result) = receipt else {
        panic!("expected terminal, got {receipt:?}");
    };
    assert_eq!(result.dispatch.pool_job, "pj-1");
}

#[test]
fn saved_evidence_round_trips_through_the_store() {
    let dir = tempdir().expect("tempdir");
    drop(store(&dir));
    save_evidence(&dir, "j-1", 42, TASK, "pj-1");
    let store = store(&dir);
    let saved = load(&store, "j-1").expect("load");
    assert_eq!(saved.started_at, Some(42));
    assert_eq!(saved.accepted, Some(accepted("pj-1")));
    assert_eq!(
        load(&store, "j-none").expect("load missing"),
        SavedDispatch::default()
    );
    // The full reader reaches the same receipt through the store alone.
    let events = vec![dispatched("pj-1", "slot-1", 50)];
    let through_store = receipt(&store, query("j-1", &events, true, &[])).expect("receipt");
    assert_eq!(
        through_store,
        select(&saved, query("j-1", &events, true, &[]))
    );
    let ExecutionReceipt::Dispatched(selected) = through_store else {
        panic!("expected dispatched, got {through_store:?}");
    };
    assert_eq!(selected.pool_job, "pj-1");
}
