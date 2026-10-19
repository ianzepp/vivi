//! Tests for [`super::blockers`]: the root of a stalled need and the units
//! that depend on it, directly or through the graph.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::pump::blockers::{check, compute};
use crate::pump::config::{PlannerConfig, PumpConfig};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::priority;
use crate::pump::state::{Attempt, NeedRecord, State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// The fake clock's start.
const T0: u64 = 1_000_000;

/// A context with the default pump configuration.
fn setup() -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        branch: String::from("next"),
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
    (dir, ctx, host)
}

/// A context whose planner command marks its purpose in the spawned line.
fn setup_planner() -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        branch: String::from("next"),
        planner: PlannerConfig {
            run: String::from("resplit-planner {charter}"),
            charter: String::from("/charter.md"),
            split_timeout: 1500,
        },
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
    (dir, ctx, host)
}

/// A unit of `need` in `status`, created `since` epoch seconds.
fn unit(need: &str, status: Status, since: u64) -> Unit {
    Unit {
        need: need.to_string(),
        status,
        since,
        ..Unit::default()
    }
}

/// A need record that minted `units`.
fn record(verdict: &str, units: &[&str]) -> NeedRecord {
    NeedRecord {
        verdict: verdict.to_string(),
        units: units.iter().map(|uid| (*uid).to_string()).collect(),
        ..NeedRecord::default()
    }
}

/// One `blocked_by` row for the `graph blocked --json` answer.
fn row(source: &str, dependencies: &[&str]) -> (String, Vec<String>) {
    (
        source.to_string(),
        dependencies.iter().map(|dep| (*dep).to_string()).collect(),
    )
}

/// The `graph blocked --json` answer for `(source, prerequisites)` rows.
fn rows(pairs: &[(String, Vec<String>)]) -> String {
    let rows: Vec<serde_json::Value> = pairs
        .iter()
        .map(|(source, dependencies)| {
            serde_json::json!({"source_id": source, "blocked_by": dependencies})
        })
        .collect();
    serde_json::Value::Array(rows).to_string()
}

/// A state whose need `r` has one stuck unit `since` and `count` dependent
/// needs, each holding one pending unit.
fn blocking_state(host: &FakeHost, since: u64, count: usize) -> State {
    let mut st = State::default();
    st.needs
        .insert(String::from("r"), record("units", &["r-1"]));
    st.units
        .insert(String::from("r-1"), unit("r", Status::Stuck, since));
    let mut pairs: Vec<(String, Vec<String>)> = Vec::new();
    for index in 1..=count {
        let need = format!("m{index}");
        st.needs.insert(need.clone(), record("units", &[]));
        st.units
            .insert(format!("{need}-1"), unit(&need, Status::Pending, T0 - 600));
        pairs.push(row(&need, &["r"]));
    }
    host.reply("graph blocked", 0, &rows(&pairs));
    st
}

/// Give the state's `r-1` unit the attempt history `why`/`detail`.
fn with_attempt(st: &mut State, why: &str, detail: &str) {
    let unit = st.units.get_mut("r-1").unwrap();
    unit.history.push(Attempt {
        attempt: 1,
        why: why.to_string(),
        detail: detail.to_string(),
        at: T0 - 60,
        ..Attempt::default()
    });
}

#[test]
fn a_stuck_root_transitively_holds_dependent_units() {
    let (_dir, ctx, host) = setup();
    host.set_now(T0);
    let mut st = State::default();
    st.needs
        .insert(String::from("r"), record("units", &["r-1"]));
    st.units
        .insert(String::from("r-1"), unit("r", Status::Stuck, T0 - 3600));
    let mut pairs: Vec<(String, Vec<String>)> = Vec::new();
    for index in 1..=20 {
        let need = format!("m{index}");
        st.needs.insert(need.clone(), record("units", &[]));
        st.units
            .insert(format!("{need}-1"), unit(&need, Status::Pending, T0 - 600));
        pairs.push(row(&need, &["r"]));
    }
    host.reply("graph blocked", 0, &rows(&pairs));

    let out = compute(&ctx, &st);

    assert_eq!(out.len(), 1, "one stuck root, no pending roots: {out:?}");
    let blocker = &out[0];
    assert_eq!(blocker.state, "stuck");
    assert_eq!(blocker.root, "r-1");
    assert_eq!(blocker.need, "r");
    assert_eq!(blocker.held, 21);
    assert_eq!(blocker.age_secs, 3600);
    assert_eq!(blocker.top_needs.len(), 3);
}

#[test]
fn a_parked_operator_need_is_a_blocker_for_its_dependents() {
    let (_dir, ctx, host) = setup();
    host.set_now(T0);
    let mut st = State::default();
    st.needs.insert(String::from("p"), record("operator", &[]));
    st.needs
        .insert(String::from("q"), record("units", &["q-1", "q-2"]));
    st.units
        .insert(String::from("q-1"), unit("q", Status::Pending, T0 - 100));
    st.units
        .insert(String::from("q-2"), unit("q", Status::Pending, T0 - 50));
    host.reply("graph blocked", 0, &rows(&[row("q", &["p"])]));

    let out = compute(&ctx, &st);

    assert_eq!(out.len(), 1, "only the parked root blocks: {out:?}");
    let blocker = &out[0];
    assert_eq!(blocker.state, "operator");
    assert_eq!(blocker.root, "p");
    assert_eq!(blocker.need, "p");
    assert_eq!(blocker.held, 2);
    assert_eq!(blocker.age_secs, 100);
    assert_eq!(blocker.top_needs, vec![(String::from("q"), 2)]);
}

#[test]
fn a_pending_starved_need_surfaces_as_pending_blocker() {
    let (_dir, ctx, host) = setup();
    host.set_now(T0);
    let mut st = State::default();
    st.needs
        .insert(String::from("s"), record("units", &["s-1", "s-2", "s-3"]));
    for index in 1..=3 {
        st.units
            .insert(format!("s-{index}"), unit("s", Status::Pending, T0 - 100));
    }
    st.needs
        .insert(String::from("t"), record("units", &["t-1"]));
    st.units
        .insert(String::from("t-1"), unit("t", Status::Pending, T0 - 50));
    host.reply("graph blocked", 0, &rows(&[row("t", &["s"])]));

    let out = compute(&ctx, &st);

    assert_eq!(out.len(), 1, "only the starved dependency blocks: {out:?}");
    let blocker = &out[0];
    assert_eq!(blocker.state, "pending");
    assert_eq!(blocker.root, "s");
    assert_eq!(blocker.need, "s");
    assert_eq!(blocker.held, 4);
}

#[test]
fn an_unreadable_graph_holds_nothing() {
    let (_dir, ctx, host) = setup();
    host.set_now(T0);
    let mut st = State::default();
    st.needs
        .insert(String::from("r"), record("units", &["r-1"]));
    st.units
        .insert(String::from("r-1"), unit("r", Status::Stuck, T0 - 3600));
    for index in 1..=3 {
        let need = format!("d{index}");
        st.needs.insert(need.clone(), record("units", &[]));
        st.units
            .insert(format!("{need}-1"), unit(&need, Status::Pending, T0 - 60));
    }
    host.reply("graph blocked", 1, "error");

    let out = compute(&ctx, &st);

    assert_eq!(out.len(), 1, "the stuck need still blocks: {out:?}");
    assert_eq!(out[0].held, 1, "no closure without a readable graph");
    assert_eq!(out[0].age_secs, 3600);

    let mut alone = State::default();
    alone
        .units
        .insert(String::from("r-1"), unit("r", Status::Stuck, T0 - 3600));
    let one = compute(&ctx, &alone);
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].held, 1);
}

#[test]
fn merged_and_withdrawn_dependent_units_are_not_counted() {
    let (_dir, ctx, host) = setup();
    host.set_now(T0);
    let mut st = State::default();
    st.needs
        .insert(String::from("r"), record("units", &["r-1"]));
    st.units
        .insert(String::from("r-1"), unit("r", Status::Stuck, T0 - 3600));
    for (need, status) in [
        ("m1", Status::Pending),
        ("m2", Status::Merged),
        ("m3", Status::Withdrawn),
        ("m4", Status::Held),
    ] {
        st.needs.insert(String::from(need), record("units", &[]));
        st.units
            .insert(format!("{need}-1"), unit(need, status, T0 - 600));
    }
    host.reply(
        "graph blocked",
        0,
        &rows(&[
            row("m1", &["r"]),
            row("m2", &["r"]),
            row("m3", &["r"]),
            row("m4", &["r"]),
        ]),
    );

    let out = compute(&ctx, &st);

    assert_eq!(out.len(), 1, "one blocker: {out:?}");
    assert_eq!(out[0].held, 2, "only r-1 and m1-1 are genuinely blocked");
    assert_eq!(
        out[0].top_needs,
        vec![(String::from("m1"), 1)],
        "merged, withdrawn and held needs hold nothing"
    );
}

#[test]
fn a_transient_blocking_root_is_requeued_once_and_prioritized() {
    let (_dir, ctx, host) = setup();
    host.set_now(T0);
    let mut st = blocking_state(&host, T0 - 3600, 12);
    with_attempt(&mut st, "timeout", "budget exceeded");

    check(&ctx, &mut st);

    let unit = st.units.get("r-1").unwrap();
    assert_eq!(unit.status, Status::Pending, "the root is requeued");
    assert!(unit.requeued, "the requeue is recorded");
    assert_eq!(
        priority::list(&ctx.home).unwrap(),
        vec![(String::from("r"), 1)],
        "the blocking need is raised to priority 1"
    );
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("blockers: requeued r-1 once")),
        "no requeue log line in {:?}",
        host.logs()
    );
    let mut store = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    store.save(&st).unwrap();
    drop(store);
    let back = PumpStore::open(&ctx.home.join(DB_FILENAME))
        .unwrap()
        .load()
        .unwrap();
    assert!(
        back.units["r-1"].requeued,
        "the requeue flag must survive a restart"
    );

    let unit = st.units.get_mut("r-1").unwrap();
    unit.status = Status::Stuck;
    check(&ctx, &mut st);

    assert_eq!(
        st.units.get("r-1").unwrap().status,
        Status::Stuck,
        "a second stall is not requeued again"
    );
    let requeues = host
        .logs()
        .iter()
        .filter(|line| line.contains("requeued r-1"))
        .count();
    assert_eq!(requeues, 1, "exactly one requeue: {:?}", host.logs());
}

#[test]
fn a_persistent_old_blocking_root_goes_to_the_resplit_planner() {
    let (_dir, ctx, host) = setup_planner();
    host.set_now(T0);
    let mut st = blocking_state(&host, T0 - 121 * 60, 12);
    with_attempt(&mut st, "proof-failed", "cargo test failed (exit 1) quickly");

    check(&ctx, &mut st);

    assert_eq!(
        st.units.get("r-1").unwrap().status,
        Status::Splitting,
        "the old root is handed to the split planner"
    );
    assert!(
        ctx.home.join("resplit/r-1.json").is_file(),
        "no resplit record written"
    );
    assert!(
        host.spawned().iter().any(|line| line.contains("resplit")),
        "no resplit planner spawned in {:?}",
        host.spawned()
    );
    assert_eq!(
        priority::list(&ctx.home).unwrap(),
        vec![(String::from("r"), 1)],
        "the blocking need is raised to priority 1"
    );
}

#[test]
fn a_young_persistent_root_waits() {
    let (_dir, ctx, host) = setup_planner();
    host.set_now(T0);
    let mut st = blocking_state(&host, T0 - 10 * 60, 12);
    with_attempt(&mut st, "proof-failed", "cargo test failed (exit 1) quickly");

    check(&ctx, &mut st);

    assert_eq!(
        st.units.get("r-1").unwrap().status,
        Status::Stuck,
        "a young root is left alone"
    );
    assert_eq!(
        priority::list(&ctx.home).unwrap(),
        vec![(String::from("r"), 1)],
        "the blocking need is still raised to priority 1"
    );
    assert!(
        host.spawned().is_empty(),
        "nothing is spawned yet: {:?}",
        host.spawned()
    );
    assert!(!ctx.home.join("resplit/r-1.json").exists());
}

#[test]
fn a_small_hold_gets_no_response() {
    let (_dir, ctx, host) = setup_planner();
    host.set_now(T0);
    let mut st = blocking_state(&host, T0 - 121 * 60, 5);
    with_attempt(&mut st, "timeout", "budget exceeded");

    check(&ctx, &mut st);

    assert!(
        priority::list(&ctx.home).unwrap().is_empty(),
        "a small hold is not prioritized"
    );
    let unit = st.units.get("r-1").unwrap();
    assert_eq!(unit.status, Status::Stuck);
    assert!(!unit.requeued, "a small hold is not requeued");
    assert!(
        host.spawned().is_empty(),
        "nothing is spawned: {:?}",
        host.spawned()
    );
}
