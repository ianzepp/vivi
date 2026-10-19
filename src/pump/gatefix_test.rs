//! Tests for [`crate::pump::gatefix`]: a gate-fix planner exit receipt is
//! counted with backoff, and a written plan clears the retry state.

use std::fs;
use std::sync::Arc;

use tempfile::tempdir;

use crate::pump::config::PumpConfig;
use crate::pump::gatefix::expire_gatefix;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::State;

/// The fake clock's start.
const T0: u64 = 1_000_000;
/// The gate-fix need id under test.
const NEED: &str = "pumpfix-g01234567";

/// A planner command so gatefix spawns would be visible.
fn planner_cfg() -> PumpConfig {
    PumpConfig {
        planner: crate::pump::config::PlannerConfig {
            run: String::from("claude -p --model claude-sonnet-5-5 {charter}"),
            charter: String::from("planner-charter.md"),
            ..crate::pump::config::PlannerConfig::default()
        },
        ..PumpConfig::default()
    }
}

/// A context with [`planner_cfg`], home at `dir/home`.
fn setup() -> (tempfile::TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), planner_cfg());
    (dir, ctx, host)
}

/// A state with one red failure name owned by the gate-fix need.
fn red_state() -> State {
    let mut st = State::default();
    st.red.insert(String::from("crate::x"), String::from(NEED));
    st
}

#[test]
fn an_exited_gatefix_planner_is_counted_with_backoff() {
    let (dir, ctx, host) = setup();
    host.set_now(T0);
    let gatefix = ctx.home.join("gatefix");
    drop(fs::create_dir_all(&gatefix));
    let receipt = gatefix.join(format!("{NEED}.out.rc"));
    fs::write(&receipt, "1\n").unwrap();
    let mut st = red_state();
    expire_gatefix(&ctx, &mut st);
    assert_eq!(st.extra["gatefix_fails"][NEED], serde_json::json!(1));
    assert_eq!(
        st.extra["gatefix_retry_at"][NEED],
        serde_json::json!(T0 + 60)
    );
    assert!(!receipt.exists());
    assert!(host.spawned().is_empty());
    assert!(host.logs().iter().any(|line| line.contains(
        "gatefix planner for pumpfix-g01234567 exited rc=1 without a plan; retry 1/3 in 60s"
    )));
    drop(dir);
}

#[test]
fn a_written_plan_clears_the_gatefix_retry_state() {
    let (dir, ctx, host) = setup();
    host.set_now(T0);
    drop(fs::create_dir_all(ctx.home.join("plans")));
    fs::write(
        ctx.home.join("plans").join(format!("{NEED}.json")),
        b"{\"verdict\": \"units\"}",
    )
    .unwrap();
    let mut st = red_state();
    let mut fails = serde_json::Map::new();
    fails.insert(String::from(NEED), serde_json::json!(2_u64));
    st.extra.insert(
        String::from("gatefix_fails"),
        serde_json::Value::Object(fails),
    );
    let mut retries = serde_json::Map::new();
    retries.insert(String::from(NEED), serde_json::json!(T0));
    st.extra.insert(
        String::from("gatefix_retry_at"),
        serde_json::Value::Object(retries),
    );
    expire_gatefix(&ctx, &mut st);
    assert!(
        st.extra
            .get("gatefix_fails")
            .and_then(|map| map.get(NEED))
            .is_none()
    );
    assert!(
        st.extra
            .get("gatefix_retry_at")
            .and_then(|map| map.get(NEED))
            .is_none()
    );
    drop(dir);
}

/// A due retry with a saved prompt respawns the planner exactly once.
#[test]
fn a_due_gatefix_retry_respawns_the_planner() {
    let (dir, ctx, host) = setup();
    host.set_now(T0);
    let gatefix = ctx.home.join("gatefix");
    drop(fs::create_dir_all(&gatefix));
    fs::write(gatefix.join(format!("{NEED}.prompt")), b"plan the fix").unwrap();
    let mut st = red_state();
    let mut fails = serde_json::Map::new();
    fails.insert(String::from(NEED), serde_json::json!(1_u64));
    st.extra.insert(
        String::from("gatefix_fails"),
        serde_json::Value::Object(fails),
    );
    let mut retries = serde_json::Map::new();
    retries.insert(String::from(NEED), serde_json::json!(T0 + 60));
    st.extra.insert(
        String::from("gatefix_retry_at"),
        serde_json::Value::Object(retries),
    );
    host.set_now(T0 + 59);
    expire_gatefix(&ctx, &mut st);
    assert!(host.spawned().is_empty());
    assert_eq!(
        st.extra["gatefix_retry_at"][NEED],
        serde_json::json!(T0 + 60)
    );
    host.set_now(T0 + 60);
    expire_gatefix(&ctx, &mut st);
    let spawned = host.spawned();
    assert_eq!(spawned.len(), 1, "spawned: {spawned:?}");
    assert!(spawned[0].contains("claude -p --model claude-sonnet-5-5"));
    assert!(
        st.extra
            .get("gatefix_retry_at")
            .and_then(|map| map.get(NEED))
            .is_none()
    );
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("gatefix retry pumpfix-g01234567"))
    );
    drop(dir);
}
