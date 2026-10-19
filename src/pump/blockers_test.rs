//! Tests for [`super::blockers`]: the root of a stalled need and the units
//! that depend on it, directly or through the graph.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::pump::blockers::{check, compute};
use crate::pump::config::{PlannerConfig, PumpConfig};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::{Attempt, NeedRecord, State, Status, Unit};

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

/// A context whose planner command names the resplit spawn.
fn response_setup() -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        branch: String::from("next"),
        planner: PlannerConfig {
            run: String::from("plan-resplit {charter} --quiet"),
            planners_max: 2,
            ..PlannerConfig::default()
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

/// A state with one stuck `r-1` root plus `dependents` dependent units, and
/// the graph rows that make them depend on `r`.
fn blocking_state(root: Unit, dependents: usize) -> (State, Vec<(String, Vec<String>)>) {
    let mut st = State::default();
    st.needs
        .insert(String::from("r"), record("units", &["r-1"]));
    st.units.insert(String::from("r-1"), root);
    let mut pairs: Vec<(String, Vec<String>)> = Vec::new();
    for index in 1..=dependents {
        let need = format!("m{index}");
        st.needs.insert(need.clone(), record("units", &[]));
        st.units
            .insert(format!("{need}-1"), unit(&need, Status::Pending, T0 - 600));
        pairs.push(row(&need, &["r"]));
    }
    (st, pairs)
}

/// The root's last attempt.
fn last_attempt(why: &str, detail: &str) -> Attempt {
    Attempt {
        why: why.to_string(),
        detail: detail.to_string(),
        ..Attempt::default()
    }
}

#[test]
fn a_transient_blocking_root_is_requeued_once_and_prioritized() {
    let (_dir, ctx, host) = response_setup();
    host.set_now(T0);
    let mut root = unit("r", Status::Stuck, T0 - 3600);
    root.history
        .push(last_attempt("timeout", "budget exceeded"));
    let (mut st, pairs) = blocking_state(root, 12);
    host.reply("graph blocked", 0, &rows(&pairs));

    check(&ctx, &mut st);

    let root = st.units.get("r-1").unwrap();
    assert_eq!(root.status, Status::Pending);
    assert!(root.requeued, "requeued once by the response");
    assert_eq!(root.attempts, 0, "attempts unchanged: one extra try only");
    assert!(
        crate::pump::priority::list(&ctx.home)
            .unwrap()
            .contains(&(String::from("r"), 1))
    );
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("blockers: requeued r-1 once"))
    );
    assert!(
        host.spawned().is_empty(),
        "a transient root needs no planner"
    );

    st.units.get_mut("r-1").unwrap().status = Status::Stuck;
    check(&ctx, &mut st);

    let root = st.units.get("r-1").unwrap();
    assert_eq!(root.status, Status::Stuck, "no second requeue");
    assert!(root.requeued);
}

#[test]
fn a_persistent_old_blocking_root_goes_to_the_resplit_planner() {
    let (_dir, ctx, host) = response_setup();
    host.set_now(T0);
    let mut root = unit("r", Status::Stuck, T0 - 121 * 60);
    root.history.push(last_attempt(
        "proof-failed",
        "proof failed (exit 1): tests failed",
    ));
    let (mut st, pairs) = blocking_state(root, 12);
    host.reply("graph blocked", 0, &rows(&pairs));

    check(&ctx, &mut st);

    let root = st.units.get("r-1").unwrap();
    assert_eq!(root.status, Status::Splitting);
    assert!(!root.requeued, "an exit-1 failure is not transient");
    assert!(ctx.home.join("resplit/r-1.json").exists());
    assert!(
        host.spawned()
            .iter()
            .any(|command| command.contains("resplit"))
    );
    assert!(
        crate::pump::priority::list(&ctx.home)
            .unwrap()
            .contains(&(String::from("r"), 1))
    );
}

#[test]
fn a_burst_blocker_pass_starts_no_more_than_the_planner_cap() {
    let (_dir, ctx, host) = response_setup();
    host.set_now(T0);
    let uids: Vec<String> = (1..=15).map(|index| format!("r-{index}")).collect();
    let names: Vec<&str> = uids.iter().map(String::as_str).collect();
    let mut st = State::default();
    st.needs.insert(String::from("r"), record("units", &names));
    for uid in &uids {
        let mut stuck = unit("r", Status::Stuck, T0 - 3 * 3600);
        stuck.history.push(last_attempt("finished", "agent exit 1"));
        st.units.insert(uid.clone(), stuck);
    }
    host.reply("graph blocked", 0, &rows(&[]));

    check(&ctx, &mut st);

    assert_eq!(
        host.spawned().len(),
        2,
        "two split planners start at once, the rest stagger"
    );
    let mut launched = 0;
    let mut deferred = 0;
    for uid in &uids {
        let unit = st.units.get(uid).unwrap();
        assert_eq!(unit.status, Status::Splitting, "{uid} waits in splitting");
        match unit.split_retry_at {
            Some(at) => {
                assert_eq!(at, T0 + 60, "{uid} respawns one stagger later");
                deferred += 1;
            }
            None => launched += 1,
        }
    }
    assert_eq!(launched, 2, "only planners_max planners launch now");
    assert_eq!(deferred, 13, "the burst tail defers");
}

#[test]
fn a_young_persistent_root_waits() {
    let (_dir, ctx, host) = response_setup();
    host.set_now(T0);
    let mut root = unit("r", Status::Stuck, T0 - 10 * 60);
    root.history.push(last_attempt(
        "proof-failed",
        "proof failed (exit 1): tests failed",
    ));
    let (mut st, pairs) = blocking_state(root, 12);
    host.reply("graph blocked", 0, &rows(&pairs));

    check(&ctx, &mut st);

    let root = st.units.get("r-1").unwrap();
    assert_eq!(root.status, Status::Stuck);
    assert!(!root.requeued);
    assert!(
        crate::pump::priority::list(&ctx.home)
            .unwrap()
            .contains(&(String::from("r"), 1)),
        "priority is raised immediately"
    );
    assert!(
        host.spawned().is_empty(),
        "no resplit before blocking_replan_minutes"
    );
}

#[test]
fn a_small_hold_gets_no_response() {
    let (_dir, ctx, host) = response_setup();
    host.set_now(T0);
    let mut root = unit("r", Status::Stuck, T0 - 121 * 60);
    root.history
        .push(last_attempt("timeout", "budget exceeded"));
    let (mut st, pairs) = blocking_state(root, 4);
    host.reply("graph blocked", 0, &rows(&pairs));

    check(&ctx, &mut st);

    let root = st.units.get("r-1").unwrap();
    assert_eq!(root.status, Status::Stuck);
    assert!(!root.requeued);
    assert!(crate::pump::priority::list(&ctx.home).unwrap().is_empty());
    assert!(host.spawned().is_empty());
}
