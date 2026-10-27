//! Tests for the recovery-intent table methods in [`super`].

use tempfile::{TempDir, tempdir};

use super::{RecoveryIntentRow, recovery_state};
use crate::pump::flow::index::StepJobRow;
use crate::pump::jobs::JobSpec;
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};

/// A complete replacement request: every field carries something.
fn full_spec(id: &str, kind: &str) -> JobSpec {
    let mut spec = JobSpec::new(id, kind, "hand", "retry the slice", "full brief");
    spec.pool_candidates = vec!["fast".into(), "slow".into()];
    spec.local_only = true;
    spec.pool_exclude = vec!["flaky".into()];
    spec.target = Some("vivi".into());
    spec.extra_targets = vec!["docs".into()];
    spec.from_ref = Some("main".into());
    spec.with = vec!["notes".into()];
    spec.model = Some("m".into());
    spec.command = Some("true".into());
    spec.builtin_role = Some("hand".into());
    spec.branch = Some("work".into());
    spec.allow_default_branch = true;
    spec.publish_ref = Some("refs/heads/work".into());
    spec.publish_expect = Some("sha1".into());
    spec.publish_contains = Some("sha0".into());
    spec.delete_ref = Some("refs/heads/old".into());
    spec.delete_expect = Some("sha2".into());
    spec.budget = 900;
    spec.read_only = false;
    spec.units = vec!["u1".into()];
    spec.source_jobs = vec!["j0".into()];
    spec.payload = serde_json::json!({"slice": 1, "why": "timeout"});
    spec.task = Some("task-9".into());
    spec
}

fn row(failed: &str, ordinal: u32) -> RecoveryIntentRow {
    let replacement = format!("{failed}-r{ordinal}");
    RecoveryIntentRow {
        need: "n1".into(),
        run: 3,
        node: "slice".into(),
        task: "t1".into(),
        input: "t1:abc123".into(),
        failed_job: failed.into(),
        replacement_job: replacement.clone(),
        kind: "slice".into(),
        ordinal,
        reason: "timeout".into(),
        deadline: 1_700,
        request: full_spec(&replacement, "slice"),
        step_job: Some(StepJobRow {
            job: replacement,
            run: 3,
            node: "slice".into(),
            task: "t1".into(),
            attempt: 2,
        }),
        state: recovery_state::PENDING.into(),
        applied_at: None,
    }
}

/// A reslice intent: no step-job binding, its own kind, its own step input.
fn reslice_row(failed: &str) -> RecoveryIntentRow {
    let mut intent = row(failed, 1);
    intent.task = "t2".into();
    intent.input = "t2:def456".into();
    intent.kind = "reslice".into();
    intent.step_job = None;
    intent.request = full_spec(&intent.replacement_job, "reslice");
    intent
}

fn open(dir: &TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open")
}

#[test]
fn full_request_round_trips() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let intent = row("t1-a2", 1);
    let stored = store.insert_recovery_intent(&intent).expect("insert");
    assert_eq!(stored, intent);
    assert_eq!(
        store.recovery_intent("t1-a2-r1").expect("read"),
        Some(intent.clone())
    );
    assert_eq!(
        store
            .recovery_intent_of_failed_job("t1-a2")
            .expect("read by failed job"),
        Some(intent)
    );
    assert_eq!(
        store.recovery_intent("nobody").expect("read"),
        None,
        "unknown replacement id"
    );
}

#[test]
fn unbound_reslice_intent_round_trips_without_step_job() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let intent = reslice_row("t1-a2");
    store.insert_recovery_intent(&intent).expect("insert");
    let back = store
        .recovery_intent("t1-a2-r1")
        .expect("read")
        .expect("row");
    assert_eq!(back.kind, "reslice");
    assert!(
        back.step_job.is_none(),
        "reslice is not an indexed step job"
    );
    assert_eq!(
        store.pending_recoveries("n1", "reslice").expect("by kind"),
        vec![intent]
    );
    assert!(
        store
            .pending_recoveries("n1", "slice")
            .expect("by kind")
            .is_empty()
    );
}

#[test]
fn duplicate_insert_returns_the_stored_intent() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let intent = row("t1-a2", 1);
    store.insert_recovery_intent(&intent).expect("first");
    let mut replay = row("t1-a2", 1);
    replay.deadline = 9_999;
    replay.reason = "host".into();
    replay.replacement_job = "t1-a2-rX".into();
    replay.request = full_spec("t1-a2-rX", "slice");
    let again = store.insert_recovery_intent(&replay).expect("replay");
    assert_eq!(again, intent, "the stored row wins, deadline included");
    assert_eq!(
        store.recovery_intent("t1-a2-r1").expect("read"),
        Some(intent)
    );
    assert_eq!(
        store.recovery_intent("t1-a2-rX").expect("read"),
        None,
        "the replayed identity was not written"
    );
}

#[test]
fn second_failed_job_cannot_steal_a_replacement_id_or_ordinal() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store
        .insert_recovery_intent(&row("t1-a2", 1))
        .expect("first");
    let mut same_id = row("t1-a3", 1);
    same_id.replacement_job = "t1-a2-r1".into();
    assert!(
        store.insert_recovery_intent(&same_id).is_err(),
        "one intent per replacement id"
    );
    assert!(
        store.insert_recovery_intent(&row("t1-a3", 1)).is_err(),
        "one ordinal per step input"
    );
    let mut other_input = row("t2-a1", 1);
    other_input.task = "t2".into();
    other_input.input = "t2:def456".into();
    store
        .insert_recovery_intent(&other_input)
        .expect("a different step input may spend ordinal 1");
}

#[test]
fn allowance_lookup_spends_one_ordinal_per_step_input() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    assert_eq!(
        store.recovery_allowance("t1", "t1:abc123").expect("empty"),
        0,
        "absence means no allowance spent"
    );
    store
        .insert_recovery_intent(&row("t1-a2", 1))
        .expect("first");
    assert_eq!(store.recovery_allowance("t1", "t1:abc123").expect("one"), 1);
    store
        .insert_recovery_intent(&row("t1-a3", 2))
        .expect("second");
    assert_eq!(store.recovery_allowance("t1", "t1:abc123").expect("two"), 2);
    assert_eq!(
        store
            .recovery_allowance("t1", "t1:other")
            .expect("other input"),
        0
    );
}

#[test]
fn applied_intents_leave_the_unfinished_queries() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.insert_recovery_intent(&row("t1-a2", 1)).expect("one");
    store
        .insert_recovery_intent(&reslice_row("t2-a1"))
        .expect("two");
    store
        .mark_recovery_applied("t1-a2-r1", 2_000)
        .expect("mark");
    let unfinished = store.unfinished_recovery_intents().expect("unfinished");
    assert_eq!(unfinished.len(), 1);
    assert_eq!(unfinished[0].replacement_job, "t2-a1-r1");
    assert!(
        store
            .pending_recoveries("n1", "slice")
            .expect("slice")
            .is_empty()
    );
    let applied = store
        .recovery_intent("t1-a2-r1")
        .expect("read")
        .expect("row");
    assert_eq!(applied.state, recovery_state::APPLIED);
    assert_eq!(applied.applied_at, Some(2_000));
}

#[test]
fn marking_applied_is_idempotent_and_refuses_unknown_rows() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    assert!(
        store.mark_recovery_applied("ghost", 1).is_err(),
        "no intent names the replacement job"
    );
    store
        .insert_recovery_intent(&row("t1-a2", 1))
        .expect("insert");
    store
        .mark_recovery_applied("t1-a2-r1", 2_000)
        .expect("mark");
    store
        .mark_recovery_applied("t1-a2-r1", 3_000)
        .expect("re-mark");
    let applied = store
        .recovery_intent("t1-a2-r1")
        .expect("read")
        .expect("row");
    assert_eq!(applied.applied_at, Some(2_000), "the first stamp survives");
}

#[test]
fn legacy_empty_database_gains_the_table_on_open() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join(DB_FILENAME);
    {
        let conn = rusqlite::Connection::open(&path).expect("open legacy db");
        conn.execute_batch(super::super::SCHEMA_DDL)
            .expect("legacy v8 schema");
        conn.pragma_update(None, "user_version", 8)
            .expect("stamp v8");
    }
    let store = PumpStore::open(&path).expect("open");
    assert!(
        store
            .unfinished_recovery_intents()
            .expect("query")
            .is_empty()
    );
    assert_eq!(store.recovery_allowance("t1", "in").expect("allowance"), 0);
    store
        .insert_recovery_intent(&row("t1-a2", 1))
        .expect("insert");
    assert_eq!(
        store
            .recovery_intent("t1-a2-r1")
            .expect("read")
            .expect("row"),
        row("t1-a2", 1)
    );
}

#[test]
fn save_and_reopen_preserve_intents() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join(DB_FILENAME);
    let intent = row("t1-a2", 1);
    {
        let mut store = PumpStore::open(&path).expect("open");
        store.insert_recovery_intent(&intent).expect("insert");
        store.save(&State::default()).expect("save");
        store.save(&State::default()).expect("save again");
    }
    let store = PumpStore::open(&path).expect("reopen");
    assert_eq!(
        store.recovery_intent("t1-a2-r1").expect("read"),
        Some(intent),
        "the state save never touches recovery intents"
    );
    assert_eq!(
        store.recovery_allowance("t1", "t1:abc123").expect("spent"),
        1
    );
}
