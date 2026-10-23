//! Tests for [`super::alarm`]: the stall exceptions, their once-per-hour
//! suppression, and the `status.txt` line.

use std::fs;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::pump::alarm::{check, status_lines};
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::{NeedRecord, State, Status, Unit};

/// The fake clock's start.
const T0: u64 = 1_000_000;

/// A context with default alarm thresholds (45 min merge idle).
fn setup() -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        branch: String::from("next"),
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
    (dir, ctx, host)
}

/// A unit in `status` whose `since` is `ago` seconds before the clock start.
fn unit(status: Status, since: u64) -> Unit {
    Unit {
        status,
        since,
        ..Unit::default()
    }
}

/// The exceptions file text, empty when none was written.
fn exceptions(ctx: &Ctx) -> String {
    fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap_or_default()
}

/// A merged unit with `merged_at`.
fn merged(at: u64) -> Unit {
    Unit {
        merged_at: Some(at),
        ..unit(Status::Merged, at)
    }
}

/// A unit of `need` in `status`, created `since` epoch seconds.
fn need_unit(need: &str, status: Status, since: u64) -> Unit {
    Unit {
        need: need.to_string(),
        ..unit(status, since)
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

/// Add `count` pending dependent needs of `root`, pushing their `graph
/// blocked` rows into `pairs`.
fn dependents(st: &mut State, root: &str, count: usize, pairs: &mut Vec<(String, Vec<String>)>) {
    for index in 1..=count {
        let need = format!("m{index}");
        st.needs.insert(need.clone(), record("units", &[]));
        st.units.insert(
            format!("{need}-1"),
            need_unit(&need, Status::Pending, T0 - 600),
        );
        pairs.push((need, vec![root.to_string()]));
    }
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
fn the_raised_times_survive_in_state_extra() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    st.units
        .insert(String::from("u1"), unit(Status::Delivered, T0 - 50 * 60));
    host.set_now(T0);
    check(&ctx, &mut st);
    let saved = serde_json::to_string(&st).unwrap();
    let mut reloaded: State = serde_json::from_str(&saved).unwrap();
    check(&ctx, &mut reloaded);
    assert_eq!(exceptions(&ctx).matches("stall-merge").count(), 1);
}

#[test]
fn merge_stall_fires_when_delivered_units_sit_with_no_merge() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    st.units
        .insert(String::from("u1"), unit(Status::Delivered, T0 - 50 * 60));
    st.units.insert(String::from("m"), merged(T0 - 60 * 60));
    host.set_now(T0);
    check(&ctx, &mut st);
    let text = exceptions(&ctx);
    assert!(
        text.contains("stall-merge pump 1 delivered, oldest waiting 50m"),
        "{text}"
    );
    assert!(text.contains("last merge 1h00m ago"));
}

#[test]
fn merge_stall_is_quiet_when_merges_flow_or_the_wait_is_short() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    st.units
        .insert(String::from("u1"), unit(Status::Delivered, T0 - 50 * 60));
    st.units.insert(String::from("m"), merged(T0 - 5 * 60));
    host.set_now(T0);
    check(&ctx, &mut st);
    assert!(!exceptions(&ctx).contains("stall-merge"), "a recent merge");
    st.units.get_mut("m").unwrap().merged_at = Some(T0 - 3 * 3600);
    st.units.get_mut("u1").unwrap().since = T0 - 10 * 60;
    check(&ctx, &mut st);
    assert!(!exceptions(&ctx).contains("stall-merge"), "a short wait");
}

#[test]
fn status_lines_report_the_delivered_backlog() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    st.units.insert(
        String::from("pumpfix-1"),
        unit(Status::Delivered, T0 - 14 * 60),
    );
    st.units.insert(
        String::from("pumpfix-2"),
        unit(Status::Merging, T0 - 20 * 60),
    );
    st.units
        .insert(String::from("u1"), unit(Status::Delivered, T0 - 3 * 60));
    host.set_now(T0);
    assert_eq!(
        status_lines(&ctx, &st),
        vec![String::from("delivered waiting 2, oldest 14m")]
    );
}

#[test]
fn status_lines_report_none_for_an_idle_pump() {
    let (_dir, ctx, host) = setup();
    host.set_now(T0);
    assert_eq!(
        status_lines(&ctx, &State::default()),
        vec![String::from("delivered waiting 0, oldest none")]
    );
}

#[test]
fn a_blocking_stuck_exception_names_the_transitive_hold_count() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    st.needs
        .insert(String::from("r"), record("units", &["r-1"]));
    st.units.insert(
        String::from("r-1"),
        need_unit("r", Status::Stuck, T0 - 30 * 60),
    );
    let mut pairs: Vec<(String, Vec<String>)> = Vec::new();
    dependents(&mut st, "r", 12, &mut pairs);
    host.reply("graph blocked", 0, &rows(&pairs));
    host.set_now(T0);

    check(&ctx, &mut st);

    let text = exceptions(&ctx);
    assert!(
        text.contains("blocking-stuck r-1"),
        "no exception in {text}"
    );
    assert!(
        text.contains("stuck for 30m; holds 13 units across 12 needs"),
        "hold count missing from {text}"
    );
    assert!(text.contains("top held: m1 (1)"), "top needs in {text}");
    // Within the hour: suppressed, exactly like the stall alarms.
    host.advance(1800);
    check(&ctx, &mut st);
    assert_eq!(exceptions(&ctx).matches("blocking-stuck").count(), 1);
    // Past the hour: raised again while the block persists.
    host.advance(1801);
    check(&ctx, &mut st);
    assert_eq!(exceptions(&ctx).matches("blocking-stuck").count(), 2);
}

#[test]
fn a_parked_root_raises_with_the_need_as_subject() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    st.needs.insert(String::from("p"), record("operator", &[]));
    st.needs.insert(String::from("q"), record("units", &[]));
    for index in 1..=10 {
        st.units.insert(
            format!("q-{index}"),
            need_unit("q", Status::Pending, T0 - 100),
        );
    }
    host.reply(
        "graph blocked",
        0,
        &rows(&[(String::from("q"), vec![String::from("p")])]),
    );
    host.set_now(T0);

    check(&ctx, &mut st);

    let text = exceptions(&ctx);
    assert!(
        text.contains("blocking-stuck p operator"),
        "no exception in {text}"
    );
    assert!(text.contains("holds 10 units across 1 need"), "{text}");
}

#[test]
fn a_small_hold_does_not_raise() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    st.needs
        .insert(String::from("r"), record("units", &["r-1"]));
    st.units.insert(
        String::from("r-1"),
        need_unit("r", Status::Stuck, T0 - 30 * 60),
    );
    let mut pairs: Vec<(String, Vec<String>)> = Vec::new();
    dependents(&mut st, "r", 4, &mut pairs);
    host.reply("graph blocked", 0, &rows(&pairs));
    host.set_now(T0);

    check(&ctx, &mut st);

    assert!(
        !exceptions(&ctx).contains("blocking-stuck"),
        "a five-unit hold is below the default threshold: {}",
        exceptions(&ctx)
    );
}
