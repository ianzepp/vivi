//! Tests for the phase-aware step execution generations in
//! [`crate::pump::store::step_execution`].

use tempfile::tempdir;

use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState, run_state};
use crate::pump::state::State;
use crate::pump::store::step_execution::ExecutionKey;
use crate::pump::store::{DB_FILENAME, PumpStore, SCHEMA_VERSION};

const HAND: &str = "hand";
const MERGE: &str = "merge";

fn key(need: &str, run: u32, task: &str, phase: &str) -> ExecutionKey {
    ExecutionKey::new(need, run, task, phase)
}

fn hand(need: &str, run: u32, task: &str) -> ExecutionKey {
    key(need, run, task, HAND)
}

fn run(need: &str, n: u32) -> RunRow {
    RunRow {
        need: need.into(),
        run: n,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "admit".into(),
        state: run_state::LIVE.into(),
    }
}

fn step(n: u32, node: &str, task: &str) -> StepRow {
    StepRow {
        run: n,
        node: node.into(),
        task: task.into(),
        state: StepState::Pending,
        verdict: None,
    }
}

fn job(job: &str, run: u32, node: &str, task: &str, attempt: u32) -> StepJobRow {
    StepJobRow {
        job: job.into(),
        run,
        node: node.into(),
        task: task.into(),
        attempt,
    }
}

fn open() -> (tempfile::TempDir, PumpStore) {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    (dir, store)
}

fn reopen(dir: &tempfile::TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("reopen")
}

#[test]
fn registration_is_idempotent_and_round_trips_through_reopen() {
    let (dir, store) = open();
    let k = hand("n1", 1, "t1");
    let first = store.register_execution(&k).expect("register");
    assert_eq!(first.generation, 1);
    assert_eq!(first.execution, "n1:r1:t1:hand:g1");
    assert_eq!(first.original_execution, first.execution);
    assert_eq!(first.attempt, None);
    assert!(!first.superseded);

    let again = store.register_execution(&k).expect("register again");
    assert_eq!(again, first);
    assert_eq!(
        store.active_execution(&k).expect("active").expect("some"),
        first
    );
    assert!(
        store
            .active_execution(&hand("n1", 1, "t9"))
            .expect("active")
            .is_none()
    );
    drop(store);
    let store = reopen(&dir);
    assert_eq!(
        store
            .execution("n1:r1:t1:hand:g1")
            .expect("by id")
            .expect("some"),
        first
    );
    assert!(
        store
            .execution("n1:r1:t1:hand:g2")
            .expect("by id")
            .is_none()
    );
}

#[test]
fn state_save_leaves_executions_phases_and_checkpoints_alone() {
    let (dir, mut store) = open();
    let k = hand("n1", 1, "t1");
    let row = store.register_execution(&k).expect("register");
    store
        .activate_job(&row.execution, "j1", 1)
        .expect("activate");
    store
        .set_input_checkpoint(&row.execution, "msg-42")
        .expect("checkpoint");
    store.set_current_phase("t1", "n1", 1, HAND).expect("phase");

    store.save(&State::default()).expect("save");
    store.save(&State::default()).expect("save again");
    let after = store
        .execution("n1:r1:t1:hand:g1")
        .expect("read")
        .expect("some");
    assert_eq!(after.active_job.as_deref(), Some("j1"));
    assert_eq!(after.attempt, Some(1));
    assert_eq!(after.original_job.as_deref(), Some("j1"));
    assert_eq!(after.original_attempt, Some(1));
    assert_eq!(after.input_checkpoint.as_deref(), Some("msg-42"));
    assert_eq!(
        store
            .current_phase("t1")
            .expect("phase")
            .expect("some")
            .phase,
        HAND
    );

    drop(store);
    let store = reopen(&dir);
    assert_eq!(
        store
            .execution("n1:r1:t1:hand:g1")
            .expect("read")
            .expect("some"),
        after
    );
}

#[test]
fn independent_needs_runs_and_phases_get_isolated_lineages() {
    let (dir, store) = open();
    let a = store.register_execution(&hand("n1", 1, "t1")).expect("a");
    let b = store.register_execution(&hand("n2", 1, "t1")).expect("b");
    let c = store.register_execution(&hand("n1", 2, "t1")).expect("c");
    let d = store
        .register_execution(&key("n1", 1, "t1", MERGE))
        .expect("d");
    for row in [&a, &b, &c, &d] {
        assert_eq!(row.generation, 1, "{row:?} starts its own lineage");
    }
    let ids = [&a, &b, &c, &d].map(|row| row.execution.clone());
    assert_eq!(
        ids.len(),
        std::collections::BTreeSet::from(ids.clone()).len()
    );

    store
        .activate_job(&a.execution, "j-a", 1)
        .expect("activate a");
    for row in [&b, &c, &d] {
        let got = store
            .execution(&row.execution)
            .expect("read")
            .expect("some");
        assert_eq!(got.active_job, None, "{row:?} is untouched");
    }
    drop(store);
    let store = reopen(&dir);
    assert_eq!(
        store
            .active_execution(&hand("n1", 1, "t1"))
            .expect("a")
            .expect("some"),
        store.execution(&a.execution).expect("read").expect("some")
    );
}

#[test]
fn activation_stamps_facts_and_rereading_the_same_job_is_idempotent() {
    let (dir, store) = open();
    let row = store
        .register_execution(&hand("n1", 1, "t1"))
        .expect("register");
    let activated = store
        .activate_job(&row.execution, "j1", 1)
        .expect("activate");
    assert_eq!(activated.active_job.as_deref(), Some("j1"));
    assert_eq!(activated.attempt, Some(1));
    assert_eq!(activated.original_job.as_deref(), Some("j1"));
    assert_eq!(activated.original_attempt, Some(1));
    assert_eq!(
        store
            .execution_by_job("j1")
            .expect("by job")
            .expect("some")
            .execution,
        row.execution
    );
    let again = store
        .activate_job(&row.execution, "j1", 1)
        .expect("idempotent re-activation");
    assert_eq!(again, activated);
    drop(store);
    let store = reopen(&dir);
    assert_eq!(
        store.execution_by_job("j1").expect("by job").expect("some"),
        again
    );

    let err = store
        .activate_job("n1:r1:t1:hand:g9", "j1", 1)
        .expect_err("unknown execution");
    assert!(err.to_string().contains("no such execution"), "{err}");
}

#[test]
fn a_retry_consumes_a_generation_and_keeps_the_original_facts() {
    let (dir, store) = open();
    let first = store
        .register_execution(&hand("n1", 1, "t1"))
        .expect("register");
    store
        .activate_job(&first.execution, "j1", 1)
        .expect("activate");
    store
        .set_input_checkpoint(&first.execution, "msg-7")
        .expect("checkpoint");

    let second = store.consume_retry(&first.execution).expect("retry");
    assert_eq!(second.generation, 2);
    assert_eq!(second.execution, "n1:r1:t1:hand:g2");
    assert_eq!(second.original_execution, first.execution);
    assert_eq!(second.original_job.as_deref(), Some("j1"));
    assert_eq!(second.retries, 1);
    assert_eq!(second.retry_history, vec![first.execution.clone()]);
    assert_eq!(second.input_checkpoint.as_deref(), Some("msg-7"));
    assert!(second.active_job.is_none());

    let stale = store
        .execution(&first.execution)
        .expect("read")
        .expect("some");
    assert!(stale.superseded);
    assert!(
        !store
            .execution_is_current(&first.execution)
            .expect("current")
    );
    let err = store
        .consume_retry(&first.execution)
        .expect_err("double retry");
    assert!(err.to_string().contains("superseded"), "{err}");

    store
        .activate_job(&second.execution, "j2", 2)
        .expect("activate 2");
    let active = store
        .active_execution(&hand("n1", 1, "t1"))
        .expect("active")
        .expect("some");
    assert_eq!(active.execution, second.execution);
    assert_eq!(
        active.original_job.as_deref(),
        Some("j1"),
        "lineage origin kept"
    );
    assert_eq!(
        store.register_execution(&hand("n1", 1, "t1")).expect("reg"),
        active
    );
    assert!(
        store
            .execution_is_current(&second.execution)
            .expect("current")
    );

    drop(store);
    let store = reopen(&dir);
    assert_eq!(
        store
            .execution(&second.execution)
            .expect("read")
            .expect("some")
            .retries,
        1
    );
}

#[test]
fn a_correction_shares_the_attempt_but_never_the_identity() {
    let (dir, store) = open();
    let first = store
        .register_execution(&hand("n1", 1, "t1"))
        .expect("register");
    store
        .activate_job(&first.execution, "j1", 1)
        .expect("activate");

    let second = store
        .start_correction(&first.execution)
        .expect("correction");
    assert_eq!(second.attempt, Some(1), "corrections share the attempt");
    assert_ne!(second.execution, first.execution, "but never the identity");
    assert_eq!(second.retries, 0, "a correction is not a retry");
    assert_eq!(second.retry_history, vec![first.execution.clone()]);
    assert_eq!(second.original_job.as_deref(), Some("j1"));

    let corrected = store
        .activate_job(&second.execution, "j1-c1", 1)
        .expect("activate correction");
    assert_eq!(corrected.active_job.as_deref(), Some("j1-c1"));
    assert_eq!(
        store
            .execution_by_job("j1-c1")
            .expect("by job")
            .expect("some")
            .execution,
        second.execution
    );
    let old = store.execution_by_job("j1").expect("by job").expect("some");
    assert_eq!(old.execution, first.execution);
    assert!(
        old.superseded,
        "the superseded row keeps its job for tracing"
    );
    assert!(
        !store
            .execution_is_current(&first.execution)
            .expect("current")
    );
    drop(store);
    let store = reopen(&dir);
    assert_eq!(
        store
            .execution(&second.execution)
            .expect("read")
            .expect("some"),
        corrected
    );
}

#[test]
fn stale_same_attempt_activation_is_refused() {
    let (dir, store) = open();
    let first = store
        .register_execution(&hand("n1", 1, "t1"))
        .expect("register");
    store
        .activate_job(&first.execution, "j1", 1)
        .expect("activate");
    let second = store
        .start_correction(&first.execution)
        .expect("correction");
    store
        .activate_job(&second.execution, "j1-c1", 1)
        .expect("activate correction");

    // Simulate adopted rows that never carried the superseded flag: the
    // generation guard alone must refuse the stale same-attempt activation.
    store
        .conn
        .execute(
            "UPDATE step_executions SET superseded = 0 WHERE generation = 1",
            [],
        )
        .expect("un-supersede");
    let err = store
        .activate_job(&first.execution, "j1", 1)
        .expect_err("stale same-attempt activation");
    let text = err.to_string();
    assert!(text.contains("newer execution"), "{text}");
    assert!(text.contains("attempt 1"), "{text}");
    let first_row = store
        .execution(&first.execution)
        .expect("read")
        .expect("some");
    assert_eq!(
        first_row.active_job.as_deref(),
        Some("j1"),
        "only the pre-refusal job; nothing new was activated"
    );
    assert_eq!(
        store
            .execution_by_job("j1-c1")
            .expect("by job")
            .expect("some")
            .execution,
        second.execution
    );
    drop(store);
    let store = reopen(&dir);
    assert_eq!(
        store
            .execution_by_job("j1-c1")
            .expect("by job")
            .expect("some")
            .execution,
        second.execution
    );
}

#[test]
fn an_old_phase_execution_cannot_become_current_after_the_merge_starts() {
    let (dir, store) = open();
    let k = hand("n1", 1, "t1");
    let row = store.register_execution(&k).expect("register");
    store
        .activate_job(&row.execution, "j1", 1)
        .expect("activate");
    assert_eq!(
        store
            .current_phase("t1")
            .expect("phase")
            .expect("some")
            .phase,
        HAND,
        "registration stamps the phase but never steals one"
    );

    store
        .set_current_phase("t1", "n1", 1, MERGE)
        .expect("merge starts");
    let err = store
        .activate_job(&row.execution, "j1-c9", 1)
        .expect_err("stale phase activation");
    assert!(err.to_string().contains("owned by phase merge"), "{err}");
    assert!(!store.execution_is_current(&row.execution).expect("current"));
    assert_eq!(
        store
            .register_execution(&key("n1", 1, "t1", MERGE))
            .expect("merge execution")
            .generation,
        1
    );
    assert_eq!(
        store
            .current_phase("t1")
            .expect("phase")
            .expect("some")
            .phase,
        MERGE,
        "registration does not steal the current phase"
    );

    let merge = store
        .register_execution(&key("n1", 1, "t1", MERGE))
        .expect("merge execution");
    store
        .activate_job(&merge.execution, "j-merge", 1)
        .expect("current phase activates");
    drop(store);
    let store = reopen(&dir);
    assert_eq!(
        store
            .execution_by_job("j-merge")
            .expect("by job")
            .expect("some")
            .execution,
        merge.execution
    );
}

#[test]
fn legacy_work_is_adopted_without_touching_step_job_rows() {
    let (dir, store) = open();
    store.insert_run(&run("n1", 1)).expect("run");
    store
        .insert_step("n1", &step(1, "slice", "t1"))
        .expect("step");
    for row in [
        job("j1", 1, "slice", "t1", 1),
        job("j1-c1", 1, "slice", "t1", 1),
        job("j2", 1, "slice", "t1", 2),
    ] {
        store.insert_step_job("n1", &row).expect("job");
    }

    let adopted = store
        .register_execution(&hand("n1", 1, "t1"))
        .expect("register");
    assert_eq!(
        adopted.attempt,
        Some(2),
        "the historical attempts are adopted"
    );
    assert_eq!(adopted.active_job, None, "nothing is fabricated");
    assert_eq!(adopted.original_job, None);
    assert_eq!(
        store.last_attempt("t1").expect("attempts"),
        2,
        "step_jobs keep their identities"
    );
    for (name, attempt) in [("j1", 1), ("j1-c1", 1), ("j2", 2)] {
        assert_eq!(
            store.step_job(name).expect("job").expect(name).attempt,
            attempt
        );
    }
    drop(store);
    let store = reopen(&dir);
    assert_eq!(
        store
            .execution(&adopted.execution)
            .expect("read")
            .expect("some"),
        adopted
    );
}

#[test]
fn supersede_and_unknown_checkpoints_behave() {
    let (dir, store) = open();
    let row = store
        .register_execution(&hand("n1", 1, "t1"))
        .expect("register");
    let err = store
        .set_input_checkpoint("n1:r1:t1:hand:g9", "msg-1")
        .expect_err("unknown execution");
    assert!(err.to_string().contains("no such execution"), "{err}");

    store
        .set_input_checkpoint(&row.execution, "msg-1")
        .expect("checkpoint");
    store
        .supersede_execution(&row.execution)
        .expect("supersede");
    store
        .supersede_execution(&row.execution)
        .expect("idempotent");
    assert!(!store.execution_is_current(&row.execution).expect("current"));
    let err = store
        .activate_job(&row.execution, "j1", 1)
        .expect_err("superseded");
    assert!(err.to_string().contains("superseded"), "{err}");
    drop(store);
    let store = reopen(&dir);
    assert_eq!(
        store
            .execution(&row.execution)
            .expect("read")
            .expect("some")
            .input_checkpoint
            .as_deref(),
        Some("msg-1")
    );
}

#[test]
fn a_v8_database_keeps_its_version_and_gains_the_tables() {
    let (dir, store) = open();
    for table in ["step_executions", "task_phase"] {
        store
            .conn
            .execute(&format!("DROP TABLE {table}"), [])
            .expect("drop");
    }
    drop(store);
    let store = reopen(&dir);
    assert_eq!(store.schema_version().expect("version"), SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 8, "the additive tables bump no version");
    let names: Vec<String> = {
        let mut stmt = store
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .expect("prepare");
        stmt.query_map([], |row| row.get::<_, String>(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows")
    };
    for table in ["step_executions", "task_phase"] {
        assert!(names.iter().any(|name| name == table), "missing {table}");
    }
}
