//! Tests for the promotion rule ("main never gets redder": a gate promotes
//! when its failing set is within the set main carries), the persisted set,
//! and the gate forced by a merged fix.

use std::sync::Arc;

use tempfile::tempdir;

use crate::pump::config::{GateConfig, PumpConfig};
use crate::pump::gate::{finish_gate, start_gate};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::{GateRecord, GateRun, Promotion, State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// The fake clock's start.
const T0: u64 = 1_000_000;
/// A 40-hex remote tip.
const TIP: &str = "0123456789abcdef0123456789abcdef01234567";
/// An earlier promoted tip.
const OLD: &str = "fedcba9876543210fedcba9876543210fedcba98";
/// The gate task handle the fake `task send` reports.
const TASK: &str = "1a2b3c4d";

fn setup() -> (tempfile::TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        branch: String::from("next"),
        gate: GateConfig {
            command: String::from("stage 4b"),
            every_merges: 8,
            every_seconds: 1800,
            pools: vec![String::from("flygate")],
            model: String::from("m"),
            budget: 1800,
            ..GateConfig::default()
        },
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
    host.reply("git fetch", 0, "");
    host.reply("git merge-base", 0, "");
    host.reply("git push", 0, "To origin\n   abc..def  main -> main\n");
    (dir, ctx, host)
}

/// A state with an open gate task at `TIP` and the given baseline.
fn state(baseline: &[&str]) -> State {
    State {
        merges: 4,
        baseline: Some(names(baseline)),
        last_gate: GateRun {
            at: T0,
            merges: 4,
            task: Some(String::from(TASK)),
            tip: Some(String::from(TIP)),
            pool: Some(String::from("flygate")),
        },
        ..State::default()
    }
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| String::from(*s)).collect()
}

fn reply(list: &[&str]) -> String {
    format!("FAILING-BEGIN\n{}\nFAILING-END\n", list.join("\n"))
}

fn pushed(host: &Arc<FakeHost>) -> bool {
    host.commands().iter().any(|c| c.contains("git push"))
}

fn promote_held(host: &Arc<FakeHost>) -> Option<String> {
    host.logs().into_iter().find(|l| l.contains("promote held"))
}

fn own(st: &mut State, name: &str) {
    st.red
        .insert(String::from(name), String::from("pumpfix-gx"));
}

#[test]
fn promotes_when_failing_is_within_main_even_with_owned_reds_outstanding() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a"]);
    st.promotions.push(Promotion {
        tip: String::from(OLD),
        at: T0 - 100,
        failing: Some(names(&["a", "c", "d"])),
    });
    own(&mut st, "c");
    own(&mut st, "d");
    finish_gate(&ctx, &mut st, &reply(&["a", "c"]));
    assert!(pushed(&host));
    assert!(promote_held(&host).is_none());
    let last = st.promotions.last().unwrap();
    assert_eq!(last.tip, TIP);
    assert_eq!(last.failing, Some(names(&["a", "c"])));
    // d passed again and is released; c is still owned.
    assert!(!st.red.contains_key("d"));
    assert!(st.red.contains_key("c"));
    drop(dir);
}

#[test]
fn is_held_when_a_name_is_not_on_main_and_lists_up_to_five() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a"]);
    let extra = ["x1", "x2", "x3", "x4", "x5", "x6", "x7"];
    for name in extra {
        own(&mut st, name);
    }
    let mut failing = vec!["a"];
    failing.extend(extra);
    finish_gate(&ctx, &mut st, &reply(&failing));
    assert!(!pushed(&host));
    assert!(st.promotions.is_empty());
    assert_eq!(
        promote_held(&host).unwrap(),
        "promote held: 7 failures not on main at 0123456789 (main never gets redder): \
         x1; x2; x3; x4; x5 and 2 more"
    );
    drop(dir);
}

#[test]
fn main_set_follows_the_last_promotion() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a", "b"]);
    finish_gate(&ctx, &mut st, &reply(&["a"]));
    assert_eq!(st.promotions.len(), 1);
    assert_eq!(st.promotions[0].failing, Some(names(&["a"])));
    // b was on main by baseline, but main now carries only a.
    own(&mut st, "b");
    st.last_gate.task = Some(String::from(TASK));
    finish_gate(&ctx, &mut st, &reply(&["a", "b"]));
    assert_eq!(st.promotions.len(), 1);
    assert!(promote_held(&host).unwrap().contains(": b"));
    drop(dir);
}

#[test]
fn the_baseline_is_main_before_the_first_promotion() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a", "b"]);
    finish_gate(&ctx, &mut st, &reply(&["b"]));
    assert!(pushed(&host));
    assert_eq!(st.promotions[0].failing, Some(names(&["b"])));
    drop(dir);
}

#[test]
fn a_promotion_without_a_recorded_set_uses_its_gate_record() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a"]);
    st.promotions.push(Promotion {
        tip: String::from(OLD),
        at: T0 - 100,
        failing: None,
    });
    st.gates.push(GateRecord {
        tip: String::from(OLD),
        at: T0 - 110,
        merges: 2,
        names: Some(names(&["a", "g"])),
        new: None,
    });
    own(&mut st, "g");
    finish_gate(&ctx, &mut st, &reply(&["g"]));
    assert!(pushed(&host));
    drop(dir);
}

#[test]
fn the_recorded_set_round_trips_through_the_store() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let mut st = State::default();
    st.promotions.push(Promotion {
        tip: String::from(OLD),
        at: 5,
        failing: Some(names(&["a", "b"])),
    });
    st.promotions.push(Promotion {
        tip: String::from(TIP),
        at: 9,
        failing: None,
    });
    let mut store = PumpStore::open(&path).unwrap();
    store.save(&st).unwrap();
    let back = PumpStore::open(&path).unwrap().load().unwrap();
    assert_eq!(back.promotions[0].failing, Some(names(&["a", "b"])));
    assert_eq!(back.promotions[1].failing, None);
}

#[test]
fn a_database_made_before_the_set_was_recorded_gains_the_column() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    drop(PumpStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "DROP TABLE promotions;
         CREATE TABLE promotions (tip TEXT PRIMARY KEY, at INTEGER NOT NULL);
         INSERT INTO promotions (tip, at) VALUES ('t1', 3);",
    )
    .unwrap();
    drop(conn);
    let mut store = PumpStore::open(&path).unwrap();
    let mut st = store.load().unwrap();
    assert_eq!(st.promotions.len(), 1);
    assert_eq!(st.promotions[0].failing, None);
    st.promotions[0].failing = Some(names(&["z"]));
    store.save(&st).unwrap();
    let back = PumpStore::open(&path).unwrap().load().unwrap();
    assert_eq!(back.promotions[0].failing, Some(names(&["z"])));
}

fn wire_gate(host: &Arc<FakeHost>) {
    host.reply("pool status", 0, "slot flygate bridge slot-9 free\n");
    host.reply("git ls-remote", 0, &format!("{TIP}\trefs/heads/next\n"));
    host.reply("task send", 0, &format!("created hand {TASK}\nnode {TASK}"));
    host.reply(
        "pool dispatch",
        0,
        &format!("dispatched {TASK} bridge slot-9"),
    );
}

fn merged_fix(st: &mut State, uid: &str, at: u64) {
    st.units.insert(
        String::from(uid),
        Unit {
            status: Status::Merged,
            merged_at: Some(at),
            ..Unit::default()
        },
    );
}

fn quiet_state() -> State {
    let mut st = state(&["a"]);
    st.last_gate.task = None;
    st
}

fn forced_logs(host: &Arc<FakeHost>) -> usize {
    host.logs()
        .iter()
        .filter(|l| l.contains("gate forced: fix pumpfix-ga-1 merged"))
        .count()
}

#[test]
fn a_merged_fix_forces_one_gate_even_when_the_gate_is_not_due() {
    let (dir, ctx, host) = setup();
    let mut st = quiet_state();
    merged_fix(&mut st, "pumpfix-ga-1", T0 + 10);
    host.advance(20);
    // No pool is free yet: the flag is written once and kept.
    start_gate(&ctx, &mut st);
    start_gate(&ctx, &mut st);
    assert!(ctx.flag("FORCE-GATE"));
    assert_eq!(forced_logs(&host), 1);
    wire_gate(&host);
    start_gate(&ctx, &mut st);
    assert!(!ctx.flag("FORCE-GATE"));
    assert_eq!(st.last_gate.task.as_deref(), Some(TASK));
    // The gate started after the merge: the same fix forces nothing more.
    st.last_gate.task = None;
    start_gate(&ctx, &mut st);
    assert!(!ctx.flag("FORCE-GATE"));
    assert_eq!(forced_logs(&host), 1);
    drop(dir);
}

#[test]
fn a_fix_merged_before_the_last_gate_or_a_non_fix_unit_forces_nothing() {
    let (dir, ctx, host) = setup();
    let mut st = quiet_state();
    merged_fix(&mut st, "pumpfix-ga-1", T0 - 5);
    merged_fix(&mut st, "plain-1", T0 + 10);
    host.advance(20);
    start_gate(&ctx, &mut st);
    assert!(!ctx.flag("FORCE-GATE"));
    assert!(host.logs().iter().all(|l| !l.contains("gate forced")));
    drop(dir);
}
