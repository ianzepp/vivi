//! Tests for the continuation tables in [`super`]: the park-task link, the
//! run's thread-input journal, and pending same-run resume decisions.

use tempfile::{TempDir, tempdir};

use super::{ContinuationLink, ResumeRequest, ResumeState, ThreadInput};
use crate::pump::flow::index::{RunRow, StepState, run_state};
use crate::pump::store::{DB_FILENAME, PumpStore};

fn open(dir: &TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open")
}

fn run_row(need: &str, run: u32) -> RunRow {
    RunRow {
        need: need.into(),
        run,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "research".into(),
        state: run_state::LIVE.into(),
    }
}

/// One run with a plan step (done, verdict `operator`), a second plan step
/// (done, verdict `hold`) and the park step (waiting).
fn seed(store: &PumpStore, need: &str, run: u32, tasks: [&str; 3]) {
    let row = run_row(need, run);
    store.insert_run(&row).expect("insert run");
    let mut plan = row.step("plan", tasks[0], StepState::Done);
    plan.verdict = Some("operator".into());
    let mut plan2 = row.step("plan 2", tasks[1], StepState::Done);
    plan2.verdict = Some("hold".into());
    let park = row.step("parked", tasks[2], StepState::Waiting);
    for step in [plan, plan2, park] {
        store.insert_run_step(&row, &step).expect("insert step");
    }
}

fn link(need: &str, run: u32, park_task: &str, node: &str, source: &str) -> ContinuationLink {
    ContinuationLink {
        task: park_task.into(),
        need: need.into(),
        run,
        node: node.into(),
        source: source.into(),
    }
}

fn input(id: &str, body: &str) -> ThreadInput {
    ThreadInput {
        message_id: format!("<{id}@thread>"),
        author: "op@example.test".into(),
        body: body.into(),
        sent_at: 1_700_000_000,
    }
}

fn resume(need: &str, run: u32, task: &str, inputs: Vec<ThreadInput>) -> ResumeRequest {
    ResumeRequest {
        need: need.into(),
        run,
        node: "plan".into(),
        task: task.into(),
        park_task: None,
        inputs,
    }
}

fn ids(inputs: &[ThreadInput]) -> Vec<String> {
    inputs
        .iter()
        .map(|input| input.message_id.clone())
        .collect()
}

#[test]
fn link_round_trips_and_reloads() {
    let dir = tempdir().expect("tempdir");
    {
        let store = open(&dir);
        seed(&store, "n1", 1, ["t-plan", "t-plan2", "t-park"]);
        let link = link("n1", 1, "t-park", "parked", "t-plan");
        store.link_continuation(&link).expect("link");
        assert_eq!(store.continuation_of("t-park").expect("read"), Some(link));
        assert_eq!(store.continuation_of("t-plan").expect("read"), None);
    }
    let reopened = open(&dir);
    assert_eq!(
        reopened.continuation_of("t-park").expect("read"),
        Some(link("n1", 1, "t-park", "parked", "t-plan"))
    );
}

#[test]
fn repeated_identical_link_is_a_no_op() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    seed(&store, "n1", 1, ["t-plan", "t-plan2", "t-park"]);
    let link = link("n1", 1, "t-park", "parked", "t-plan");
    store.link_continuation(&link).expect("first link");
    store.link_continuation(&link).expect("same link again");
    assert_eq!(store.continuation_of("t-park").expect("read"), Some(link));
}

#[test]
fn second_different_source_is_refused() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    seed(&store, "n1", 1, ["t-plan", "t-plan2", "t-park"]);
    store
        .link_continuation(&link("n1", 1, "t-park", "parked", "t-plan"))
        .expect("first link");
    let conflict = store.link_continuation(&link("n1", 1, "t-park", "parked", "t-plan2"));
    assert!(conflict.is_err(), "conflicting link must fail");
    assert_eq!(
        store.continuation_of("t-park").expect("read"),
        Some(link("n1", 1, "t-park", "parked", "t-plan"))
    );
}

#[test]
fn link_needs_the_park_step_in_its_need_and_run() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    seed(&store, "n1", 1, ["t-plan", "t-plan2", "t-park"]);
    let missing = store.link_continuation(&link("n1", 1, "t-none", "parked", "t-plan"));
    assert!(missing.is_err(), "unknown park task must fail");
    let other_node = store.link_continuation(&link("n1", 1, "t-park", "park", "t-plan"));
    assert!(other_node.is_err(), "wrong node must fail");
    let other_need = store.link_continuation(&link("n2", 1, "t-park", "parked", "t-plan"));
    assert!(other_need.is_err(), "park of another need must fail");
    let other_run = store.link_continuation(&link("n1", 2, "t-park", "parked", "t-plan"));
    assert!(other_run.is_err(), "park of another run must fail");
    assert_eq!(store.continuation_of("t-park").expect("read"), None);
}

#[test]
fn link_needs_the_source_step_in_the_same_need_and_run() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    seed(&store, "n1", 1, ["t-plan", "t-plan2", "t-park"]);
    seed(&store, "n1", 2, ["t-plan-r2", "t-plan2-r2", "t-park-r2"]);
    seed(&store, "n2", 1, ["t-plan-n2", "t-plan2-n2", "t-park-n2"]);
    let ghost = store.link_continuation(&link("n1", 1, "t-park", "parked", "t-ghost"));
    assert!(ghost.is_err(), "unknown source must fail");
    let other_run = store.link_continuation(&link("n1", 1, "t-park", "parked", "t-plan-r2"));
    assert!(other_run.is_err(), "source of another run must fail");
    let other_need = store.link_continuation(&link("n1", 1, "t-park", "parked", "t-plan-n2"));
    assert!(other_need.is_err(), "source of another need must fail");
    assert_eq!(store.continuation_of("t-park").expect("read"), None);
}

#[test]
fn same_timestamp_messages_are_distinct_inputs() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let mut first = input("m1", "first");
    first.sent_at = 42;
    let mut second = input("m2", "second");
    second.sent_at = 42;
    store
        .journal_run_inputs("n1", 1, &[first.clone(), second.clone()])
        .expect("journal");
    assert_eq!(
        store.run_inputs("n1", 1).expect("read"),
        vec![first, second],
        "two distinct messages may share one timestamp"
    );
}

#[test]
fn ordered_batched_input_keeps_thread_order() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let batch = [input("m1", "one"), input("m2", "two"), input("m3", "three")];
    store.journal_run_inputs("n1", 1, &batch).expect("journal");
    store
        .journal_run_inputs("n1", 1, &[input("m2", "again"), input("m4", "four")])
        .expect("re-observe");
    let expected = vec!["<m1@thread>", "<m2@thread>", "<m3@thread>", "<m4@thread>"];
    assert_eq!(
        ids(&store.run_inputs("n1", 1).expect("read")),
        expected,
        "batches append in order and re-observed identities are a no-op"
    );
    store
        .record_source_inputs(
            "n1",
            1,
            "plan",
            "t-source",
            &[input("m3", "three"), input("m4", "four")],
        )
        .expect("record source inputs");
    assert_eq!(
        ids(&store
            .source_inputs("n1", 1, "plan", "t-source")
            .expect("read")),
        vec!["<m3@thread>", "<m4@thread>"],
        "the source execution keeps the order it included"
    );
}

#[test]
fn duplicate_observation_returns_the_same_decision() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let batch = vec![input("m1", "one"), input("m2", "two")];
    let first = store
        .observe_resume(&resume("n1", 1, "t-source", batch.clone()))
        .expect("observe");
    let mut edited = batch.clone();
    edited[1].body = "rewritten".to_string();
    let again = store
        .observe_resume(&resume("n1", 1, "t-source", edited))
        .expect("observe again");
    assert_eq!(first, again, "the first snapshot of a batch wins");
    assert_eq!(first.generation, 1);
    let fresh = store
        .observe_resume(&resume("n1", 1, "t-source", vec![input("m3", "three")]))
        .expect("observe new batch");
    assert_ne!(fresh.decision, first.decision);
    assert_eq!(fresh.generation, 2, "a new batch is the next generation");
    assert_eq!(
        store
            .observe_resume(&resume("n1", 1, "t-source", batch))
            .expect("observe"),
        first,
        "the first batch still returns its own decision"
    );
    let empty = store.observe_resume(&resume("n1", 1, "t-source", vec![]));
    assert!(empty.is_err(), "an empty batch resumes nothing");
}

#[test]
fn consumption_is_per_source_task() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let batch = vec![input("m1", "the reply")];
    let a = store
        .observe_resume(&resume("n1", 1, "t-plan", batch.clone()))
        .expect("observe a");
    let b = store
        .observe_resume(&resume("n1", 1, "t-plan2", batch))
        .expect("observe b");
    assert_ne!(a.decision, b.decision, "the source scopes the decision");
    assert_eq!(b.generation, 1, "each source counts its own decisions");
    store
        .complete_resume(&a.decision)
        .expect("complete a")
        .expect("decision");
    assert!(store.input_consumed("t-plan", "<m1@thread>").expect("read"));
    assert!(
        !store
            .input_consumed("t-plan2", "<m1@thread>")
            .expect("read")
    );
    assert_eq!(
        store.consumed_inputs("t-plan").expect("read"),
        vec!["<m1@thread>".to_owned()]
    );
    assert_eq!(
        ids(&store.pending_resumes().expect("read")[0].inputs),
        vec!["<m1@thread>".to_owned()],
        "the other held step is still pending on the same reply"
    );
    store
        .complete_resume(&b.decision)
        .expect("complete b")
        .expect("decision");
    assert!(
        store
            .input_consumed("t-plan2", "<m1@thread>")
            .expect("read")
    );
    assert!(store.pending_resumes().expect("read").is_empty());
}

#[test]
fn decisions_survive_a_reload() {
    let dir = tempdir().expect("tempdir");
    let decision_id;
    {
        let store = open(&dir);
        store
            .journal_run_inputs("n1", 1, &[input("m1", "one")])
            .expect("journal");
        let mut request = resume(
            "n1",
            1,
            "t-source",
            vec![input("m1", "one"), input("m2", "two")],
        );
        request.park_task = Some("t-park".into());
        let decision = store.observe_resume(&request).expect("observe");
        decision_id = decision.decision.clone();
        let updated = store
            .set_resume_replacement(&decision_id, "t-replacement", 7)
            .expect("replacement")
            .expect("decision");
        assert_eq!(updated.replacement_id.as_deref(), Some("t-replacement"));
        assert_eq!(updated.replacement_order, Some(7));
    }
    let reopened = open(&dir);
    let decision = reopened
        .resume_decision(&decision_id)
        .expect("read")
        .expect("decision");
    assert_eq!(decision.state, ResumeState::Pending);
    assert_eq!(decision.park_task.as_deref(), Some("t-park"));
    assert_eq!(decision.generation, 1);
    assert_eq!(decision.progress, 0);
    assert_eq!(ids(&decision.inputs), vec!["<m1@thread>", "<m2@thread>"]);
    assert_eq!(decision.replacement_id.as_deref(), Some("t-replacement"));
    assert_eq!(reopened.pending_resumes().expect("read").len(), 1);
    assert_eq!(
        ids(&reopened.run_inputs("n1", 1).expect("read")),
        vec!["<m1@thread>"],
        "the run journal survives too"
    );
}

#[test]
fn partial_progress_stays_recoverable_until_completed() {
    let dir = tempdir().expect("tempdir");
    let batch = vec![input("m1", "one"), input("m2", "two")];
    let decision_id = open(&dir)
        .observe_resume(&resume("n1", 1, "t-source", batch))
        .expect("observe")
        .decision;
    {
        let store = open(&dir);
        store
            .set_resume_progress(&decision_id, 1)
            .expect("progress")
            .expect("decision");
    }
    let reopened = open(&dir);
    let resumed = reopened
        .resume_decision(&decision_id)
        .expect("read")
        .expect("decision");
    assert_eq!(
        resumed.state,
        ResumeState::Pending,
        "intent is not completion"
    );
    assert_eq!(resumed.progress, 1);
    assert_eq!(reopened.pending_resumes().expect("read").len(), 1);
    assert!(
        !reopened
            .input_consumed("t-source", "<m1@thread>")
            .expect("read")
    );
    assert!(
        reopened.set_resume_progress(&decision_id, 3).is_err(),
        "progress cannot pass the batch"
    );
    let done = reopened
        .complete_resume(&decision_id)
        .expect("complete")
        .expect("decision");
    assert_eq!(done.state, ResumeState::Applied);
    assert!(
        reopened
            .input_consumed("t-source", "<m1@thread>")
            .expect("read")
    );
    assert!(
        reopened
            .input_consumed("t-source", "<m2@thread>")
            .expect("read")
    );
    assert!(reopened.pending_resumes().expect("read").is_empty());
    let again = reopened
        .complete_resume(&decision_id)
        .expect("complete again")
        .expect("decision");
    assert_eq!(again.state, ResumeState::Applied);
    assert!(
        reopened.cancel_resume(&decision_id).is_err(),
        "applied is final"
    );
}

#[test]
fn cancelled_decisions_stay_out_of_pending() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let batch = vec![input("m1", "one")];
    let decision = store
        .observe_resume(&resume("n1", 1, "t-source", batch.clone()))
        .expect("observe");
    let cancelled = store
        .cancel_resume(&decision.decision)
        .expect("cancel")
        .expect("decision");
    assert_eq!(cancelled.state, ResumeState::Cancelled);
    assert!(store.pending_resumes().expect("read").is_empty());
    let again = store
        .cancel_resume(&decision.decision)
        .expect("cancel again")
        .expect("decision");
    assert_eq!(again.state, ResumeState::Cancelled);
    assert!(
        store.set_resume_progress(&decision.decision, 1).is_err(),
        "a cancelled decision takes no progress"
    );
    let replayed = store
        .observe_resume(&resume("n1", 1, "t-source", batch))
        .expect("observe again");
    assert_eq!(replayed.decision, decision.decision);
    assert_eq!(replayed.state, ResumeState::Cancelled);
    assert!(
        !store
            .input_consumed("t-source", "<m1@thread>")
            .expect("read")
    );
}

#[test]
fn decisions_of_other_sources_and_runs_are_not_confused() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let batch = vec![input("m1", "one")];
    let first = store
        .observe_resume(&resume("n1", 1, "t-source", batch.clone()))
        .expect("observe");
    let other_task = store
        .observe_resume(&resume("n1", 1, "t-source2", batch.clone()))
        .expect("observe other task");
    let other_run = store
        .observe_resume(&resume("n1", 2, "t-source", batch.clone()))
        .expect("observe other run");
    let other_need = store
        .observe_resume(&resume("n2", 1, "t-source", batch))
        .expect("observe other need");
    for other in [&other_task, &other_run, &other_need] {
        assert_ne!(other.decision, first.decision);
        assert_eq!(other.generation, 1, "each source counts its own decisions");
    }
    assert_eq!(store.pending_resumes().expect("read").len(), 4);
    assert!(
        store
            .resume_decision("resume-none")
            .expect("read")
            .is_none(),
        "an unknown decision reads as none"
    );
    assert!(
        store
            .set_resume_replacement("resume-none", "t-job", 1)
            .expect("read")
            .is_none()
    );
}
