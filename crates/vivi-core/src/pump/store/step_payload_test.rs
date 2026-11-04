//! Tests for the `step_payloads` table methods in [`super`].

use serde_json::json;
use tempfile::{TempDir, tempdir};

use super::StepPayloadRow;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::store::{DB_FILENAME, PumpStore};

fn row(need: &str, run: u32, task: &str) -> StepPayloadRow {
    StepPayloadRow {
        need: need.into(),
        run,
        node: "audit1".into(),
        class: "auditor".into(),
        task: task.into(),
        payload: json!({"raise_rung": "unit", "n": 3}),
    }
}

fn open(dir: &TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open")
}

#[test]
fn recorded_row_reads_back_equal() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store
        .record_step_payload(&row("n1", 1, "t1"))
        .expect("record");
    assert_eq!(
        store.step_payload("t1").expect("read"),
        Some(row("n1", 1, "t1"))
    );
    assert_eq!(store.step_payload("nope").expect("read"), None);
}

#[test]
fn second_record_replaces_payload_and_keeps_position() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.record_step_payload(&row("n1", 1, "t1")).expect("one");
    store.record_step_payload(&row("n1", 1, "t2")).expect("two");
    let mut again = row("n1", 1, "t1");
    again.payload = json!({"replaced": true});
    store.record_step_payload(&again).expect("again");
    let rows = store.step_payloads_of_run("n1", 1).expect("rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], again);
    assert_eq!(rows[1].task, "t2");
}

#[test]
fn run_and_need_reads_filter_and_order() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.record_step_payload(&row("n1", 2, "t3")).expect("t3");
    store.record_step_payload(&row("n1", 1, "t1")).expect("t1");
    store.record_step_payload(&row("n2", 1, "t4")).expect("t4");
    store.record_step_payload(&row("n1", 1, "t2")).expect("t2");
    let run1: Vec<String> = store
        .step_payloads_of_run("n1", 1)
        .expect("run")
        .into_iter()
        .map(|r| r.task)
        .collect();
    assert_eq!(run1, ["t1", "t2"]);
    let need: Vec<String> = store
        .step_payloads_of_need("n1")
        .expect("need")
        .into_iter()
        .map(|r| r.task)
        .collect();
    assert_eq!(need, ["t1", "t2", "t3"]);
}

#[test]
fn step_run_of_reads_the_flow_step_table() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store
        .insert_step(
            "n1",
            &StepRow {
                run: 3,
                node: "plan".into(),
                task: "t9".into(),
                state: StepState::Running,
                verdict: None,
            },
        )
        .expect("insert step");
    assert_eq!(
        store.step_run_of("t9").expect("find"),
        Some(("n1".to_string(), 3))
    );
    assert_eq!(store.step_run_of("unknown").expect("find"), None);
}
