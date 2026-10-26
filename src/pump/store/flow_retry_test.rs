//! Tests for the flow-retry decision methods in [`super`].

use tempfile::{TempDir, tempdir};

use super::{FlowRetryDecision, FlowRetryRequest, RETRY_LIMIT};
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};

/// A request for `failed_job` of one fixed step and input generation.
fn req(failed_job: &str) -> FlowRetryRequest {
    FlowRetryRequest {
        need: "n1".into(),
        run: 1,
        node: "slice:1".into(),
        task: "t1".into(),
        generation: 1,
        failed_job: failed_job.into(),
        failure_class: "agent-failed".into(),
        replacement_job: None,
        deadline: 1_000,
    }
}

/// The same failed job, re-asked with a later clock and a changed story.
fn repeat(original: &FlowRetryRequest) -> (FlowRetryRequest, u64) {
    let mut again = original.clone();
    again.failure_class = "budget".into();
    again.replacement_job = Some("repl-x".into());
    again.deadline = 9_000;
    (again, 9_999)
}

fn open(dir: &TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open")
}

/// Decide `count` fresh failed jobs of `req`'s step and generation.
fn decide_many(store: &PumpStore, req: &FlowRetryRequest, count: u32, now: u64) {
    for nth in 1..=count {
        let mut one = req.clone();
        one.failed_job = format!("{}-f{nth}", req.failed_job);
        store.decide_flow_retry(&one, now).expect("decide");
    }
}

#[test]
fn a_decided_job_repeats_its_original_decision() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let first = req("j1");
    let decided = store.decide_flow_retry(&first, 100).expect("decide");
    assert!(decided.allowed);
    assert_eq!(decided.ordinal, 1);
    assert_eq!(decided.decided_at, 100);
    assert_eq!(decided.deadline, 1_000);

    let (again, later) = repeat(&first);
    let repeated = store.decide_flow_retry(&again, later).expect("repeat");
    assert_eq!(repeated, decided);
    assert_eq!(repeated.decided_at, 100);
    let history = store
        .flow_retry_failures("n1", 1, "slice:1", "t1", 1)
        .expect("history");
    assert_eq!(history.len(), 1);
}

#[test]
fn mixed_causes_share_one_allowance_then_exhaustion_is_recorded() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    for (nth, class) in ["budget", "env", "agent-failed"].iter().enumerate() {
        let mut one = req(&format!("j{nth}"));
        one.failure_class = (*class).into();
        let decided = store.decide_flow_retry(&one, 100).expect("decide");
        assert!(decided.allowed, "class {class} must still be allowed");
        assert_eq!(decided.ordinal, nth as u32 + 1);
    }
    let mut fourth = req("j4");
    fourth.failure_class = "budget".into();
    let exhausted = store.decide_flow_retry(&fourth, 100).expect("decide");
    assert!(!exhausted.allowed);
    assert_eq!(exhausted.ordinal, RETRY_LIMIT + 1);

    let (again, later) = repeat(&fourth);
    assert_eq!(
        store.decide_flow_retry(&again, later).expect("repeat"),
        exhausted
    );
    let stored = store.flow_retry_decision("j4").expect("read").expect("row");
    assert_eq!(stored, exhausted);
}

#[test]
fn steps_and_generations_budget_independently() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let base = req("base");
    decide_many(&store, &base, RETRY_LIMIT, 100);
    assert!(
        !store
            .decide_flow_retry(&req("over-base"), 100)
            .expect("decide")
            .allowed
    );

    let mut other_task = base.clone();
    other_task.task = "t2".into();
    other_task.failed_job = "t2-first".into();
    let decided = store.decide_flow_retry(&other_task, 100).expect("decide");
    assert!(decided.allowed);
    assert_eq!(decided.ordinal, 1);

    let mut other_node = base.clone();
    other_node.node = "slice:2".into();
    other_node.failed_job = "node2-first".into();
    assert!(
        store
            .decide_flow_retry(&other_node, 100)
            .expect("decide")
            .allowed
    );

    let mut other_run = base.clone();
    other_run.run = 2;
    other_run.failed_job = "run2-first".into();
    assert!(
        store
            .decide_flow_retry(&other_run, 100)
            .expect("decide")
            .allowed
    );

    let mut other_need = base.clone();
    other_need.need = "n2".into();
    other_need.failed_job = "need2-first".into();
    assert!(
        store
            .decide_flow_retry(&other_need, 100)
            .expect("decide")
            .allowed
    );

    let mut other_generation = base;
    other_generation.generation = 2;
    other_generation.failed_job = "gen2-first".into();
    let decided = store
        .decide_flow_retry(&other_generation, 100)
        .expect("decide");
    assert!(decided.allowed);
    assert_eq!(decided.ordinal, 1);
}

#[test]
fn changing_only_the_cause_does_not_reset_a_budget() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let base = req("base");
    decide_many(&store, &base, RETRY_LIMIT, 100);
    let mut shifted = base;
    shifted.failure_class = "env".into();
    shifted.failed_job = "cause-shift".into();
    assert!(
        !store
            .decide_flow_retry(&shifted, 100)
            .expect("decide")
            .allowed
    );
}

#[test]
fn decisions_survive_reopening_and_a_state_save() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join(DB_FILENAME);
    let base = req("base");
    let mut with_replacement = base.clone();
    with_replacement.failed_job = "base-pre".into();
    with_replacement.replacement_job = Some("repl-1".into());
    {
        let mut store = PumpStore::open(&path).expect("open");
        store
            .decide_flow_retry(&with_replacement, 100)
            .expect("one");
        decide_many(&store, &base, RETRY_LIMIT - 1, 100);
        store.save(&State::default()).expect("save");
    }
    let mut store = PumpStore::open(&path).expect("reopen");
    let history = store
        .flow_retry_failures("n1", 1, "slice:1", "t1", 1)
        .expect("history");
    assert_eq!(history.len(), usize::try_from(RETRY_LIMIT).expect("fits"));
    assert_eq!(history[0].failed_job, "base-pre");
    assert_eq!(history[0].replacement_job.as_deref(), Some("repl-1"));
    assert_eq!(history[0].replacement_order, Some(1));
    assert!(history.iter().all(|row| row.allowed));
    let mut too_late = base;
    too_late.failed_job = "too-late".into();
    assert!(
        !store
            .decide_flow_retry(&too_late, 200)
            .expect("decide")
            .allowed
    );

    let state = store.load().expect("load");
    store.save(&state).expect("save");
    assert_eq!(
        store
            .flow_retry_failures("n1", 1, "slice:1", "t1", 1)
            .expect("history")
            .len(),
        history.len() + 1
    );
}

#[test]
fn a_partial_decision_can_be_read_and_finished() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let first = req("j1");
    let decided = store.decide_flow_retry(&first, 100).expect("decide");
    assert_eq!(
        store
            .unfinished_flow_retries("n1", 1, "slice:1", "t1", 1)
            .expect("unfinished"),
        vec![decided.clone()]
    );
    assert_eq!(store.flow_retry_decision("nope").expect("read"), None);
    assert_eq!(
        store
            .set_flow_retry_replacement("nope", "repl-x")
            .expect("attach"),
        None
    );
    let finished = store
        .set_flow_retry_replacement("j1", "repl-1")
        .expect("attach")
        .expect("decision");
    assert_eq!(finished.replacement_job.as_deref(), Some("repl-1"));
    assert_eq!(finished.replacement_order, Some(decided.ordinal));
    assert_eq!(
        store.flow_retry_decision("j1").expect("read"),
        Some(finished.clone())
    );
    assert!(
        store
            .unfinished_flow_retries("n1", 1, "slice:1", "t1", 1)
            .expect("unfinished")
            .is_empty()
    );

    let mut second = req("j2");
    second.replacement_job = Some("repl-2".into());
    let allocated = store.decide_flow_retry(&second, 100).expect("decide");
    assert_eq!(allocated.replacement_job.as_deref(), Some("repl-2"));
    assert_eq!(allocated.replacement_order, Some(2));
    assert!(
        store
            .unfinished_flow_retries("n1", 1, "slice:1", "t1", 1)
            .expect("unfinished")
            .is_empty()
    );
}

#[test]
fn the_history_orders_every_failed_job_by_decision() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let classes = ["budget", "env", "agent-failed", "budget"];
    for (nth, class) in classes.iter().enumerate() {
        let mut one = req(&format!("j{nth}"));
        one.failure_class = (*class).into();
        store
            .decide_flow_retry(&one, u64::try_from(nth).unwrap_or(0) + 100)
            .expect("decide");
    }
    let history = store
        .flow_retry_failures("n1", 1, "slice:1", "t1", 1)
        .expect("history");
    let got: Vec<(&str, bool)> = history
        .iter()
        .map(|row: &FlowRetryDecision| (row.failed_job.as_str(), row.allowed))
        .collect();
    assert_eq!(
        got,
        [("j0", true), ("j1", true), ("j2", true), ("j3", false)]
    );
    assert_eq!(
        store
            .flow_retry_failures("n1", 1, "slice:1", "t9", 1)
            .expect("empty"),
        []
    );
}
