//! Round-trip tests for the recovery tables in [`super`].

use tempfile::tempdir;

use super::{DispatchAccept, DispatchStart, ExecutionOwner, snapshot_json};
use crate::pump::jobs::{JobSpec, PoolJob};
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};

fn open(dir: &tempfile::TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open")
}

fn owner(need: &str, task: &str, node: &str, phase: &str, seq: u64) -> ExecutionOwner {
    ExecutionOwner {
        need: need.into(),
        run: 1,
        task: task.into(),
        node: node.into(),
        phase: phase.into(),
        seq,
        job: None,
        inputs_json: "{}".into(),
        transient_json: "{}".into(),
        allowance: 0,
    }
}

#[test]
fn executions_round_trip_by_phase_and_keep_history() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let mut hand = owner("n1", "t1", "nd-a", "hand", 1);
    hand.job = Some("j-h1".into());
    hand.inputs_json = "{\"brief\":\"b\"}".into();
    hand.transient_json = "{\"tokens\":3}".into();
    store.register_execution(&hand).expect("register hand");
    let mut merge = owner("n1", "t1", "nd-a", "merge", 1);
    merge.job = Some("j-m1".into());
    store.register_execution(&merge).expect("register merge");

    assert_eq!(
        store
            .lookup_executions("n1", 1, "t1", "nd-a", "hand")
            .expect("hand"),
        vec![hand.clone()]
    );
    assert_eq!(
        store
            .lookup_executions("n1", 1, "t1", "nd-a", "merge")
            .expect("merge"),
        vec![merge.clone()]
    );
    assert_eq!(
        store
            .current_execution("n1", 1, "t1", "nd-a", "hand")
            .expect("current hand"),
        Some(hand.clone())
    );

    let second = owner("n1", "t1", "nd-a", "hand", 2);
    store.register_execution(&second).expect("register second");
    assert_eq!(
        store
            .lookup_executions("n1", 1, "t1", "nd-a", "hand")
            .expect("history"),
        vec![hand, second.clone()]
    );
    assert_eq!(
        store
            .current_execution("n1", 1, "t1", "nd-a", "hand")
            .expect("current"),
        Some(second)
    );
    assert_eq!(
        store
            .current_execution("n1", 1, "t1", "nd-a", "merge")
            .expect("merge current"),
        Some(merge)
    );
    assert!(
        store
            .current_execution("n2", 1, "t1", "nd-a", "hand")
            .expect("missing")
            .is_none()
    );
}

#[test]
fn registration_is_idempotent_without_resetting_allowance() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let mut first = owner("n1", "t1", "nd-a", "hand", 1);
    first.inputs_json = "{\"brief\":\"saved\"}".into();
    store.register_execution(&first).expect("register");

    store
        .set_execution_job(&first, Some("j-h1"))
        .expect("bind job");
    store.set_execution_allowance(&first, 1).expect("allowance");

    let mut again = owner("n1", "t1", "nd-a", "hand", 1);
    again.job = Some("j-other".into());
    again.allowance = 5;
    store.register_execution(&again).expect("re-register");

    let mut expected = owner("n1", "t1", "nd-a", "hand", 1);
    expected.inputs_json = "{\"brief\":\"saved\"}".into();
    expected.job = Some("j-h1".into());
    expected.allowance = 1;
    assert_eq!(
        store
            .current_execution("n1", 1, "t1", "nd-a", "hand")
            .expect("current"),
        Some(expected)
    );
}

#[test]
fn dispatch_start_is_kept_once_and_accepts_replace() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let start = DispatchStart {
        job: "j-h1".into(),
        at: 5,
        evidence_json: "{\"why\":\"step\"}".into(),
    };
    store.record_dispatch_start(&start).expect("start");
    let later = DispatchStart {
        job: "j-h1".into(),
        at: 9,
        evidence_json: "{\"why\":\"retry\"}".into(),
    };
    store.record_dispatch_start(&later).expect("second start");
    assert_eq!(
        store.dispatch_start("j-h1").expect("read start"),
        Some(start)
    );

    let accept = DispatchAccept {
        job: "j-h1".into(),
        task: Some("t1".into()),
        pool: Some("studio".into()),
        slot: Some("slot-1".into()),
        pool_job: Some("pool-77".into()),
        snapshot_json: None,
    };
    store.record_dispatch_accept(&accept).expect("accept");
    let mut second = accept;
    second.slot = Some("slot-2".into());
    second.pool_job = Some("pool-78".into());
    store
        .record_dispatch_accept(&second)
        .expect("second accept");
    assert_eq!(
        store.dispatch_accept("j-h1").expect("read accept"),
        Some(second.clone())
    );
    assert_ne!(second.pool_job.as_deref(), Some(second.job.as_str()));

    assert!(store.dispatch_start("nope").expect("none").is_none());
    assert!(store.dispatch_accept("nope").expect("none").is_none());
}

#[test]
fn pool_job_snapshot_round_trips_every_field() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let mut job = PoolJob::from(JobSpec::new("j-h1", "hand", "hand", "subject", "body"));
    job.budget = 300;
    job.read_only = true;
    job.payload = serde_json::json!({"k": "v"});
    let accept = DispatchAccept {
        job: job.id.clone(),
        task: None,
        pool: None,
        slot: None,
        pool_job: Some("pool-1".into()),
        snapshot_json: Some(snapshot_json(&job).expect("snapshot")),
    };
    store.record_dispatch_accept(&accept).expect("accept");

    let stored = store.dispatch_accept("j-h1").expect("read").expect("row");
    let text = stored.snapshot_json.as_deref().expect("snapshot");
    let round: PoolJob = serde_json::from_str(text).expect("decode");
    assert_eq!(round, job);
}

#[test]
fn recovery_rows_survive_state_default_save_and_reopen() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let mut hand = owner("n1", "t1", "nd-a", "hand", 1);
    hand.job = Some("j-h1".into());
    let merge = owner("n1", "t1", "nd-a", "merge", 1);
    store.register_execution(&hand).expect("register hand");
    store.register_execution(&merge).expect("register merge");
    store
        .record_dispatch_start(&DispatchStart {
            job: "j-h1".into(),
            at: 5,
            evidence_json: "{\"why\":\"step\"}".into(),
        })
        .expect("start");
    drop(store);

    let store = open(&dir);
    assert_eq!(
        store
            .lookup_executions("n1", 1, "t1", "nd-a", "hand")
            .expect("hand after reopen"),
        vec![hand.clone()]
    );
    drop(store);

    let mut store = open(&dir);
    store.save(&State::default()).expect("save default state");
    drop(store);

    let store = open(&dir);
    assert_eq!(
        store
            .lookup_executions("n1", 1, "t1", "nd-a", "hand")
            .expect("hand after save"),
        vec![hand]
    );
    assert_eq!(
        store
            .lookup_executions("n1", 1, "t1", "nd-a", "merge")
            .expect("merge after save"),
        vec![merge]
    );
    assert!(
        store
            .dispatch_start("j-h1")
            .expect("start after save")
            .is_some()
    );
}
