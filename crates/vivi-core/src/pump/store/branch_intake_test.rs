//! Tests for the branch-intake tables and their independence from State.

use rusqlite::Connection;
use tempfile::tempdir;

use crate::pump::branch_intake::model::{
    BranchObservation, BranchState, GenerationRecord, GenerationStage, IntakeIntent,
    IntakeIntentKind, NotificationState, ProofRecord, PublicationState, TipRecord,
};
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore, SCHEMA_VERSION};

const TARGET: &str = "vivi";
const SOURCE: &str = "refs/heads/vivi/unit/example";

fn generation(n: u64, stage: GenerationStage) -> GenerationRecord {
    GenerationRecord {
        target: TARGET.into(),
        source_ref: SOURCE.into(),
        generation: n,
        observed_main: format!("main-{n}"),
        candidate_sha: format!("candidate-{n}"),
        candidate_ref: format!("refs/heads/candidate/{n}"),
        construction_author: "author".into(),
        construction_committer: "committer".into(),
        construction_message: format!("construct {n}"),
        constructed_at: 10 + n,
        stage,
        reasons: vec![format!("reason-{n}")],
        created_at: 20 + n,
        updated_at: 30 + n,
    }
}

fn proof() -> ProofRecord {
    ProofRecord {
        purpose: "landing".into(),
        target: TARGET.into(),
        source_ref: SOURCE.into(),
        generation: 2,
        tip: "candidate-2".into(),
        rung: "build".into(),
        command: "cargo test --lib".into(),
        fingerprint: "fingerprint".into(),
        job: "job-1".into(),
        task: "task-1".into(),
        inputs: "{\"clean\":true}".into(),
        results: "{\"passed\":1}".into(),
        evidence: vec!["evidence-1".into()],
        lines: vec!["original line".into()],
        created_at: 50,
    }
}

fn sample() -> BranchState {
    let mut state = BranchState::new(TARGET, SOURCE);
    state.latest_observation = Some(BranchObservation {
        source_sha: "candidate-2".into(),
        observed_main: "main-2".into(),
        observed_at: 60,
    });
    state.active_generation = Some(2);
    state.notification = NotificationState {
        needed: true,
        delivered_at: None,
    };
    state.publication = PublicationState {
        needed: true,
        published_at: None,
    };
    state.record_tip(TipRecord {
        target: TARGET.into(),
        source_ref: SOURCE.into(),
        source_sha: "candidate-1".into(),
        observed_main: "main-1".into(),
        observed_at: 11,
    });
    state.record_tip(TipRecord {
        target: TARGET.into(),
        source_ref: SOURCE.into(),
        source_sha: "candidate-2".into(),
        observed_main: "main-2".into(),
        observed_at: 12,
    });
    state.record_generation(generation(1, GenerationStage::Superseded));
    state.record_generation(generation(2, GenerationStage::Landed));
    state.proofs.push(proof());
    state.intents.push(IntakeIntent {
        kind: IntakeIntentKind::Notification,
        target: TARGET.into(),
        source_ref: SOURCE.into(),
        generation: Some(2),
        candidate_ref: Some("refs/heads/candidate/2".into()),
        candidate_sha: Some("candidate-2".into()),
        body: "landed".into(),
        created_at: 70,
        ..IntakeIntent::default()
    });
    state.updated_at = 80;
    state
}

#[test]
fn intake_history_survives_state_save_and_restart() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join(DB_FILENAME);
    let store = PumpStore::open(&path).expect("open");
    store.save_branch_state(&sample()).expect("save intake");
    store.save(&State::default()).expect("save state");

    let before = store
        .branch_state(TARGET, SOURCE)
        .expect("read intake")
        .expect("state");
    assert_eq!(before.generations.len(), 2);
    assert_eq!(before.generations[0].stage, GenerationStage::Superseded);
    assert_eq!(before.generations[0].candidate_sha, "candidate-1");
    assert_eq!(before.generations[1].stage, GenerationStage::Landed);
    assert_eq!(before.tips.len(), 2);
    assert_eq!(before.proofs.len(), 1);
    assert_eq!(before.intents.len(), 1);
    drop(store);

    let reopened = PumpStore::open(&path).expect("reopen");
    let after = reopened
        .branch_state(TARGET, SOURCE)
        .expect("read after restart")
        .expect("state after restart");
    assert_eq!(after, before);
    assert_eq!(
        reopened
            .branch_generations(TARGET, SOURCE)
            .expect("full history")
            .len(),
        2
    );
}

#[test]
fn intake_missing_tables_read_empty() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join(DB_FILENAME);
    let conn = Connection::open(&path).expect("create old database");
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)
        .expect("version");
    drop(conn);

    let store = PumpStore::open_read_only(&path)
        .expect("open read-only")
        .expect("file exists");
    assert!(store.branch_state(TARGET, SOURCE).expect("state").is_none());
    assert!(
        store
            .branch_generations(TARGET, SOURCE)
            .expect("generations")
            .is_empty()
    );
    assert!(store.branch_tips(TARGET, SOURCE).expect("tips").is_empty());
    assert!(
        store
            .branch_proofs(TARGET, SOURCE)
            .expect("proofs")
            .is_empty()
    );
    assert!(
        store
            .branch_intents(TARGET, SOURCE)
            .expect("intents")
            .is_empty()
    );
}

#[test]
fn intake_corrupt_history_is_not_empty() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    store
        .record_branch_generation(&generation(1, GenerationStage::Landed))
        .expect("generation");
    store
        .conn()
        .execute(
            "UPDATE branch_intake_generations SET stage = 'not-a-stage'",
            [],
        )
        .expect("corrupt stage");

    let error = store
        .branch_generations(TARGET, SOURCE)
        .expect_err("corrupt history must be an error");
    assert!(
        error.to_string().contains("generation stage"),
        "unexpected error: {error}"
    );
}
