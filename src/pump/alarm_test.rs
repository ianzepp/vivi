//! Tests for [`super::alarm`]: the three stall exceptions, their once-per-hour
//! suppression, and the `status.txt` lines.

use std::fs;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::pump::alarm::{check, status_lines};
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::{GateRecord, Promotion, State, Status, Unit};

/// The fake clock's start.
const T0: u64 = 1_000_000;
/// A 40-hex tip.
const TIP: &str = "0123456789abcdef0123456789abcdef01234567";

/// A context with default alarm thresholds (30 min / 3 h / 45 min).
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

#[test]
fn a_fix_unit_delivered_past_the_limit_raises_once_per_hour() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    st.units.insert(
        String::from("pumpfix-1"),
        unit(Status::Delivered, T0 - 31 * 60),
    );
    st.units.insert(
        String::from("other-1"),
        unit(Status::Delivered, T0 - 31 * 60),
    );
    host.set_now(T0);
    check(&ctx, &mut st);
    let text = exceptions(&ctx);
    assert!(text.contains("stall-fix-unit pumpfix-1 fix unit delivered for 31m"));
    assert!(!text.contains("other-1"), "only fix units alarm here");
    // Again within the hour: suppressed.
    host.advance(1800);
    check(&ctx, &mut st);
    assert_eq!(exceptions(&ctx).matches("stall-fix-unit").count(), 1);
    // After the hour: raised again.
    host.advance(1801);
    check(&ctx, &mut st);
    assert_eq!(exceptions(&ctx).matches("stall-fix-unit").count(), 2);
}

#[test]
fn a_merging_fix_unit_alarms_and_a_fresh_one_does_not() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    st.units.insert(
        String::from("pumpfix-1"),
        unit(Status::Merging, T0 - 40 * 60),
    );
    st.units.insert(
        String::from("pumpfix-2"),
        unit(Status::Delivered, T0 - 29 * 60),
    );
    host.set_now(T0);
    check(&ctx, &mut st);
    let text = exceptions(&ctx);
    assert!(text.contains("stall-fix-unit pumpfix-1 fix unit merging for 40m"));
    assert!(!text.contains("pumpfix-2"));
}

#[test]
fn the_raised_times_survive_in_state_extra() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    st.units.insert(
        String::from("pumpfix-1"),
        unit(Status::Delivered, T0 - 3600),
    );
    host.set_now(T0);
    check(&ctx, &mut st);
    let saved = serde_json::to_string(&st).unwrap();
    let mut reloaded: State = serde_json::from_str(&saved).unwrap();
    check(&ctx, &mut reloaded);
    assert_eq!(exceptions(&ctx).matches("stall-fix-unit").count(), 1);
}

#[test]
fn promotion_stall_names_hours_and_the_last_gate() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    st.promotions.push(Promotion {
        tip: String::from(TIP),
        at: T0 - 4 * 3600,
        failing: None,
    });
    st.gates.push(GateRecord {
        tip: String::from(TIP),
        names: Some(vec![String::from("a"), String::from("b")]),
        ..GateRecord::default()
    });
    st.units.insert(String::from("u1"), merged(T0 - 3600));
    host.set_now(T0);
    check(&ctx, &mut st);
    let text = exceptions(&ctx);
    assert!(
        text.contains("stall-promote default 4h00m since last promotion"),
        "{text}"
    );
    assert!(text.contains("1 merged awaiting promotion"));
    assert!(text.contains("last gate 0123456789 failing 2"));
}

#[test]
fn promotion_stall_is_quiet_without_merges_since_or_under_the_limit() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    st.promotions.push(Promotion {
        tip: String::from(TIP),
        at: T0 - 4 * 3600,
        failing: None,
    });
    // Merged before the promotion: nothing waits.
    st.units.insert(String::from("old"), merged(T0 - 5 * 3600));
    host.set_now(T0);
    check(&ctx, &mut st);
    assert!(!exceptions(&ctx).contains("stall-promote"));
    // A merge since, but the promotion is only 2 h old.
    st.promotions[0].at = T0 - 2 * 3600;
    st.units.insert(String::from("new"), merged(T0 - 3600));
    check(&ctx, &mut st);
    assert!(!exceptions(&ctx).contains("stall-promote"));
}

#[test]
fn promotion_stall_with_no_promotion_ever_counts_from_the_first_merge() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    st.units.insert(String::from("u1"), merged(T0 - 4 * 3600));
    host.set_now(T0);
    check(&ctx, &mut st);
    let text = exceptions(&ctx);
    assert!(
        text.contains("stall-promote default no promotion yet"),
        "{text}"
    );
    assert!(text.contains("no gate has finished"));
}

#[test]
fn promotion_stall_is_quiet_when_promotion_is_disabled() {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        branch: String::from("next"),
        promote: false,
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
    let mut st = State::default();
    st.units.insert(String::from("u1"), merged(T0 - 4 * 3600));
    host.set_now(T0);
    check(&ctx, &mut st);
    assert!(!exceptions(&ctx).contains("stall-promote"));
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
fn status_lines_report_promotion_fix_and_delivered_backlog() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    st.promotions.push(Promotion {
        tip: String::from(TIP),
        at: T0 - (2 * 3600 + 5 * 60),
        failing: None,
    });
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
        vec![
            String::from("last promote 2h05m ago (0123456789)"),
            String::from("oldest waiting fix pumpfix-2 20m"),
            String::from("delivered waiting 2, oldest 14m"),
        ]
    );
}

#[test]
fn status_lines_report_none_for_an_idle_pump() {
    let (_dir, ctx, host) = setup();
    host.set_now(T0);
    assert_eq!(
        status_lines(&ctx, &State::default()),
        vec![
            String::from("last promote never"),
            String::from("oldest waiting fix none"),
            String::from("delivered waiting 0, oldest none"),
        ]
    );
}
