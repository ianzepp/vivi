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

fn need_target(
    job: &str,
    need: &str,
    target: &str,
    decision: RulingDecision,
) -> ScopeRulingRequest {
    let mut ask = request(job, "t1", decision);
    ask.need = need.into();
    ask.target = target.into();
    ask.paths = vec!["src/".into()];
    ask
}

#[test]
fn effective_scope_returns_original_additions_and_effective_distinctly() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    let original = vec![String::from("docs/"), String::from("src/pump/")];
    let none = store
        .effective_need_scope("56d07dc5", "web", &original)
        .expect("read");
    assert_eq!(none.original, ["docs/", "src/pump/"]);
    assert!(none.additions.is_empty());
    assert_eq!(none.effective, ["docs/", "src/pump/"]);

    let mut ask = need_target("j1", "56d07dc5", "web", RulingDecision::Approved);
    ask.paths = vec!["./src/./pump/".into(), "docs/".into(), "skills/".into()];
    store.insert_scope_ruling(&ask).expect("insert");
    let granted = store
        .effective_need_scope("56d07dc5", "web", &original)
        .expect("read");
    assert_eq!(granted.original, ["docs/", "src/pump/"]);
    assert_eq!(granted.additions, ["src/pump/", "docs/", "skills/"]);
    assert_eq!(granted.effective, ["docs/", "src/pump/", "skills/"]);
}

#[test]
fn operator_denial_grants_nothing_and_operator_approval_grants_all() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    let denied = need_target("j1", "56d07dc5", "web", RulingDecision::OperatorDenied);
    store.insert_scope_ruling(&denied).expect("insert");
    let refused = store
        .effective_need_scope("56d07dc5", "web", &["docs/".into()])
        .expect("read");
    assert!(refused.additions.is_empty());
    assert_eq!(refused.effective, ["docs/"]);

    let mut approved = need_target("j2", "56d07dc5", "web", RulingDecision::OperatorApproved);
    approved.paths = vec!["secrets/".into(), "src/lib.rs".into()];
    store.insert_scope_ruling(&approved).expect("insert");
    let granted = store
        .effective_need_scope("56d07dc5", "web", &[])
        .expect("read");
    assert_eq!(granted.additions, ["secrets/", "src/lib.rs"]);
    assert_eq!(granted.effective, ["secrets/", "src/lib.rs"]);
}

#[test]
fn grants_are_keyed_by_need_and_target_exactly() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    store
        .insert_scope_ruling(&need_target(
            "j1",
            "56d07dc5",
            "other",
            RulingDecision::Approved,
        ))
        .expect("insert other target");
    store
        .insert_scope_ruling(&need_target(
            "j2",
            "another",
            "web",
            RulingDecision::Approved,
        ))
        .expect("insert other need");
    let own = store
        .effective_need_scope("56d07dc5", "web", &[])
        .expect("read");
    assert!(own.additions.is_empty());
    assert!(own.effective.is_empty());
    let sibling = store
        .effective_need_scope("56d07dc5", "other", &[])
        .expect("read");
    assert_eq!(sibling.additions, ["src/"]);
    let cousin = store
        .effective_need_scope("another", "web", &[])
        .expect("read");
    assert_eq!(cousin.additions, ["src/"]);
}

#[test]
fn duplicate_additions_are_suppressed_across_rulings_and_original() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    store
        .insert_scope_ruling(&need_target(
            "j1",
            "56d07dc5",
            "web",
            RulingDecision::Approved,
        ))
        .expect("insert");
    let mut second = need_target("j2", "56d07dc5", "web", RulingDecision::Approved);
    second.paths = vec!["src/".into(), "docs/".into()];
    store.insert_scope_ruling(&second).expect("insert");
    let granted = store
        .effective_need_scope("56d07dc5", "web", &["src/".into()])
        .expect("read");
    assert_eq!(granted.additions, ["src/", "docs/"]);
    assert_eq!(granted.effective, ["src/", "docs/"]);
}

#[test]
fn grants_survive_later_runs_of_the_same_need() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    let mut first_run = need_target("j1", "56d07dc5", "web", RulingDecision::Approved);
    first_run.run = 1;
    store.insert_scope_ruling(&first_run).expect("insert");
    // A later run re-reads the same (need, target) key; run is not part of
    // the grant key, so the run-1 grant is still in force.
    let later = store
        .effective_need_scope("56d07dc5", "web", &[])
        .expect("read");
    assert_eq!(later.additions, ["src/"]);
    let mut second_run = need_target("j2", "56d07dc5", "web", RulingDecision::Approved);
    second_run.run = 2;
    second_run.paths = vec!["tests/".into()];
    store.insert_scope_ruling(&second_run).expect("insert");
    let both = store
        .effective_need_scope("56d07dc5", "web", &[])
        .expect("read");
    assert_eq!(both.additions, ["src/", "tests/"]);
}

#[test]
fn unspecified_original_scope_stays_unspecified() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    let bare = store
        .effective_need_scope("56d07dc5", "web", &[])
        .expect("read");
    assert!(bare.original.is_empty());
    assert!(bare.additions.is_empty());
    assert!(bare.effective.is_empty());
    store
        .insert_scope_ruling(&need_target(
            "j1",
            "56d07dc5",
            "web",
            RulingDecision::Approved,
        ))
        .expect("insert");
    let granted = store
        .effective_need_scope("56d07dc5", "web", &[])
        .expect("read");
    assert!(granted.original.is_empty());
    assert_eq!(granted.additions, ["src/"]);
    assert_eq!(granted.effective, ["src/"]);
}

#[test]
fn tightening_policy_leaves_a_historical_grant_intact() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    let first = store
        .insert_scope_ruling(&need_target(
            "j1",
            "56d07dc5",
            "web",
            RulingDecision::Approved,
        ))
        .expect("insert");
    // The same request replayed under a tightened live policy: the stored
    // ruling is returned unchanged, decision and policy snapshot alike.
    let tightened = ScopePolicy::snapshot("web", &["docs/".into()], &["src/".into()])
        .expect("tightened snapshot");
    let mut replay = need_target("j1", "56d07dc5", "web", RulingDecision::Denied);
    replay.policy = tightened.clone();
    let again = store.insert_scope_ruling(&replay).expect("reuse");
    assert_eq!(again, first);
    assert_eq!(again.decision, RulingDecision::Approved);
    assert_eq!(again.policy.allow, ["./", "vendor/"]);
    let granted = store
        .effective_need_scope("56d07dc5", "web", &[])
        .expect("read");
    assert_eq!(granted.additions, ["src/"]);
}

#[test]
fn progress_transitions_survive_state_save_and_reopen() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join(DB_FILENAME);
    let mut store = PumpStore::open(&path).expect("open");
    store
        .insert_scope_ruling(&need_target(
            "j1",
            "56d07dc5",
            "web",
            RulingDecision::Approved,
        ))
        .expect("insert");
    // Identity first: the reply body is persisted before posting.
    store
        .set_ruling_reply("j1", "approved: src/ is yours")
        .expect("set reply");
    store.save(&State::default()).expect("save legacy state");
    assert_eq!(
        store
            .scope_ruling("j1")
            .expect("read")
            .expect("ruling")
            .reply_body,
        Some(String::from("approved: src/ is yours"))
    );
    drop(store);
    let mut store = PumpStore::open(&path).expect("reopen");
    store.mark_ruling_posted("j1", 100).expect("mark posted");
    // Continuation identity before enqueueing, completion after.
    store
        .set_ruling_continuation("j1", &continuation_id("j1"))
        .expect("set continuation");
    drop(store);
    let store = PumpStore::open(&path).expect("reopen again");
    store
        .mark_ruling_continuation_posted("j1", 200)
        .expect("mark continuation");
    let done = store.scope_ruling("j1").expect("read").expect("ruling");
    assert_eq!(
        done.reply_body,
        Some(String::from("approved: src/ is yours"))
    );
    assert_eq!(done.posted_at, Some(100));
    assert_eq!(done.continuation_id, Some(continuation_id("j1")));
    assert_eq!(done.continuation_posted_at, Some(200));
}

#[test]
fn progress_marks_are_idempotent_and_repairable() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    store
        .insert_scope_ruling(&need_target(
            "j1",
            "56d07dc5",
            "web",
            RulingDecision::Approved,
        ))
        .expect("insert");
    store.set_ruling_reply("j1", "first").expect("set reply");
    store.set_ruling_reply("j1", "redraft").expect("rewrite");
    store.mark_ruling_posted("j1", 100).expect("mark posted");
    // A completion flag never blocks repairing a lost posting record.
    store.mark_ruling_posted("j1", 150).expect("re-mark");
    store
        .set_ruling_continuation("j1", "j1-c1")
        .expect("set continuation");
    store
        .mark_ruling_continuation_posted("j1", 200)
        .expect("mark continuation");
    store
        .mark_ruling_continuation_posted("j1", 250)
        .expect("re-mark continuation");
    let done = store.scope_ruling("j1").expect("read").expect("ruling");
    assert_eq!(done.reply_body, Some(String::from("redraft")));
    assert_eq!(done.posted_at, Some(150));
    assert_eq!(done.continuation_id, Some(String::from("j1-c1")));
    assert_eq!(done.continuation_posted_at, Some(250));
}

#[test]
fn progress_updates_without_a_ruling_fail() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    for attempt in [
        |store: &PumpStore| store.set_ruling_reply("missing", "body"),
        |store: &PumpStore| store.mark_ruling_posted("missing", 1),
        |store: &PumpStore| store.set_ruling_continuation("missing", "m-c1"),
        |store: &PumpStore| store.mark_ruling_continuation_posted("missing", 1),
    ] {
        let error = attempt(&store).expect_err("missing ruling");
        assert!(error.to_string().contains("missing"), "{error}");
    }
}
