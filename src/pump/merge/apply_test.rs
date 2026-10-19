use tempfile::tempdir;

use super::apply_failure;
use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;
use crate::pump::state::{Attempt, MergeResult, NeedRecord, State, Status, Unit};

const PROOF_FAILURE: &str = "After merging into the integration branch, this command failed (exit 1):\ncargo test failed_case\n\nLast output:\n---- failed_case stdout ----\nassertion failed";

fn failure() -> MergeResult {
    MergeResult {
        uid: String::from("u1"),
        ok: Some(false),
        why: Some(String::from("proof-failed")),
        detail: Some(PROOF_FAILURE.to_owned()),
        ..MergeResult::default()
    }
}

#[test]
fn the_first_proof_failure_keeps_the_normal_attempt_path() {
    let dir = tempdir().expect("temporary home");
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let mut state = State::default();
    state.units.insert(
        String::from("u1"),
        Unit {
            need: String::from("n1"),
            status: Status::Merging,
            attempts: 1,
            task: Some(String::from("task-1")),
            proof: vec![String::from("cargo test failed_case")],
            ..Unit::default()
        },
    );

    apply_failure(&ctx, &mut state, &failure());

    let unit = &state.units["u1"];
    assert_eq!(unit.status, Status::Pending);
    assert_eq!(unit.history.len(), 1);
    assert_eq!(unit.history[0].why, "proof-failed");
    assert_eq!(unit.attempts, 1);
}

#[test]
fn the_same_proof_failure_sends_the_unit_for_revision_without_an_attempt_record() {
    let dir = tempdir().expect("temporary home");
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let mut state = State::default();
    state.needs.insert(
        String::from("n1"),
        NeedRecord {
            verdict: String::from("units"),
            units: vec![String::from("u1")],
            ..NeedRecord::default()
        },
    );
    state.units.insert(
        String::from("u1"),
        Unit {
            need: String::from("n1"),
            status: Status::Merging,
            attempts: 2,
            task: Some(String::from("task-2")),
            proof: vec![String::from("cargo test failed_case")],
            history: vec![Attempt {
                attempt: 1,
                why: String::from("proof-failed"),
                detail: PROOF_FAILURE.to_owned(),
                ..Attempt::default()
            }],
            ..Unit::default()
        },
    );

    apply_failure(&ctx, &mut state, &failure());

    let unit = &state.units["u1"];
    assert_eq!(unit.status, Status::NeedsReplan);
    assert_eq!(unit.attempts, 2, "the dispatched branch number is retained");
    assert_eq!(
        unit.history.len(),
        1,
        "the repeated failure is not recorded"
    );
    assert!(unit.task.is_none());
    assert!(state.needs["n1"].amendment_pending);
    assert!(dir.path().join("home/needs/n1.json").is_file());
    assert!(
        !host
            .commands()
            .iter()
            .any(|command| command.contains("mail reply n1")),
        "the no-attempt path must not post a second attempt receipt"
    );
}

#[test]
fn an_explicit_out_of_scope_hand_report_requests_revision_on_first_failure() {
    let dir = tempdir().expect("temporary home");
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.reply(
        "task show task-1",
        0,
        "summary\n\n--- receipt\nOUT-OF-SCOPE: docs/budget.toml\nThe proof cannot pass without that file.",
    );
    let mut state = State::default();
    state.needs.insert(
        String::from("n1"),
        NeedRecord {
            verdict: String::from("units"),
            units: vec![String::from("u1")],
            ..NeedRecord::default()
        },
    );
    state.units.insert(
        String::from("u1"),
        Unit {
            need: String::from("n1"),
            status: Status::Merging,
            attempts: 1,
            task: Some(String::from("task-1")),
            proof: vec![String::from("cargo test failed_case")],
            write_scope: vec![String::from("src/unit.rs")],
            ..Unit::default()
        },
    );

    apply_failure(&ctx, &mut state, &failure());

    let unit = &state.units["u1"];
    assert_eq!(unit.status, Status::NeedsReplan);
    assert_eq!(unit.attempts, 1);
    assert!(unit.history.is_empty());
    assert!(state.needs["n1"].amendment_pending);
    assert!(
        !host
            .commands()
            .iter()
            .any(|command| command.contains("mail reply n1")),
        "the scope handoff must not post a unit attempt"
    );
}
