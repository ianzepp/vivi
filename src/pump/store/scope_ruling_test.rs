//! Tests for the scope ruling table in [`super`].

use tempfile::tempdir;

use super::{RulingDecision, ScopeRulingRequest, continuation_id, decision_reference};
use crate::pump::config::scope::{ScopePolicy, ScopeRejection};
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore, SCHEMA_VERSION};

fn policy() -> ScopePolicy {
    ScopePolicy::snapshot(
        "web",
        &["./".into(), "vendor/".into()],
        &["secrets/".into()],
    )
    .expect("policy snapshot")
}

fn request(job: &str, task: &str, decision: RulingDecision) -> ScopeRulingRequest {
    ScopeRulingRequest {
        job: job.into(),
        job_kind: "hand".into(),
        need: "56d07dc5".into(),
        run: 1,
        node: "slice:56d07dc5-5".into(),
        task: task.into(),
        role: "hand".into(),
        target: "web".into(),
        paths: vec!["./src/".into(), "src/pump/./store.rs".into(), ".".into()],
        reason: "the slice needs src/ to land its units".into(),
        decision,
        policy_reason: None,
        policy: policy(),
    }
}

#[test]
fn approved_ruling_round_trips() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    let stored = store
        .insert_scope_ruling(&request("slice-1-a1", "task-1", RulingDecision::Approved))
        .expect("insert");
    assert_eq!(stored.decision, RulingDecision::Approved);
    assert!(stored.decision.is_grant());
    assert_eq!(stored.paths, ["src/", "src/pump/store.rs", "./"]);
    assert!(stored.invalid.is_empty());
    assert_eq!(
        stored.decision_ref,
        Some(decision_reference("slice-1-a1", RulingDecision::Approved))
    );
    assert_eq!(stored.reply_body, None);
    assert_eq!(stored.posted_at, None);
    assert_eq!(stored.continuation_id, None);
    assert_eq!(stored.continuation_posted_at, None);
    assert!(stored.created_at > 0);
    assert_eq!(
        store.scope_ruling("slice-1-a1").expect("read"),
        Some(stored)
    );
    assert_eq!(store.scope_ruling("missing").expect("read"), None);
}

#[test]
fn operator_rulings_round_trip_by_task() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    let granted = store
        .insert_scope_ruling(&request("j1", "task-9", RulingDecision::OperatorApproved))
        .expect("insert");
    assert!(granted.decision.is_grant());
    let mut refused = request("j2", "task-9", RulingDecision::OperatorDenied);
    refused.policy_reason = Some(ScopeRejection::DeniedPath {
        path: "secrets/key".into(),
        deny: "secrets/".into(),
    });
    let refused = store.insert_scope_ruling(&refused).expect("insert");
    assert!(!refused.decision.is_grant());
    assert_eq!(
        store.scope_rulings_for_task("task-9").expect("read"),
        [granted, refused]
    );
    assert!(
        store
            .scope_rulings_for_task("other")
            .expect("read")
            .is_empty()
    );
}

#[test]
fn reuse_returns_the_original_decision() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    let first = store
        .insert_scope_ruling(&request("j1", "t1", RulingDecision::Approved))
        .expect("insert");
    let replay = store
        .insert_scope_ruling(&request("j1", "t1", RulingDecision::OperatorDenied))
        .expect("reuse");
    assert_eq!(replay, first);
    assert_eq!(replay.decision, RulingDecision::Approved);
    assert_eq!(store.scope_rulings_for_task("t1").expect("read").len(), 1);
}

#[test]
fn incompatible_reuse_fails_and_keeps_the_original() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    let first = store
        .insert_scope_ruling(&request("j1", "t1", RulingDecision::Approved))
        .expect("insert");
    let mut drifted = request("j1", "t1", RulingDecision::Approved);
    drifted.task = "t2".into();
    let error = store
        .insert_scope_ruling(&drifted)
        .expect_err("incompatible");
    assert!(error.to_string().contains("task differs"), "{error}");
    let mut other_ask = request("j1", "t1", RulingDecision::Approved);
    other_ask.paths = vec!["docs/".into()];
    let error = store
        .insert_scope_ruling(&other_ask)
        .expect_err("different request");
    assert!(error.to_string().contains("paths differs"), "{error}");
    assert_eq!(store.scope_ruling("j1").expect("read"), Some(first));
}

#[test]
fn invalid_paths_are_evidence_and_typed_reasons_round_trip() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    let mut ask = request("j1", "t1", RulingDecision::Denied);
    ask.paths = vec!["/abs/path".into(), "../out".into(), "src/lib.rs".into()];
    ask.policy_reason = Some(ScopeRejection::OutsideAllow {
        path: "/abs/path".into(),
        allow: vec!["./".into()],
    });
    let stored = store.insert_scope_ruling(&ask).expect("insert");
    assert!(!stored.decision.is_grant());
    assert_eq!(stored.paths, ["src/lib.rs"]);
    assert_eq!(
        stored.invalid,
        [
            super::InvalidScopePath {
                path: "/abs/path".into(),
                problem: "a scope path must be repository-relative, not absolute".into(),
            },
            super::InvalidScopePath {
                path: "../out".into(),
                problem: "a scope path must not climb out of the repository with `..`".into(),
            },
        ]
    );
    assert_eq!(
        stored.policy_reason,
        Some(ScopeRejection::OutsideAllow {
            path: "/abs/path".into(),
            allow: vec!["./".into()],
        })
    );
    assert_eq!(stored.policy.target, "web");
    assert_eq!(stored.policy.allow, ["./", "vendor/"]);
    assert_eq!(stored.policy.deny, ["secrets/"]);
    assert_eq!(store.scope_ruling("j1").expect("read"), Some(stored));
}

#[test]
fn legacy_state_save_and_reopen_keep_rulings() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join(DB_FILENAME);
    let mut store = PumpStore::open(&path).expect("open");
    let stored = store
        .insert_scope_ruling(&request("j1", "t1", RulingDecision::Approved))
        .expect("insert");
    store.save(&State::default()).expect("save legacy state");
    assert_eq!(
        store.scope_ruling("j1").expect("read"),
        Some(stored.clone())
    );
    drop(store);
    let store = PumpStore::open(&path).expect("reopen");
    assert_eq!(store.scope_ruling("j1").expect("read"), Some(stored));
    assert_eq!(store.schema_version().expect("version"), SCHEMA_VERSION);
}

#[test]
fn decision_and_continuation_identities_are_deterministic() {
    let approved = decision_reference("j1", RulingDecision::Approved);
    assert_eq!(decision_reference("j1", RulingDecision::Approved), approved);
    assert_ne!(decision_reference("j1", RulingDecision::Denied), approved);
    assert_ne!(decision_reference("j2", RulingDecision::Approved), approved);
    assert!(approved.starts_with("scope-"));
    assert_eq!(continuation_id("j1"), "j1-c1");
    assert_eq!(continuation_id("j1-c1"), "j1-c2");
}
