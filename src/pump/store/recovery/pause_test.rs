//! Tests for the pause boundary and continuation journal store methods.

use tempfile::{TempDir, tempdir};

use super::{PauseBoundaryRow, PauseContinuation, PauseJournalReply, journal_stage, pause_state};
use crate::pump::jobs::JobSpec;
use crate::pump::state::State;
use crate::pump::store::recovery::{RecoveryIntentRow, recovery_state};
use crate::pump::store::{DB_FILENAME, PumpStore};

fn open(dir: &TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open")
}

fn boundary(origin: &str, generation: u32) -> PauseBoundaryRow {
    PauseBoundaryRow {
        need: "n1".into(),
        run: 3,
        node: "slice".into(),
        origin_task: origin.into(),
        gate: "answer-gate".into(),
        pause_generation: generation,
        input_generation: 7,
        role_request: format!("{origin}-request"),
        boundary_message: None,
        paused_at: 1_700,
        state: pause_state::OPEN.into(),
    }
}

fn reply(id: &str) -> PauseJournalReply {
    PauseJournalReply {
        id: id.into(),
        author: "hand".into(),
        body: format!("body of {id}"),
    }
}

fn continuation(replacement: &str, replies: &[PauseJournalReply]) -> PauseContinuation {
    let mut request = JobSpec::new(replacement, "gate", "hand", "continue the slice", "brief");
    request.payload = serde_json::json!({"gate": "answer-gate"});
    request.budget = 600;
    PauseContinuation {
        replacement_request: replacement.into(),
        input_message: format!("{replacement}-input"),
        answers: serde_json::json!({"answer": "ship it"}),
        replies: replies.to_vec(),
        request,
        attempt: 1,
    }
}

/// A minimal transient recovery intent of run 3, for invalidation coverage.
fn intent(failed: &str) -> RecoveryIntentRow {
    RecoveryIntentRow {
        need: "n1".into(),
        run: 3,
        node: "slice".into(),
        task: "t1".into(),
        input: "t1:abc123".into(),
        failed_job: failed.into(),
        replacement_job: format!("{failed}-r1"),
        kind: "slice".into(),
        ordinal: 1,
        reason: "timeout".into(),
        deadline: 1_800,
        request: JobSpec::new(format!("{failed}-r1"), "slice", "hand", "retry", "brief"),
        step_job: None,
        state: recovery_state::PENDING.into(),
        applied_at: None,
    }
}

#[test]
fn record_and_adopt_keeps_the_first_boundary() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let pause = boundary("t1", 1);
    assert_eq!(store.record_pause(&pause).expect("record"), pause);
    assert_eq!(
        store
            .pause_boundary("n1", 3, "slice", "t1", 1)
            .expect("read"),
        Some(pause.clone())
    );
    assert_eq!(
        store
            .latest_pause_boundary("n1", 3, "slice", "t1")
            .expect("read"),
        Some(pause.clone())
    );
    assert_eq!(
        store
            .pause_boundary("n1", 3, "slice", "t1", 2)
            .expect("read"),
        None
    );
    assert!(pause.boundary_message.is_none(), "no snapshot yet");
    let mut replay = boundary("t1", 1);
    replay.gate = "other-gate".into();
    replay.input_generation = 99;
    replay.role_request = "other-request".into();
    replay.paused_at = 9_999;
    assert_eq!(
        store.record_pause(&replay).expect("adopt"),
        pause,
        "the stored boundary wins"
    );
    let stored = store
        .latest_pause_boundary("n1", 3, "slice", "t1")
        .expect("read")
        .expect("row");
    assert_eq!(stored.gate, "answer-gate");
    assert_eq!(stored.input_generation, 7);
    assert_eq!(stored.paused_at, 1_700);
}

#[test]
fn pause_generations_move_forward() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.record_pause(&boundary("t1", 2)).expect("gen 2");
    assert!(
        store.record_pause(&boundary("t1", 1)).is_err(),
        "an older, unrecorded generation is refused"
    );
    let mut replay = boundary("t1", 2);
    replay.paused_at = 5_000;
    assert_eq!(
        store.record_pause(&replay).expect("adopt"),
        boundary("t1", 2),
        "the newest generation is adopted on replay"
    );
    store.record_pause(&boundary("t1", 3)).expect("gen 3");
    assert_eq!(
        store
            .latest_pause_boundary("n1", 3, "slice", "t1")
            .expect("read")
            .expect("row")
            .pause_generation,
        3
    );
    assert_eq!(
        store
            .pause_boundary("n1", 3, "slice", "t1", 2)
            .expect("read")
            .expect("row")
            .paused_at,
        1_700,
        "older generations stay readable"
    );
}

#[test]
fn snapshot_stamps_the_boundary_once() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    assert!(
        store
            .snapshot_input("n1", 3, "slice", "ghost", 1, "msg-1")
            .is_err(),
        "no pause carries the identity"
    );
    store.record_pause(&boundary("t1", 1)).expect("record");
    assert_eq!(
        store
            .snapshot_input("n1", 3, "slice", "t1", 1, "msg-1")
            .expect("snapshot"),
        "msg-1"
    );
    assert_eq!(
        store
            .snapshot_input("n1", 3, "slice", "t1", 1, "msg-1")
            .expect("repeat"),
        "msg-1"
    );
    assert!(
        store
            .snapshot_input("n1", 3, "slice", "t1", 1, "msg-2")
            .is_err(),
        "the boundary never moves"
    );
    let stored = store
        .pause_boundary("n1", 3, "slice", "t1", 1)
        .expect("read")
        .expect("row");
    assert_eq!(stored.boundary_message.as_deref(), Some("msg-1"));
}

#[test]
fn reservation_needs_a_recorded_snapshot() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    assert!(
        store
            .reserve_continuation("n1", 3, "slice", "t1", 1, &continuation("c1", &[]))
            .is_err(),
        "no pause to continue"
    );
    store.record_pause(&boundary("t1", 1)).expect("record");
    assert!(
        store
            .reserve_continuation("n1", 3, "slice", "t1", 1, &continuation("c1", &[]))
            .is_err(),
        "the pause has no input snapshot"
    );
    assert_eq!(store.pause_journal("c1").expect("read"), None);
    store
        .snapshot_input("n1", 3, "slice", "t1", 1, "msg-1")
        .expect("snapshot");
    let journal = store
        .reserve_continuation("n1", 3, "slice", "t1", 1, &continuation("c1", &[]))
        .expect("reserve");
    assert_eq!(journal.boundary_message, "msg-1");
    assert_eq!(journal.gate, "answer-gate");
    assert_eq!(journal.input_generation, 7);
    assert_eq!(journal.role_request, "t1-request");
    assert_eq!(journal.pause_generation, 1);
    assert_eq!(journal.paused_at, 1_700);
    assert_eq!(journal.state, recovery_state::PENDING);
    assert_eq!(journal.reopened_at, None);
    assert_eq!(journal.input_message, "c1-input");
}

#[test]
fn duplicate_reservation_returns_the_stored_journal() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.record_pause(&boundary("t1", 1)).expect("record");
    store
        .snapshot_input("n1", 3, "slice", "t1", 1, "msg-1")
        .expect("snapshot");
    let stored = store
        .reserve_continuation(
            "n1",
            3,
            "slice",
            "t1",
            1,
            &continuation("c1", &[reply("m1")]),
        )
        .expect("reserve");
    let mut replay = continuation("c1", &[reply("m9")]);
    replay.input_message = "other-input".into();
    replay.answers = serde_json::json!({"answer": "different"});
    replay.attempt = 4;
    replay.request = JobSpec::new("c1", "gate", "hand", "other", "other");
    let again = store
        .reserve_continuation("n1", 3, "slice", "t1", 1, &replay)
        .expect("replay");
    assert_eq!(again, stored, "the stored journal wins");
    assert_eq!(
        store
            .consumed_pause_replies("n1", 3, "slice", "t1")
            .expect("replies"),
        vec!["m1".to_owned()],
        "the replay consumed nothing further"
    );
}

#[test]
fn one_origin_cannot_consume_a_reply_twice() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.record_pause(&boundary("t1", 1)).expect("record");
    store
        .snapshot_input("n1", 3, "slice", "t1", 1, "msg-1")
        .expect("snapshot");
    store
        .reserve_continuation(
            "n1",
            3,
            "slice",
            "t1",
            1,
            &continuation("c1", &[reply("m1")]),
        )
        .expect("reserve");
    assert!(
        store
            .reserve_continuation(
                "n1",
                3,
                "slice",
                "t1",
                1,
                &continuation("c2", &[reply("m1")])
            )
            .is_err(),
        "one consumption per origin and reply"
    );
    assert_eq!(
        store.pause_journal("c2").expect("read"),
        None,
        "the refused journal was not written"
    );
    store
        .reserve_continuation(
            "n1",
            3,
            "slice",
            "t1",
            1,
            &continuation("c3", &[reply("m2")]),
        )
        .expect("a fresh reply still works");
}

#[test]
fn the_same_reply_may_serve_separate_origins() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.record_pause(&boundary("t1", 1)).expect("t1");
    store.record_pause(&boundary("t2", 1)).expect("t2");
    store
        .snapshot_input("n1", 3, "slice", "t1", 1, "msg-1")
        .expect("snapshot t1");
    store
        .snapshot_input("n1", 3, "slice", "t2", 1, "msg-2")
        .expect("snapshot t2");
    store
        .reserve_continuation(
            "n1",
            3,
            "slice",
            "t1",
            1,
            &continuation("c1", &[reply("m1")]),
        )
        .expect("reserve for t1");
    store
        .reserve_continuation(
            "n1",
            3,
            "slice",
            "t2",
            1,
            &continuation("c2", &[reply("m1")]),
        )
        .expect("the same reply may serve another origin");
    assert_eq!(
        store
            .consumed_pause_replies("n1", 3, "slice", "t1")
            .expect("t1"),
        vec!["m1".to_owned()]
    );
    assert_eq!(
        store
            .consumed_pause_replies("n1", 3, "slice", "t2")
            .expect("t2"),
        vec!["m1".to_owned()]
    );
    assert_eq!(
        store.unfinished_pause_journals().expect("unfinished").len(),
        2
    );
}

#[test]
fn journal_stages_and_application_stamp_once() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    assert!(
        store
            .mark_journal_stage("ghost", journal_stage::REOPENED, 1)
            .is_err(),
        "no journal names the replacement request"
    );
    assert!(
        store.mark_journal_stage("ghost", "rewound", 1).is_err(),
        "unknown stage word"
    );
    store.record_pause(&boundary("t1", 1)).expect("record");
    store
        .snapshot_input("n1", 3, "slice", "t1", 1, "msg-1")
        .expect("snapshot");
    store
        .reserve_continuation("n1", 3, "slice", "t1", 1, &continuation("c1", &[]))
        .expect("reserve");
    assert_eq!(
        store.unfinished_pause_journals().expect("unfinished").len(),
        1
    );
    for (stage, at) in [
        (journal_stage::REOPENED, 1_100),
        (journal_stage::GATE_RETIRED, 1_200),
        (journal_stage::ENQUEUED, 1_300),
    ] {
        store.mark_journal_stage("c1", stage, at).expect("stamp");
        store
            .mark_journal_stage("c1", stage, at + 1_000)
            .expect("re-stamp");
    }
    let stamped = store.pause_journal("c1").expect("read").expect("row");
    assert_eq!(stamped.reopened_at, Some(1_100), "the first stamp survives");
    assert_eq!(stamped.gate_retired_at, Some(1_200));
    assert_eq!(stamped.enqueued_at, Some(1_300));
    store
        .mark_pause_journal_applied("c1", 2_000)
        .expect("mark applied");
    store
        .mark_pause_journal_applied("c1", 3_000)
        .expect("re-mark applied");
    assert!(
        store
            .unfinished_pause_journals()
            .expect("unfinished")
            .is_empty()
    );
    let applied = store.pause_journal("c1").expect("read").expect("row");
    assert_eq!(applied.state, recovery_state::APPLIED);
    assert_eq!(applied.applied_at, Some(2_000), "the first stamp survives");
    assert_eq!(applied.enqueued_at, Some(1_300));
    assert!(store.mark_pause_journal_applied("ghost", 1).is_err());
}

#[test]
fn retire_pause_moves_the_boundary_out_of_open() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    assert!(store.retire_pause("n1", 3, "slice", "ghost", 1).is_err());
    store.record_pause(&boundary("t1", 1)).expect("record");
    store
        .retire_pause("n1", 3, "slice", "t1", 1)
        .expect("retire");
    store
        .retire_pause("n1", 3, "slice", "t1", 1)
        .expect("idempotent");
    let stored = store
        .pause_boundary("n1", 3, "slice", "t1", 1)
        .expect("read")
        .expect("row");
    assert_eq!(stored.state, pause_state::RETIRED);
}

#[test]
fn save_and_reopen_preserve_pauses_and_journals() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join(DB_FILENAME);
    let pause = boundary("t1", 1);
    let journal;
    {
        let mut store = PumpStore::open(&path).expect("open");
        store.record_pause(&pause).expect("record");
        store
            .snapshot_input("n1", 3, "slice", "t1", 1, "msg-1")
            .expect("snapshot");
        journal = store
            .reserve_continuation(
                "n1",
                3,
                "slice",
                "t1",
                1,
                &continuation("c1", &[reply("m1"), reply("m2")]),
            )
            .expect("reserve");
        store.save(&State::default()).expect("save");
        store.save(&State::default()).expect("save again");
    }
    let store = PumpStore::open(&path).expect("reopen");
    let mut expected_boundary = pause;
    expected_boundary.boundary_message = Some("msg-1".into());
    assert_eq!(
        store
            .pause_boundary("n1", 3, "slice", "t1", 1)
            .expect("read"),
        Some(expected_boundary),
        "the state save never touches pause boundaries"
    );
    assert_eq!(
        store.pause_journal("c1").expect("read"),
        Some(journal),
        "the state save never touches continuation journals"
    );
    assert_eq!(
        store.unfinished_pause_journals().expect("unfinished").len(),
        1
    );
}

#[test]
fn invalidation_cancels_transient_and_continuation_records() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store
        .insert_recovery_intent(&intent("t1-a2"))
        .expect("pending");
    let mut applied = intent("t1-a3");
    applied.ordinal = 2;
    applied.input = "t1:other".into();
    store.insert_recovery_intent(&applied).expect("applied");
    store
        .mark_recovery_applied("t1-a3-r1", 1_500)
        .expect("mark applied");
    applied.state = recovery_state::APPLIED.into();
    applied.applied_at = Some(1_500);
    store.record_pause(&boundary("t1", 1)).expect("run 3 pause");
    store
        .snapshot_input("n1", 3, "slice", "t1", 1, "msg-1")
        .expect("snapshot");
    store
        .reserve_continuation(
            "n1",
            3,
            "slice",
            "t1",
            1,
            &continuation("c1", &[reply("m1")]),
        )
        .expect("reserve");
    let mut run4 = boundary("t9", 1);
    run4.run = 4;
    run4.role_request = "t9-request".into();
    store.record_pause(&run4).expect("run 4 pause");

    let removed = store.invalidate_run_recovery("n1", 3).expect("invalidate");
    assert_eq!(removed.intents, 1, "only the pending intent is cancelled");
    assert_eq!(removed.boundaries, 1);
    assert_eq!(removed.replies, 1);
    assert_eq!(removed.journals, 1);
    assert_eq!(store.recovery_intent("t1-a2-r1").expect("read"), None);
    assert_eq!(
        store.recovery_intent_of_failed_job("t1-a3").expect("read"),
        Some(applied),
        "applied intents stay as receipts"
    );
    assert_eq!(store.pause_journal("c1").expect("read"), None);
    assert!(
        store
            .unfinished_pause_journals()
            .expect("unfinished")
            .is_empty()
    );
    assert_eq!(
        store
            .pause_boundary("n1", 3, "slice", "t1", 1)
            .expect("read"),
        None
    );
    assert!(
        store
            .consumed_pause_replies("n1", 3, "slice", "t1")
            .expect("replies")
            .is_empty()
    );
    assert_eq!(
        store
            .pause_boundary("n1", 4, "slice", "t9", 1)
            .expect("read"),
        Some(run4),
        "another run keeps its pause"
    );
}

#[test]
fn legacy_empty_database_gains_the_tables_on_open() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join(DB_FILENAME);
    {
        let conn = rusqlite::Connection::open(&path).expect("open legacy db");
        conn.execute_batch(super::super::super::SCHEMA_DDL)
            .expect("legacy v8 schema");
        conn.pragma_update(None, "user_version", 8)
            .expect("stamp v8");
    }
    let store = PumpStore::open(&path).expect("open");
    assert!(store.unfinished_pause_journals().expect("query").is_empty());
    store.record_pause(&boundary("t1", 1)).expect("record");
    assert_eq!(
        store
            .pause_boundary("n1", 3, "slice", "t1", 1)
            .expect("read"),
        Some(boundary("t1", 1))
    );
}
