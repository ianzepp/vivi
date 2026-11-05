use crate::pump::branch_intake::state::{
    BranchIdentity, BranchIntakeState, Evidence, GateSettings, Observation, TipKey, TipRecord,
    VERSION,
};
use crate::pump::state::State;
use crate::pump::store::PumpStore;
use std::collections::BTreeMap;
use tempfile::tempdir;

fn record(sha: &str) -> TipRecord {
    let key = TipKey {
        branch: BranchIdentity {
            repository: "repo".into(),
            branch: "main".into(),
        },
        sha: sha.into(),
    };
    TipRecord {
        key,
        latest: Observation {
            sha: sha.into(),
            observed_at: 10,
            blocker: None,
        },
        pinned_base: None,
        effective_gates: GateSettings {
            checks: vec!["cargo test".into()],
            policy: BTreeMap::new(),
        },
        evidence: Evidence {
            job_ids: vec![],
            details: BTreeMap::new(),
        },
        lifecycle: "observed".into(),
        outcome: None,
        successor: None,
        created_at: 10,
        updated_at: 10,
        receipts: vec![],
    }
}

#[test]
fn branch_intake_round_trips_through_pump_store() {
    let dir = tempdir().unwrap();
    let store = PumpStore::open(&dir.path().join("pump.db")).unwrap();
    let mut state = State::default();
    let mut intake = BranchIntakeState::default();
    intake.remember(record(&"a".repeat(40)));
    intake.write_extra(&mut state.extra).unwrap();
    store.save(&state).unwrap();
    let decoded = BranchIntakeState::from_extra(&store.load().unwrap().extra).unwrap();
    assert_eq!(decoded, intake);
}

#[test]
fn branch_intake_full_sha_keys_do_not_collide() {
    let mut intake = BranchIntakeState::default();
    intake.remember(record(&format!("{}a", "b".repeat(39))));
    intake.remember(record(&format!("{}c", "b".repeat(39))));
    assert_eq!(intake.records.len(), 2);
}

#[test]
fn intake_corrupt_state_holds_decoding() {
    let mut extra = BTreeMap::new();
    extra.insert(
        "branch_intake".into(),
        serde_json::json!({"version": VERSION, "records": "bad"}),
    );
    assert!(BranchIntakeState::from_extra(&extra).is_err());
    extra.insert(
        "branch_intake".into(),
        serde_json::json!({"version": VERSION + 1, "records": {}, "handled": []}),
    );
    assert!(BranchIntakeState::from_extra(&extra).is_err());
}
