//! Tests for the flow run index tables in [`crate::pump::store::flow_index`].

use tempfile::tempdir;

use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState, run_state};
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore, SCHEMA_VERSION};

const TABLES: [&str; 7] = [
    "flow_runs",
    "flow_steps",
    "step_jobs",
    "slice_scope",
    "need_branches",
    "rung_runs",
    "main_state",
];

fn run(need: &str, n: u32, state: &str) -> RunRow {
    RunRow {
        need: need.into(),
        run: n,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "admit".into(),
        state: state.into(),
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

fn table_names(store: &PumpStore) -> Vec<String> {
    let mut stmt = store
        .conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .expect("prepare");
    stmt.query_map([], |row| row.get::<_, String>(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

#[test]
fn runs_steps_and_jobs_round_trip() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");

    store.insert_run(&run("n1", 1, "done")).expect("run 1");
    store
        .insert_run(&run("n1", 2, run_state::LIVE))
        .expect("run 2");
    store
        .insert_run(&run("n2", 1, run_state::LIVE))
        .expect("n2 run");
    assert_eq!(store.runs_of("n1").expect("runs").len(), 2);
    let live: Vec<_> = store
        .live_runs()
        .expect("live")
        .into_iter()
        .map(|r| (r.need, r.run))
        .collect();
    assert_eq!(live, vec![("n1".into(), 2), ("n2".into(), 1)]);
    store
        .set_run_state("n1", 2, run_state::SUPERSEDED)
        .expect("set");
    assert_eq!(store.live_runs().expect("live").len(), 1);

    store
        .insert_step("n1", &step(2, "admit", "t1"))
        .expect("s1");
    store
        .insert_step("n1", &step(2, "slice", "t2"))
        .expect("s2");
    store
        .insert_step("n2", &step(1, "admit", "t3"))
        .expect("s3");
    let nodes: Vec<_> = store
        .steps_of("n1", 2)
        .expect("steps")
        .into_iter()
        .map(|s| s.node)
        .collect();
    assert_eq!(nodes, ["admit", "slice"]);
    store
        .set_step_state("t1", StepState::Done, Some("approve"))
        .expect("state");
    let got = store.step_by_task("t1").expect("by task").expect("some");
    assert_eq!(got.state, StepState::Done);
    assert_eq!(got.verdict.as_deref(), Some("approve"));
    assert!(store.step_by_task("nope").expect("by task").is_none());

    for (job, attempt) in [("j1", 1), ("j2", 2)] {
        let row = StepJobRow {
            job: job.into(),
            run: 2,
            node: "admit".into(),
            task: "t1".into(),
            attempt,
        };
        store.insert_step_job("n1", &row).expect("job");
    }
    assert_eq!(store.last_attempt("t1").expect("attempts"), 2);
    assert_eq!(store.next_attempt("t1").expect("attempts"), 3);
    assert_eq!(store.last_attempt("t2").expect("attempts"), 0);
    assert_eq!(store.next_attempt("t2").expect("attempts"), 1);
    assert_eq!(store.step_job("j2").expect("job").expect("some").attempt, 2);
    assert!(store.step_job("j9").expect("job").is_none());
}

#[test]
fn a_v8_database_keeps_its_version_and_gains_the_tables() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join(DB_FILENAME);
    {
        let store = PumpStore::open(&path).expect("open");
        for table in TABLES {
            store
                .conn
                .execute(&format!("DROP TABLE {table}"), [])
                .expect("drop");
        }
    }
    let store = PumpStore::open(&path).expect("reopen");
    assert_eq!(store.schema_version().expect("version"), 8);
    assert_eq!(SCHEMA_VERSION, 8);
    let names = table_names(&store);
    for table in TABLES {
        assert!(names.iter().any(|n| n == table), "missing {table}");
    }
}

#[test]
fn save_leaves_flow_rows_alone() {
    let dir = tempdir().expect("tempdir");
    let mut store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    store
        .insert_run(&run("n1", 1, run_state::LIVE))
        .expect("run");
    store
        .insert_step("n1", &step(1, "admit", "t1"))
        .expect("step");
    store.save(&State::default()).expect("save");
    store.save(&State::default()).expect("save again");
    assert_eq!(store.runs_of("n1").expect("runs").len(), 1);
    assert_eq!(store.steps_of("n1", 1).expect("steps").len(), 1);
}

#[test]
fn a_finishing_run_is_listed_by_finishing_runs_and_not_by_live_runs() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    store
        .insert_run(&run("n1", 1, run_state::LIVE))
        .expect("run 1");
    store
        .insert_run(&run("n2", 1, run_state::LIVE))
        .expect("run 2");
    assert!(store.finishing_runs().expect("finishing").is_empty());

    store
        .set_run_state("n1", 1, "finishing")
        .expect("set finishing");
    let finishing: Vec<_> = store
        .finishing_runs()
        .expect("finishing")
        .into_iter()
        .map(|r| (r.need, r.run))
        .collect();
    assert_eq!(finishing, vec![("n1".to_string(), 1)]);
    let live: Vec<_> = store
        .live_runs()
        .expect("live")
        .into_iter()
        .map(|r| r.need)
        .collect();
    assert_eq!(live, ["n2"]);
    assert_eq!(store.runs_in_state("finishing").expect("state").len(), 1);

    store.set_run_state("n1", 1, "done").expect("set done");
    assert!(store.finishing_runs().expect("finishing").is_empty());
}

#[test]
fn state_at_updates_on_each_state_change() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    store
        .insert_run(&run("n1", 1, run_state::LIVE))
        .expect("run");
    assert!(store.run_state_at("n1", 9).expect("at").is_none());
    assert!(store.run_state_at("n1", 1).expect("at").expect("some") > 0);

    for state in ["finishing", "done"] {
        store
            .conn
            .execute("UPDATE flow_runs SET state_at = 1", [])
            .expect("age the row");
        store.set_run_state("n1", 1, state).expect("set");
        assert!(
            store.run_state_at("n1", 1).expect("at").expect("some") > 1,
            "state {state} stamps the time"
        );
    }
}

#[test]
fn insert_run_step_checks_the_run_and_run_row_names_a_missing_run() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    store
        .insert_run(&run("n1", 2, run_state::LIVE))
        .expect("run");
    let row = store.run_row("n1", 2).expect("run row");
    assert_eq!((row.need.as_str(), row.run), ("n1", 2));

    let err = store
        .insert_run_step(&row, &step(1, "admit", "t1"))
        .expect_err("mismatched run");
    assert_eq!(
        err.to_string(),
        "step t1: expected run r2; actual: row run r1"
    );
    assert!(store.steps_of("n1", 1).expect("steps").is_empty());

    store
        .insert_run_step(&row, &step(2, "admit", "t2"))
        .expect("matching run");
    assert_eq!(store.steps_of("n1", 2).expect("steps").len(), 1);

    let err = store.run_row("n1", 9).expect_err("missing run");
    assert_eq!(
        err.to_string(),
        "need n1: expected run r9; actual: no such run"
    );
}

#[test]
fn a_correction_does_not_inflate_the_next_attempt() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    store
        .insert_run(&run("n1", 1, run_state::LIVE))
        .expect("run");
    assert_eq!(store.last_attempt("t1").expect("last"), 0);
    assert_eq!(store.next_attempt("t1").expect("next"), 1);
    for (job, attempt) in [("j1", 1), ("j1-c1", 1), ("j2", 2)] {
        let row = StepJobRow {
            job: job.into(),
            run: 1,
            node: "admit".into(),
            task: "t1".into(),
            attempt,
        };
        store.insert_step_job("n1", &row).expect("job");
    }
    assert_eq!(store.last_attempt("t1").expect("last"), 2);
    assert_eq!(store.next_attempt("t1").expect("next"), 3);
}

#[test]
fn run_states_outside_the_vocabulary_are_refused() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    let err = store
        .insert_run(&run("n1", 1, "lock"))
        .expect_err("insert unknown state");
    assert!(err.to_string().contains("actual: lock"), "{err}");

    store
        .insert_run(&run("n1", 1, run_state::LIVE))
        .expect("run");
    let err = store
        .set_run_state("n1", 1, "lock")
        .expect_err("set unknown state");
    assert!(err.to_string().contains("actual: lock"), "{err}");

    store
        .conn
        .execute("UPDATE flow_runs SET state = 'lock' WHERE need = 'n1'", [])
        .expect("corrupt");
    let err = store.runs_of("n1").expect_err("read unknown state");
    assert!(err.to_string().contains("actual: lock"), "{err}");
}
