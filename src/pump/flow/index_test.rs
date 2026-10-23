use super::*;
use serde_json::json;

fn run() -> RunRow {
    RunRow {
        need: "n1".into(),
        run: 3,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: run_state::LIVE.into(),
    }
}

#[test]
fn constructors_copy_identity_from_parent() {
    let step = run().step("plan", "t1", StepState::Running);
    assert_eq!(step.run, 3);
    assert_eq!(step.node, "plan");
    assert_eq!(step.task, "t1");
    assert_eq!(step.state, StepState::Running);
    assert_eq!(step.verdict, None);

    let job = step.job("j1", 1);
    assert_eq!(
        (job.run, job.node.as_str(), job.task.as_str()),
        (3, "plan", "t1")
    );
    assert_eq!((job.job.as_str(), job.attempt), ("j1", 1));

    let next = job.next("j2");
    assert_eq!(next.attempt, 2);
    assert_eq!(
        (next.run, next.node.as_str(), next.task.as_str()),
        (3, "plan", "t1")
    );
    assert_eq!(next.job, "j2");

    let again = next.again("j2-c1");
    assert_eq!(again.attempt, 2);
    assert_eq!(again.job, "j2-c1");
    assert_eq!(again.task, "t1");
}

#[test]
fn stamp_overwrites_identity_and_keeps_other_keys() {
    let row = run().step("plan", "t1", StepState::Running).job("j1", 2);
    let mut payload =
        json!({"need": "old", "run": 9, "node": "x", "task": "y", "attempt": 7, "kind": "k"});
    row.stamp("n1", &mut payload);
    assert_eq!(payload["need"], "n1");
    assert_eq!(payload["run"], 3);
    assert_eq!(payload["node"], "plan");
    assert_eq!(payload["task"], "t1");
    assert_eq!(payload["attempt"], 2);
    assert_eq!(payload["kind"], "k");

    let id = PayloadIdentity::of(&payload).unwrap();
    assert_eq!(
        id,
        PayloadIdentity {
            need: "n1".into(),
            run: 3,
            task: "t1".into(),
            attempt: Some(2)
        }
    );

    let mut not_object = json!("text");
    row.stamp("n1", &mut not_object);
    assert_eq!(not_object["need"], "n1");
}

#[test]
fn identity_names_the_bad_field() {
    let err = PayloadIdentity::of(&json!({"need": "n1", "task": "t"})).unwrap_err();
    assert_eq!(
        err,
        "payload field `run`: expected a number; actual: missing"
    );
    let err = PayloadIdentity::of(&json!({"need": "n1", "task": "t", "run": "3"})).unwrap_err();
    assert!(err.contains("`run`") && err.contains("\"3\""), "{err}");
    let err = PayloadIdentity::of(&json!({"need": 5, "task": "t", "run": 1})).unwrap_err();
    assert!(err.contains("`need`"), "{err}");
    let err = PayloadIdentity::of(&json!({"need": "n", "run": 1})).unwrap_err();
    assert!(err.contains("`task`"), "{err}");
    let ok = PayloadIdentity::of(&json!({"need": "n", "task": "t", "run": 1})).unwrap();
    assert_eq!(ok.attempt, None);
}

#[test]
fn run_state_vocabulary() {
    for word in run_state::ALL {
        assert!(run_state::is_known(word), "{word}");
    }
    assert!(!run_state::is_known("lock"));
    assert_eq!(run_state::ALL.len(), 7);
}
