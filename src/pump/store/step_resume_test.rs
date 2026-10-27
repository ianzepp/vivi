//! Tests for the typed suspensions and reply-resume intents in
//! [`crate::pump::store::step_resume`].

use tempfile::tempdir;

use crate::pump::jobs::JobSpec;
use crate::pump::state::State;
use crate::pump::store::step_execution::ExecutionKey;
use crate::pump::store::step_resume::{
    ResumeCreation, ResumeIntent, ResumeIntentRow, ResumeMessage, ResumeMessageRow, Suspension,
    SuspensionReason, SuspensionRow,
};
use crate::pump::store::{DB_FILENAME, PumpStore};

const HAND: &str = "hand";

fn open() -> (tempfile::TempDir, PumpStore) {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    (dir, store)
}

fn reopen(dir: &tempfile::TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("reopen")
}

fn hand(need: &str, run: u32, task: &str) -> ExecutionKey {
    ExecutionKey::new(need, run, task, HAND)
}

fn execution_id(key: &ExecutionKey, generation: u32) -> String {
    format!(
        "{}:r{}:{}:{}:g{}",
        key.need, key.run, key.task, key.phase, generation
    )
}

fn suspension(
    key: &ExecutionKey,
    generation: u32,
    reason: SuspensionReason,
    outcome: &str,
    detail: &str,
) -> Suspension {
    Suspension {
        need: key.need.clone(),
        run: key.run,
        task: key.task.clone(),
        phase: key.phase.clone(),
        execution: execution_id(key, generation),
        generation,
        reason,
        outcome: outcome.into(),
        reason_detail: detail.into(),
        park_task: None,
        checkpoint: Some("msg-seed".into()),
    }
}

fn message(id: &str, sender: &str, sent_at: u64, content: &str) -> ResumeMessage {
    ResumeMessage {
        message_id: id.into(),
        sender: sender.into(),
        sent_at,
        content: content.into(),
    }
}

fn intent(
    key: &ExecutionKey,
    suspension: u32,
    replacement: &str,
    from: u32,
    to: u32,
    messages: &[&str],
) -> ResumeIntent {
    ResumeIntent {
        key: key.clone(),
        suspension,
        execution: execution_id(key, from),
        from_generation: from,
        to_generation: to,
        replacement_job: replacement.into(),
        spec: JobSpec::new(replacement, "slice", HAND, "subject", "body"),
        messages: messages.iter().map(|id| (*id).to_string()).collect(),
    }
}

fn fresh(result: ResumeCreation) -> ResumeIntentRow {
    match result {
        ResumeCreation::Fresh(row) => row,
        other => panic!("expected a fresh intent, got {other:?}"),
    }
}

fn existing(result: ResumeCreation) -> ResumeIntentRow {
    match result {
        ResumeCreation::Existing(row) => row,
        other => panic!("expected an existing intent, got {other:?}"),
    }
}

fn ids(rows: &[ResumeMessageRow]) -> Vec<&str> {
    rows.iter().map(|row| row.message_id.as_str()).collect()
}

#[test]
fn suspension_reasons_round_trip_through_their_labels() {
    let all = [
        (SuspensionReason::RoleInputs, "role_inputs"),
        (SuspensionReason::Lock, "lock"),
        (SuspensionReason::Milestone, "milestone"),
        (SuspensionReason::Wait, "wait"),
        (SuspensionReason::Mechanical, "mechanical"),
        (SuspensionReason::Environment, "environment"),
        (SuspensionReason::Uncertain, "uncertain"),
    ];
    for (reason, label) in all {
        assert_eq!(reason.as_str(), label);
        assert_eq!(SuspensionReason::parse(label).expect("parse"), reason);
    }
    assert!(SuspensionReason::parse("something-else").is_err());
}

#[test]
fn suspensions_round_trip_and_reparks_append_history() {
    let (_dir, store) = open();
    let k = hand("n1", 1, "t1");
    assert!(store.suspensions("n1", 1, "t1").expect("empty").is_empty());
    assert!(
        store
            .latest_suspension("n1", 1, "t1")
            .expect("none")
            .is_none()
    );

    let mut first = suspension(
        &k,
        1,
        SuspensionReason::RoleInputs,
        "waiting",
        "role inputs pending",
    );
    first.park_task = Some("park-task-1".into());
    first.checkpoint = Some("msg-42".into());
    let row1 = store.record_suspension(&first).expect("record 1");
    assert_eq!(row1.suspension, 1);
    assert_eq!(row1.execution, execution_id(&k, 1));
    assert_eq!(row1.reason, SuspensionReason::RoleInputs);
    assert_eq!(row1.park_task.as_deref(), Some("park-task-1"));
    assert_eq!(row1.checkpoint.as_deref(), Some("msg-42"));

    let repark = suspension(&k, 2, SuspensionReason::Lock, "blocked", "pool lock held");
    let row2 = store.record_suspension(&repark).expect("record 2");
    assert_eq!(row2.suspension, 2);
    assert_eq!(row2.reason, SuspensionReason::Lock);

    let history = store.suspensions("n1", 1, "t1").expect("history");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].reason, SuspensionReason::RoleInputs);
    assert_eq!(history[1].suspension, 2);
    assert_eq!(history[1].generation, 2);
    let latest = store.latest_suspension("n1", 1, "t1").expect("latest");
    assert_eq!(latest.expect("some"), row2);

    let other = store
        .record_suspension(&suspension(
            &hand("n1", 1, "t2"),
            1,
            SuspensionReason::Wait,
            "waiting",
            "dependency open",
        ))
        .expect("other lineage");
    assert_eq!(other.suspension, 1);
}

#[test]
fn same_second_messages_stay_distinct_and_redelivery_is_a_no_op() {
    let (_dir, store) = open();
    store
        .store_message("n1", 1, "t1", &message("m-1", "op", 1_000, "first reply"))
        .expect("store m-1");
    store
        .store_message("n1", 1, "t1", &message("m-2", "op", 1_000, "second reply"))
        .expect("store m-2");
    store
        .store_message("n1", 1, "t1", &message("m-1", "op", 1_000, "redelivered"))
        .expect("redelivery");

    let unconsumed = store
        .unconsumed_messages("n1", 1, "t1")
        .expect("unconsumed");
    assert_eq!(ids(&unconsumed), vec!["m-1", "m-2"]);
    assert_eq!(unconsumed[0].seq, 1);
    assert_eq!(unconsumed[1].seq, 2);
    assert_eq!(unconsumed[0].content, "first reply");
    assert_eq!(unconsumed[1].content, "second reply");
    assert!(unconsumed.iter().all(|row| row.consumed_by.is_none()));

    assert!(
        store
            .unconsumed_messages("n1", 1, "t2")
            .expect("other lineage")
            .is_empty()
    );
}

#[test]
fn intent_creation_is_idempotent_and_claims_each_message_once() {
    let (_dir, store) = open();
    let k = hand("n1", 1, "t1");
    let g1 = store.register_execution(&k).expect("register");
    assert_eq!(g1.generation, 1);
    store
        .record_suspension(&suspension(
            &k,
            1,
            SuspensionReason::RoleInputs,
            "waiting",
            "inputs",
        ))
        .expect("suspend");
    store
        .store_message("n1", 1, "t1", &message("m-1", "op", 1_000, "first"))
        .expect("store m-1");
    store
        .store_message("n1", 1, "t1", &message("m-2", "op", 1_000, "second"))
        .expect("store m-2");

    let row = fresh(
        store
            .create_resume_intent(&intent(&k, 1, "job-x-resume", 1, 2, &["m-1", "m-2"]))
            .expect("create"),
    );
    assert_eq!(row.replacement_job, "job-x-resume");
    assert_eq!(row.from_generation, 1);
    assert_eq!(row.to_generation, 2);
    assert_eq!(row.execution, execution_id(&k, 1));
    assert_eq!(row.spec.id, "job-x-resume");
    assert_eq!(ids(&row.messages), vec!["m-1", "m-2"]);
    assert_eq!(row.messages[0].content, "first");
    assert_eq!(row.messages[0].consumed_by.as_deref(), Some("job-x-resume"));
    assert!(row.is_incomplete());
    assert!(
        store
            .unconsumed_messages("n1", 1, "t1")
            .expect("none")
            .is_empty()
    );

    let mut drifted = intent(&k, 1, "other-identity", 9, 9, &[]);
    drifted.spec.body = "a completely different brief".into();
    let replay = existing(store.create_resume_intent(&drifted).expect("replay"));
    assert_eq!(replay, row);
    assert_eq!(replay.spec.body, "body");
    assert_eq!(
        store.resume_intents("n1", 1, "t1").expect("history").len(),
        1
    );
    let active = store.active_execution(&k).expect("active").expect("some");
    assert_eq!(active.generation, 1);

    store
        .record_suspension(&suspension(
            &k,
            2,
            SuspensionReason::Lock,
            "blocked",
            "lock",
        ))
        .expect("repark");
    assert!(
        store
            .create_resume_intent(&intent(&k, 2, "job-y-resume", 2, 3, &["nope"]))
            .is_err()
    );
    assert!(
        store
            .create_resume_intent(&intent(&k, 2, "job-y-resume", 2, 3, &["m-1"]))
            .is_err()
    );
    assert_eq!(
        store.resume_intents("n1", 1, "t1").expect("history").len(),
        1
    );
    let claimed = store.resume_messages("n1", 1, "t1").expect("messages");
    assert!(
        claimed
            .iter()
            .all(|row| row.consumed_by.as_deref() == Some("job-x-resume"))
    );
}

#[test]
fn a_second_suspension_generation_requires_new_messages() {
    let (_dir, store) = open();
    let k = hand("n1", 1, "t1");
    let g1 = store.register_execution(&k).expect("register");

    store
        .record_suspension(&suspension(
            &k,
            1,
            SuspensionReason::RoleInputs,
            "waiting",
            "inputs",
        ))
        .expect("suspend 1");
    store
        .store_message("n1", 1, "t1", &message("m-1", "op", 1_000, "first"))
        .expect("store m-1");
    let first = fresh(
        store
            .create_resume_intent(&intent(&k, 1, "job-x-resume", 1, 2, &["m-1"]))
            .expect("resume 1"),
    );
    assert_eq!(first.to_generation, 2);
    let g2 = store.consume_retry(&g1.execution).expect("mint 2");
    assert_eq!(g2.generation, 2);

    let reparked = store
        .record_suspension(&suspension(
            &k,
            2,
            SuspensionReason::Milestone,
            "held",
            "gate",
        ))
        .expect("suspend 2");
    assert_eq!(reparked.execution, g2.execution);
    assert!(
        store
            .unconsumed_messages("n1", 1, "t1")
            .expect("drained")
            .is_empty()
    );
    store
        .store_message("n1", 1, "t1", &message("m-2", "op", 2_000, "second"))
        .expect("store m-2");
    assert_eq!(
        ids(&store.unconsumed_messages("n1", 1, "t1").expect("fresh")),
        vec!["m-2"]
    );

    let second = fresh(
        store
            .create_resume_intent(&intent(&k, 2, "job-y-resume", 2, 3, &["m-2"]))
            .expect("resume 2"),
    );
    assert_eq!(second.from_generation, 2);
    assert_eq!(second.to_generation, 3);

    let history = store.suspensions("n1", 1, "t1").expect("suspensions");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].generation, 1);
    assert_eq!(history[1].generation, 2);
    assert_eq!(history[1].park_task, reparked.park_task);

    let intents = store.resume_intents("n1", 1, "t1").expect("intents");
    assert_eq!(intents.len(), 2);
    assert_eq!(ids(&intents[0].messages), vec!["m-1"]);
    assert_eq!(ids(&intents[1].messages), vec!["m-2"]);
    let answered = store
        .resume_intent_for_suspension(&k, 2)
        .expect("lookup")
        .expect("some");
    assert_eq!(answered.replacement_job, "job-y-resume");
}

#[test]
fn lifecycle_stamps_are_idempotent_and_survive_save_and_reopen() {
    let (dir, mut store) = open();
    let k = hand("n1", 1, "t1");
    let g1 = store.register_execution(&k).expect("register");
    store
        .record_suspension(&suspension(
            &k,
            1,
            SuspensionReason::RoleInputs,
            "waiting",
            "inputs",
        ))
        .expect("suspend 1");
    store
        .store_message("n1", 1, "t1", &message("m-1", "op", 1_000, "first"))
        .expect("store m-1");
    store
        .create_resume_intent(&intent(&k, 1, "job-x-resume", 1, 2, &["m-1"]))
        .expect("resume 1");

    let _g2 = store.consume_retry(&g1.execution).expect("mint 2");
    store
        .record_suspension(&suspension(
            &k,
            2,
            SuspensionReason::Uncertain,
            "unsure",
            "publication",
        ))
        .expect("suspend 2");
    store
        .store_message("n1", 1, "t1", &message("m-2", "op", 2_000, "second"))
        .expect("store m-2");
    store
        .create_resume_intent(&intent(&k, 2, "job-y-resume", 2, 3, &["m-2"]))
        .expect("resume 2");

    store
        .mark_resume_materialized("job-x-resume", 1_500)
        .expect("materialize");
    store
        .mark_resume_materialized("job-x-resume", 2_500)
        .expect("materialize again");
    store
        .complete_resume_intent("job-x-resume", 3_000)
        .expect("complete");
    store
        .complete_resume_intent("job-x-resume", 4_000)
        .expect("complete again");
    store
        .cancel_resume_intent("job-y-resume", 5_000)
        .expect("cancel");
    assert!(
        store
            .incomplete_resume_intents()
            .expect("incomplete")
            .is_empty()
    );
    assert!(store.complete_resume_intent("no-such-intent", 1).is_err());

    let suspensions = store.suspensions("n1", 1, "t1").expect("suspensions");
    let messages = store.resume_messages("n1", 1, "t1").expect("messages");
    let intents = store.resume_intents("n1", 1, "t1").expect("intents");
    assert_eq!(intents[0].materialized_at, Some(1_500));
    assert_eq!(intents[0].completed_at, Some(3_000));
    assert_eq!(intents[1].cancelled_at, Some(5_000));

    store.save(&State::default()).expect("save");
    drop(store);
    let store = reopen(&dir);

    assert_eq!(
        store.suspensions("n1", 1, "t1").expect("suspensions"),
        suspensions
    );
    assert_eq!(
        store.resume_messages("n1", 1, "t1").expect("messages"),
        messages
    );
    assert_eq!(
        store.resume_intents("n1", 1, "t1").expect("intents"),
        intents
    );
    let replay = existing(
        store
            .create_resume_intent(&intent(&k, 1, "drifted-identity", 1, 9, &[]))
            .expect("replay"),
    );
    assert_eq!(replay, intents[0]);
    assert_eq!(replay.messages, intents[0].messages);
    let settled = store
        .resume_intent("job-x-resume")
        .expect("by identity")
        .expect("some");
    assert!(!settled.is_incomplete());
}
