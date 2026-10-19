//! Tests for [`super::blockers`]: the root of a stalled need and the units
//! that depend on it, directly or through the graph.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::pump::blockers::compute;
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::{NeedRecord, State, Status, Unit};

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
