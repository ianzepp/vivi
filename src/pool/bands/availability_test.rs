//! Tests for pool capacity snapshots: recorded usage, caps, and orphan
//! claims.

use std::collections::BTreeMap;

use super::*;
use crate::pool::record::{DispatchRecord, dispatch_note};
use crate::pool::slot::{
    META_CLAIMED, META_MAILSPACE, META_POOL, META_STATE, META_TASK, STATE_READY,
};

const SPACE: &str = "space";
const POOL: &str = "p";
const OTHER_POOL: &str = "q";
const MODEL: &str = "prov/a";
const NOW: u64 = 5_000_000;

fn pool_id() -> PoolId {
    PoolId::keyed(SPACE, POOL)
}

fn record(pool: &str, machine: &str, model: &str) -> DispatchRecord {
    DispatchRecord {
        target: None,
        repository: None,
        pool: pool.into(),
        need: None,
        kind: None,
        slot: format!("{pool}-slot-1"),
        machine: machine.into(),
        job: "j".into(),
        model: model.into(),
        from: "main".into(),
        branch: "factory/a".into(),
        start: "a".repeat(40),
        with: Vec::new(),
        budget: 600,
        at: 1000,
        read_only: false,
        hardware: String::new(),
        abandoned: 0,
        abandoned_cost_micros: 0,
    }
}

fn dispatched(task: &str, record: &DispatchRecord) -> PoolEventRow {
    PoolEventRow {
        handle: task.into(),
        event_type: "pool_dispatched".into(),
        note: dispatch_note(record),
    }
}

fn collected(task: &str) -> PoolEventRow {
    PoolEventRow {
        handle: task.into(),
        event_type: "pool_collected".into(),
        note: "{}".into(),
    }
}

fn machine(id: &str, mailspace: &str, pool: &str) -> MachineRecord {
    MachineRecord {
        id: id.into(),
        name: format!("{mailspace}-{pool}-slot-1"),
        state: "started".into(),
        region: "iad".into(),
        created_at: "then".into(),
        metadata: BTreeMap::from([
            (META_MAILSPACE.into(), mailspace.into()),
            (META_POOL.into(), pool.into()),
            (META_STATE.into(), STATE_READY.into()),
        ]),
        exit_code: None,
    }
}

fn claimed(machine: MachineRecord, task: &str) -> MachineRecord {
    let mut claimed = machine;
    claimed.metadata.insert(META_TASK.into(), task.into());
    claimed
}

/// A capped model is excluded at its cap and admitted below it, a
/// finished-but-uncollected job keeps occupying the cap, and collecting
/// the dispatch releases the usage again.
#[test]
fn capped_model_is_excluded() {
    let id = pool_id();
    let finished = {
        let mut m = claimed(machine("m1", SPACE, POOL), "aaaa1111");
        m.state = "stopped".into();
        m
    };

    let events = vec![dispatched("aaaa1111", &record(POOL, "m1", MODEL))];
    let snapshot = Availability::at(NOW, &events, &id, &[finished.clone()]);
    assert_eq!(snapshot.usage(MODEL), 1);
    assert!(!snapshot.admits(MODEL, Some(1)));
    assert!(snapshot.admits(MODEL, Some(2)));
    assert!(snapshot.admits(MODEL, None));

    // The job finished on the slot but was never collected: the open
    // dispatch still holds the cap, and its matching claim is not an
    // orphan on top of it.
    assert_eq!(snapshot.orphan_claims.len(), 0);
    assert!(!snapshot.admits(MODEL, Some(1)));

    let mut events = events;
    events.push(collected("aaaa1111"));
    // The collector releases the slot with the collected event: usage
    // falls to zero and the cap admits again.
    let mut released = finished;
    released.metadata.remove(META_TASK);
    let snapshot = Availability::at(NOW, &events, &id, &[released]);
    assert_eq!(snapshot.usage(MODEL), 0);
    assert!(snapshot.admits(MODEL, Some(1)));
}

/// Dispatch records carry the model slug alone, so every reasoning
/// variant and band of one model aggregates onto the model's one cap.
#[test]
fn reasoning_variants_share_one_cap() {
    let id = pool_id();
    let mut first = record(POOL, "m1", MODEL);
    first.kind = Some("hand".into());
    let mut second = record(POOL, "m2", MODEL);
    second.kind = Some("mind".into());
    let events = vec![
        dispatched("aaaa1111", &first),
        dispatched("bbbb2222", &second),
    ];
    let snapshot = Availability::at(NOW, &events, &id, &[]);
    assert_eq!(snapshot.usage(MODEL), 2);
    assert!(!snapshot.admits(MODEL, Some(2)));
    assert!(snapshot.admits(MODEL, Some(3)));
}

/// Target and repository choices share one pool cap, and a dispatch of
/// another pool is never charged to this pool's cap.
#[test]
fn targets_share_pool_caps() {
    let id = pool_id();
    let mut first = record(POOL, "m1", MODEL);
    first.target = Some("t1".into());
    first.repository = Some("o/r1".into());
    let second = record(POOL, "m2", MODEL);
    let events = vec![
        dispatched("aaaa1111", &first),
        dispatched("bbbb2222", &second),
    ];
    let snapshot = Availability::at(NOW, &events, &id, &[]);
    assert_eq!(snapshot.usage(MODEL), 2);
    assert!(!snapshot.admits(MODEL, Some(2)));

    let foreign = vec![dispatched("cccc3333", &record(OTHER_POOL, "m9", MODEL))];
    let snapshot = Availability::at(NOW, &foreign, &id, &[]);
    assert_eq!(snapshot.usage(MODEL), 0);
    assert!(snapshot.admits(MODEL, Some(1)));
}

/// A zero cap always refuses the model, even with nothing recorded
/// against it.
#[test]
fn zero_cap_disables_model() {
    let id = pool_id();
    let snapshot = Availability::at(NOW, &[], &id, &[]);
    assert_eq!(snapshot.usage(MODEL), 0);
    assert!(!snapshot.admits(MODEL, Some(0)));
    assert!(snapshot.admits(MODEL, None));
}

/// A claimed slot with no open dispatch behind it blocks every capped
/// model of its pool however old the claim is, never a foreign pool's or
/// mailspace's, and never twice for a claim a record already explains.
#[test]
fn unrecorded_claim_blocks_capped_admission() {
    let id = pool_id();
    let snapshot = Availability::at(
        NOW,
        &[],
        &id,
        &[claimed(machine("m1", SPACE, POOL), "aaaa1111")],
    );
    assert_eq!(snapshot.orphan_claims, vec!["m1".to_string()]);
    assert!(!snapshot.admits(MODEL, Some(1)));
    // Every capped model of the pool is charged.
    assert!(!snapshot.admits("other/b", Some(1)));
    // Uncapped models are untouched.
    assert!(snapshot.admits(MODEL, None));

    // Foreign pools and mailspaces are never charged.
    let snapshot = Availability::at(
        NOW,
        &[],
        &id,
        &[
            claimed(machine("m2", SPACE, OTHER_POOL), "bbbb2222"),
            claimed(machine("m3", "elsewhere", POOL), "cccc3333"),
        ],
    );
    assert!(snapshot.orphan_claims.is_empty());
    assert!(snapshot.admits(MODEL, Some(1)));

    // Age never expires an orphan reservation; reconcile owns cleanup.
    let old = {
        let mut m = claimed(machine("m4", SPACE, POOL), "dddd4444");
        m.metadata
            .insert(META_CLAIMED.into(), (NOW - 100_000).to_string());
        m
    };
    let snapshot = Availability::at(NOW, &[], &id, &[old]);
    assert_eq!(snapshot.orphan_claims.len(), 1);
    assert!(!snapshot.admits(MODEL, Some(1)));

    // A claim an open dispatch already explains is counted once, through
    // its record.
    let events = vec![dispatched("eeee5555", &record(POOL, "m5", MODEL))];
    let snapshot = Availability::at(
        NOW,
        &events,
        &id,
        &[claimed(machine("m5", SPACE, POOL), "eeee5555")],
    );
    assert!(snapshot.orphan_claims.is_empty());
    assert_eq!(snapshot.usage(MODEL), 1);
    assert!(!snapshot.admits(MODEL, Some(1)));
    assert!(snapshot.admits(MODEL, Some(2)));
}
