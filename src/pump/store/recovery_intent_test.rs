//! Round-trip tests for the transactional replacement intents in [`super`].

use tempfile::tempdir;

use super::super::{ExecutionOwner, ReplacementIntent, ReplacementRequest};
use crate::pump::flow::index::{FLOW_VERSION_NUMBER, RunRow, StepJobRow, StepState, run_state};
use crate::pump::store::{DB_FILENAME, PumpStore};

fn open(dir: &tempfile::TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open")
}

fn owner(task: &str, seq: u64) -> ExecutionOwner {
    ExecutionOwner {
        need: "n1".into(),
        run: 1,
        task: task.into(),
        node: "nd-a".into(),
        phase: "hand".into(),
        seq,
        job: Some(format!("j-{seq}")),
        inputs_json: "{}".into(),
        transient_json: "{}".into(),
        allowance: 0,
    }
}

fn request(
    store: &PumpStore,
    collector: &str,
    trigger: &str,
    job: Option<StepJobRow>,
) -> ReplacementRequest {
    let expected = store
        .current_execution("n1", 1, "t1", "nd-a", "hand")
        .expect("current")
        .expect("owner");
    ReplacementRequest {
        expected,
        collector: collector.into(),
        trigger: trigger.into(),
        kind: "recollect".into(),
        deadline: 90,
        inputs_json: "{\"brief\":\"fresh\"}".into(),
        tasks: vec!["t1".into(), "t2".into()],
        source_settled: true,
        job,
        at: 10,
    }
}

fn job_row(job: &str, attempt: u32) -> StepJobRow {
    StepJobRow {
        job: job.into(),
        run: 1,
        node: "nd-a".into(),
        task: "t1".into(),
        attempt,
    }
}

fn seed_flow(store: &PumpStore) {
    let run = RunRow {
        need: "n1".into(),
        run: 1,
        flow: "f".into(),
        flow_version: FLOW_VERSION_NUMBER,
        kind: "k".into(),
        entry: "nd-a".into(),
        state: run_state::LIVE.into(),
    };
    store.insert_run(&run).expect("run");
    store
        .insert_run_step(&run, &run.step("nd-a", "t1", StepState::Waiting))
        .expect("step");
}

fn current_seq(store: &PumpStore) -> u64 {
    store
        .current_execution("n1", 1, "t1", "nd-a", "hand")
        .expect("read current")
        .expect("owner")
        .seq
}

#[test]
fn replacement_schedules_atomically_and_updates_flow_rows() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.register_execution(&owner("t1", 1)).expect("register");
    store
        .set_execution_allowance(&owner("t1", 1), 2)
        .expect("allowance");
    seed_flow(&store);

    let intent = store
        .schedule_replacement(&request(
            &store,
            "recollect",
            "letter-1",
            Some(job_row("j-h2", 2)),
        ))
        .expect("schedule");
    assert!(intent.intent.starts_with("ri-"));
    assert_eq!(intent.collector, "recollect");
    assert_eq!(intent.trigger, "letter-1");
    assert_eq!(intent.seq, 1);
    assert_eq!(intent.kind, "recollect");
    assert_eq!(intent.deadline, 90);
    assert_eq!(intent.tasks, vec!["t1".to_string(), "t2".to_string()]);
    assert!(intent.source_settled);
    assert!(!intent.cancelled);
    assert_eq!(intent.replay_seq, 0);
    assert_eq!(intent.job.as_deref(), Some("j-h2"));

    let next = store
        .current_execution("n1", 1, "t1", "nd-a", "hand")
        .expect("current")
        .expect("owner");
    assert_eq!(next.seq, 2);
    assert_eq!(next.inputs_json, "{\"brief\":\"fresh\"}");
    assert_eq!(next.job.as_deref(), Some("j-h2"));
    assert_eq!(next.allowance, 1);

    let step = store.step_by_task("t1").expect("step").expect("row");
    assert_eq!(step.state, StepState::Running);
    assert_eq!(
        store.step_job("j-h2").expect("job"),
        Some(job_row("j-h2", 2))
    );

    let history = store
        .lookup_executions("n1", 1, "t1", "nd-a", "hand")
        .expect("history");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].allowance, 1);
    assert_eq!(history[1].allowance, 1);
}

#[test]
fn repeated_trigger_returns_original_intent_after_ownership_advanced() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.register_execution(&owner("t1", 1)).expect("register");
    store
        .set_execution_allowance(&owner("t1", 1), 2)
        .expect("allowance");
    let intent = store
        .schedule_replacement(&request(&store, "recollect", "letter-1", None))
        .expect("schedule");

    let mut again = request(&store, "recollect", "letter-1", None);
    again.expected = owner("t1", 1);
    let replay = store.schedule_replacement(&again).expect("repeated");
    assert_eq!(replay, intent);

    assert_eq!(current_seq(&store), 2);
    assert_eq!(
        store.replacement_intents("n1", 1).expect("intents").len(),
        1
    );
    let current = store
        .current_execution("n1", 1, "t1", "nd-a", "hand")
        .expect("current")
        .expect("owner");
    assert_eq!(current.allowance, 1);
}

#[test]
fn stale_trigger_cannot_replace_current_work() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.register_execution(&owner("t1", 1)).expect("register");
    store
        .set_execution_allowance(&owner("t1", 1), 2)
        .expect("allowance");
    store
        .schedule_replacement(&request(&store, "recollect", "letter-1", None))
        .expect("schedule");

    let mut stale = request(&store, "recollect", "letter-2", None);
    stale.expected = owner("t1", 1);
    assert!(store.schedule_replacement(&stale).is_err());

    assert_eq!(current_seq(&store), 2);
    let intents = store.replacement_intents("n1", 1).expect("intents");
    assert_eq!(intents.len(), 1);
    assert_eq!(intents[0].trigger, "letter-1");

    let mut unknown = request(&store, "recollect", "letter-3", None);
    unknown.expected.task = "t-missing".into();
    assert!(store.schedule_replacement(&unknown).is_err());
}

#[test]
fn failed_schedule_rolls_back_every_write() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.register_execution(&owner("t1", 1)).expect("register");
    store
        .set_execution_allowance(&owner("t1", 1), 2)
        .expect("allowance");
    seed_flow(&store);
    store
        .insert_step_job("n1", &job_row("j-taken", 9))
        .expect("seed job");

    assert!(
        store
            .schedule_replacement(&request(
                &store,
                "recollect",
                "letter-9",
                Some(job_row("j-taken", 2))
            ))
            .is_err()
    );

    assert!(
        store
            .replacement_intents("n1", 1)
            .expect("intents")
            .is_empty()
    );
    assert_eq!(current_seq(&store), 1);
    let history = store
        .lookup_executions("n1", 1, "t1", "nd-a", "hand")
        .expect("history");
    assert_eq!(history[0].allowance, 2);
    let step = store.step_by_task("t1").expect("step").expect("row");
    assert_eq!(step.state, StepState::Waiting);
}

#[test]
fn recollection_debits_one_allowance_and_never_infers_it() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.register_execution(&owner("t1", 1)).expect("register");
    store
        .set_execution_allowance(&owner("t1", 1), 2)
        .expect("allowance");

    store
        .schedule_replacement(&request(&store, "recollect", "a", None))
        .expect("recollect a");
    assert_eq!(allowances(&store), vec![1, 1]);
    store
        .schedule_replacement(&request(&store, "recollect", "b", None))
        .expect("recollect b");
    assert_eq!(allowances(&store), vec![1, 0, 0]);
    // With the balance spent, a further recollection still schedules and the
    // allowance stays at zero: it is never negative and never derived from
    // the sequence or attempt numbers.
    store
        .schedule_replacement(&request(&store, "recollect", "c", None))
        .expect("recollect c");

    assert_eq!(allowances(&store), vec![1, 0, 0, 0]);
    assert_eq!(current_seq(&store), 4);
}

fn allowances(store: &PumpStore) -> Vec<u64> {
    store
        .lookup_executions("n1", 1, "t1", "nd-a", "hand")
        .expect("history")
        .iter()
        .map(|row| row.allowance)
        .collect()
}

#[test]
fn independent_collectors_schedule_without_flow_jobs() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.register_execution(&owner("t1", 1)).expect("register");

    let first = store
        .schedule_replacement(&request(&store, "boundary", "letter-1", None))
        .expect("boundary schedule");
    assert!(first.job.is_none());
    assert_eq!(current_seq(&store), 2);
    assert!(store.step_job("j-2").expect("no flow job").is_none());

    let second = store
        .schedule_replacement(&request(&store, "thread", "letter-1", None))
        .expect("thread schedule");
    assert_ne!(second.intent, first.intent);
    assert_ne!(second.seq, first.seq);
    assert_eq!(current_seq(&store), 3);
}

#[test]
fn intents_enumerate_cancel_and_survive_reopen() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store
        .register_execution(&owner("t1", 1))
        .expect("register t1");
    let intent = store
        .schedule_replacement(&request(&store, "recollect", "letter-1", None))
        .expect("schedule t1");

    let mut other = request(&store, "recollect", "letter-2", None);
    other.expected.task = "t2".into();
    other.expected.node = "nd-b".into();
    other.expected.phase = "merge".into();
    other.expected.seq = 1;
    other.source_settled = false;
    store
        .register_execution(&other.expected.clone())
        .expect("register t2");
    let other_intent = store.schedule_replacement(&other).expect("schedule t2");

    drop(store);
    let store = open(&dir);
    let intents = store.replacement_intents("n1", 1).expect("intents");
    assert_eq!(intents, vec![intent.clone(), other_intent.clone()]);
    assert!(intents[0].source_settled);
    assert!(!intents[1].source_settled);

    assert_eq!(
        store
            .cancel_owner_intents("n1", 1, "t1", "nd-a", "hand")
            .expect("cancel owner"),
        1
    );
    let intents = store.replacement_intents("n1", 1).expect("intents");
    assert!(intents[0].cancelled);
    assert!(!intents[1].cancelled);

    store.set_intent_replay(&intent.intent, 3).expect("replay");
    let intents = store.replacement_intents("n1", 1).expect("intents");
    assert_eq!(intents[0].replay_seq, 3);

    assert_eq!(store.cancel_run_intents("n1", 1).expect("cancel run"), 1);
    assert_eq!(store.cancel_run_intents("n1", 1).expect("cancel again"), 0);
    assert_eq!(
        store
            .replacement_intents("n1", 1)
            .expect("intents")
            .iter()
            .filter(|row| row.cancelled)
            .count(),
        2
    );
}

#[test]
fn intent_ids_are_deterministic_across_reopen() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.register_execution(&owner("t1", 1)).expect("register");
    let intent = store
        .schedule_replacement(&request(&store, "recollect", "letter-1", None))
        .expect("schedule");
    drop(store);

    let store = open(&dir);
    let mut retry = request(&store, "recollect", "letter-1", None);
    retry.expected = owner("t1", 1);
    let replay = store.schedule_replacement(&retry).expect("repeated");
    assert_eq!(replay.intent, intent.intent);
    let intents: Vec<ReplacementIntent> = store.replacement_intents("n1", 1).expect("intents");
    assert_eq!(intents.len(), 1);
}
