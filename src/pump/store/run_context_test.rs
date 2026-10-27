//! Tests for the admission artifacts and run-continuation context in
//! [`crate::pump::store::run_context`].

use tempfile::tempdir;

use crate::pump::state::State;
use crate::pump::store::run_context::{
    AdmissionArtifact, AdmissionArtifactRow, AdmissionOutcome, ContinuationKind,
    ContinuationOutcome, RestartIntent, RestartIntentOutcome, RestartIntentRow, RunContinuation,
    RunContinuationRow,
};
use crate::pump::store::{DB_FILENAME, PumpStore};

fn open() -> (tempfile::TempDir, PumpStore) {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    (dir, store)
}

fn reopen(dir: &tempfile::TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("reopen")
}

fn artifact(need: &str, run: u32, plan: &str) -> AdmissionArtifact {
    AdmissionArtifact {
        need: need.into(),
        run,
        planner_task: format!("t-plan-{need}-{run}"),
        execution: format!("{need}:r{run}:plan:plan:g1"),
        generation: 1,
        admit_task: format!("t-admit-{need}-{run}"),
        admission: "admitted".into(),
        plan_body: plan.into(),
    }
}

fn recorded(result: AdmissionOutcome) -> AdmissionArtifactRow {
    match result {
        AdmissionOutcome::Recorded(row) => row,
        other => panic!("expected a recorded artifact, got {other:?}"),
    }
}

fn existing_artifact(result: AdmissionOutcome) -> AdmissionArtifactRow {
    match result {
        AdmissionOutcome::Existing(row) => row,
        other => panic!("expected the existing artifact, got {other:?}"),
    }
}

fn continuation(
    need: &str,
    run: u32,
    kind: ContinuationKind,
    source: Option<u32>,
) -> RunContinuation {
    RunContinuation {
        need: need.into(),
        run,
        kind,
        source_run: source,
        source_artifact: source.map(|from| format!("{need}:r{from}")),
    }
}

fn recorded_continuation(result: ContinuationOutcome) -> RunContinuationRow {
    match result {
        ContinuationOutcome::Recorded(row) => row,
        other => panic!("expected a recorded continuation, got {other:?}"),
    }
}

fn existing_continuation(result: ContinuationOutcome) -> RunContinuationRow {
    match result {
        ContinuationOutcome::Existing(row) => row,
        other => panic!("expected the existing continuation, got {other:?}"),
    }
}

fn recorded_intent(result: RestartIntentOutcome) -> RestartIntentRow {
    match result {
        RestartIntentOutcome::Recorded(row) => row,
        other => panic!("expected a recorded intent, got {other:?}"),
    }
}

#[test]
fn kind_round_trips_and_boundary_follows_the_entry() {
    for (word, boundary) in [
        ("research", true),
        ("plan", true),
        ("slice", false),
        ("rework", false),
    ] {
        let kind = ContinuationKind::parse(word).expect("parse");
        assert_eq!(kind.as_str(), word);
        assert_eq!(kind.establishes_boundary(), boundary);
    }
    assert!(ContinuationKind::parse("merge").is_err());
}

#[test]
fn a_newer_reply_never_replaces_a_stored_artifact() {
    let (_dir, store) = open();
    let first = artifact("n1", 1, "# plan one");
    let row = recorded(store.store_admission_artifact(&first).expect("store"));
    assert_eq!(row.identity(), "n1:r1");
    assert_eq!(row.plan_body, "# plan one");
    assert_eq!(row.generation, 1);
    assert_eq!(
        store.admission_artifact("n1", 1).expect("read"),
        Some(row.clone())
    );

    let replay = existing_artifact(store.store_admission_artifact(&first).expect("replay"));
    assert_eq!(replay, row);

    let conflicting = AdmissionArtifact {
        planner_task: "t-plan-revision-1".into(),
        execution: "n1:r1:plan:plan:g2".into(),
        generation: 2,
        admit_task: "t-admit-revision-1".into(),
        admission: "admitted revision 1".into(),
        plan_body: "# a completely different plan".into(),
        ..first.clone()
    };
    let kept = existing_artifact(
        store
            .store_admission_artifact(&conflicting)
            .expect("conflict"),
    );
    assert_eq!(kept, row);
    assert_eq!(kept.plan_body, "# plan one");
    assert_eq!(kept.planner_task, first.planner_task);
    assert_eq!(store.admission_artifact("n1", 1).expect("read"), Some(row));
    assert_eq!(store.admission_artifact("n1", 2).expect("read"), None);
}

#[test]
fn boundary_continuations_refuse_a_source_and_inherit_nothing() {
    let (_dir, store) = open();
    let plan = continuation("n1", 2, ContinuationKind::Plan, Some(1));
    assert!(store.record_run_continuation(&plan).is_err());

    let research = continuation("n1", 2, ContinuationKind::Research, None);
    let row = recorded_continuation(store.record_run_continuation(&research).expect("record"));
    assert!(row.boundary);
    assert_eq!(row.inherited_source(), None);
    assert_eq!(row.source_run, None);
    assert_eq!(row.source_artifact, None);
}

#[test]
fn a_repeated_continuation_keeps_the_original_context() {
    let (_dir, store) = open();
    let first = continuation("n1", 2, ContinuationKind::Slice, Some(1));
    let row = recorded_continuation(store.record_run_continuation(&first).expect("record"));
    assert_eq!(row.inherited_source(), Some((1, Some("n1:r1"))));

    let replay = existing_continuation(store.record_run_continuation(&first).expect("replay"));
    assert_eq!(replay, row);

    let drifted = RunContinuation {
        source_run: Some(3),
        source_artifact: Some("n1:r3".into()),
        kind: ContinuationKind::Rework,
        ..first.clone()
    };
    let kept = existing_continuation(store.record_run_continuation(&drifted).expect("replay"));
    assert_eq!(kept, row);
    assert_eq!(kept.kind, ContinuationKind::Slice);
    assert_eq!(kept.inherited_source(), Some((1, Some("n1:r1"))));
    assert_eq!(store.run_continuation("n1", 2).expect("read"), Some(row));
    assert_eq!(store.run_continuation("n1", 9).expect("read"), None);
}

#[test]
fn slice_chains_inherit_only_their_explicit_source() {
    let (_dir, store) = open();
    for (run, source) in [(2, Some(1)), (3, Some(2)), (4, None)] {
        let link = continuation("n1", run, ContinuationKind::Slice, source);
        let row = recorded_continuation(store.record_run_continuation(&link).expect("record"));
        assert_eq!(
            row.inherited_source().map(|(from, _)| from),
            source,
            "run {run} inherits exactly its explicit source"
        );
    }
    let rework = continuation("n1", 5, ContinuationKind::Rework, Some(4));
    let row = recorded_continuation(store.record_run_continuation(&rework).expect("record"));
    assert_eq!(row.inherited_source(), Some((4, Some("n1:r4"))));

    let chain: Vec<Option<u32>> = (2..=5)
        .map(|run| {
            store
                .run_continuation("n1", run)
                .expect("read")
                .map(|row| row.inherited_source().map(|(from, _)| from))
                .expect("continuation")
        })
        .collect();
    assert_eq!(chain, vec![Some(1), Some(2), None, Some(4)]);
}

#[test]
fn a_restart_intent_attaches_idempotently_to_the_eventual_run() {
    let (_dir, store) = open();
    let intent = RestartIntent {
        need: "n1".into(),
        kind: ContinuationKind::Rework,
        source_run: Some(1),
        source_artifact: Some("n1:r1".into()),
    };
    let row = recorded_intent(store.record_restart_intent(&intent).expect("record"));
    assert!(row.is_pending());
    assert_eq!(row.attached_run, None);
    assert!(!row.boundary);
    assert_eq!(
        store.restart_intent(row.intent_id).expect("read"),
        Some(row.clone())
    );
    assert_eq!(
        store.pending_restart_intents("n1").expect("pending"),
        vec![row.clone()]
    );

    let replay = match store.record_restart_intent(&intent).expect("replay") {
        RestartIntentOutcome::Existing(existing) => existing,
        other => panic!("expected the pending intent, got {other:?}"),
    };
    assert_eq!(replay, row);
    assert_eq!(
        store.pending_restart_intents("n1").expect("pending").len(),
        1
    );

    let attached = recorded_continuation(
        store
            .attach_restart_intent("n1", 2, row.intent_id)
            .expect("attach"),
    );
    assert_eq!(attached.kind, ContinuationKind::Rework);
    assert_eq!(attached.intent_id, Some(row.intent_id));
    assert_eq!(attached.inherited_source(), Some((1, Some("n1:r1"))));
    assert!(
        store
            .pending_restart_intents("n1")
            .expect("pending")
            .is_empty()
    );
    assert_eq!(
        store
            .restart_intent(row.intent_id)
            .expect("read")
            .map(|i| i.attached_run),
        Some(Some(2))
    );

    let repeated = existing_continuation(
        store
            .attach_restart_intent("n1", 2, row.intent_id)
            .expect("re-attach"),
    );
    assert_eq!(repeated, attached);
    assert_eq!(
        store.run_continuation("n1", 2).expect("read"),
        Some(attached)
    );

    let elsewhere = store.attach_restart_intent("n1", 3, row.intent_id);
    assert!(elsewhere.is_err(), "one intent seeds one lineage");
    assert_eq!(store.run_continuation("n1", 3).expect("read"), None);
    assert!(store.attach_restart_intent("n1", 3, 999).is_err());

    let fresh = RestartIntent {
        need: "n1".into(),
        kind: ContinuationKind::Rework,
        source_run: None,
        source_artifact: None,
    };
    let next = recorded_intent(store.record_restart_intent(&fresh).expect("record"));
    assert_ne!(next.intent_id, row.intent_id);
}

#[test]
fn records_survive_a_state_save_and_a_reopen() {
    let (dir, mut store) = open();
    let plan = artifact("n1", 1, "# plan one");
    recorded(store.store_admission_artifact(&plan).expect("store"));
    recorded_continuation(
        store
            .record_run_continuation(&continuation("n1", 2, ContinuationKind::Slice, Some(1)))
            .expect("record"),
    );
    recorded_continuation(
        store
            .record_run_continuation(&continuation("n1", 3, ContinuationKind::Plan, None))
            .expect("record"),
    );
    let intent = recorded_intent(
        store
            .record_restart_intent(&RestartIntent {
                need: "n2".into(),
                kind: ContinuationKind::Slice,
                source_run: Some(7),
                source_artifact: Some("n2:r7".into()),
            })
            .expect("record"),
    );

    store.save(&State::default()).expect("save");
    let expect = |store: &PumpStore| {
        (
            store.admission_artifact("n1", 1).expect("artifact"),
            store.run_continuation("n1", 2).expect("slice"),
            store.run_continuation("n1", 3).expect("plan"),
            store.restart_intent(intent.intent_id).expect("intent"),
            store.pending_restart_intents("n2").expect("pending"),
        )
    };
    let before = expect(&store);

    let reopened = reopen(&dir);
    assert_eq!(expect(&reopened), before);

    let again = reopen(&dir);
    assert_eq!(
        expect(&again),
        before,
        "an open that writes nothing changes nothing"
    );
}
