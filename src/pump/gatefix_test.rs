//! Tests for [`crate::pump::gatefix::expire_gatefix`]: a gate-fix planner
//! that exits without writing its plan is retried with backoff and reported
//! after three planless exits.

use std::collections::BTreeMap;
use std::fs;

use tempfile::tempdir;

use crate::pump::config::{PlannerConfig, PumpConfig};
use crate::pump::gatefix::expire_gatefix;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::state::State;

/// The fake clock's start.
const T0: u64 = 1_000_000;
/// The gate-fix need that owns the red name.
const NEED: &str = "pumpfix-g01234567";

/// A config whose planner command is set so respawns are visible.
fn planner_cfg() -> PumpConfig {
    PumpConfig {
        planner: PlannerConfig {
            run: String::from("claude -p --model claude-sonnet-5-5 {charter}"),
            charter: String::from("planner-charter.md"),
            ..PlannerConfig::default()
        },
        ..PumpConfig::default()
    }
}

/// A state whose red name is owned by the gate-fix need.
fn red_state() -> State {
    State {
        red: BTreeMap::from([(String::from("crate::x"), String::from(NEED))]),
        ..State::default()
    }
}

/// Write the planner prompt and an exit receipt carrying `code`.
fn write_planner_exit(ctx: &Ctx, code: &str) {
    let dir = ctx.home.join("gatefix");
    drop(fs::create_dir_all(&dir));
    drop(fs::write(dir.join(format!("{NEED}.prompt")), "plan the fix"));
    drop(fs::write(dir.join(format!("{NEED}.out.rc")), format!("{code}\n")));
}

/// The u64 value of `key -> need` in the state's extra maps.
fn extra_u64(st: &State, key: &str) -> Option<u64> {
    st.extra.get(key)?.get(NEED)?.as_u64()
}

#[test]
fn an_exited_gatefix_planner_is_retried_with_backoff() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), planner_cfg());
    let mut st = red_state();
    write_planner_exit(&ctx, "1");

    expire_gatefix(&ctx, &mut st);
    assert!(host.spawned().is_empty());
    assert_eq!(extra_u64(&st, "gatefix_fails"), Some(1));
    assert_eq!(extra_u64(&st, "gatefix_retry_at"), Some(T0 + 60));
    assert!(host.logs().iter().any(|line| {
        line.contains(&format!(
            "gatefix planner for {NEED} exited rc=1 without a plan; retry 1/3 in 60s"
        ))
    }));

    host.set_now(T0 + 60);
    expire_gatefix(&ctx, &mut st);
    let spawned = host.spawned();
    assert_eq!(spawned.len(), 1);
    assert!(spawned[0].contains("claude -p --model claude-sonnet-5-5"));
    assert!(!st.extra.contains_key("gatefix_retry_at"));
    assert_eq!(extra_u64(&st, "gatefix_fails"), Some(1));
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains(&format!("gatefix retry {NEED}")))
    );
    let prompt = fs::read_to_string(ctx.home.join("gatefix").join(format!("{NEED}.prompt"))).unwrap();
    assert_eq!(prompt, "plan the fix");
    drop(dir);
}

#[test]
fn three_exits_without_a_plan_raise_gatefix_no_plan() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), planner_cfg());
    let mut st = red_state();

    write_planner_exit(&ctx, "1");
    expire_gatefix(&ctx, &mut st);
    assert_eq!(extra_u64(&st, "gatefix_retry_at"), Some(T0 + 60));
    host.advance(60);

    write_planner_exit(&ctx, "1");
    expire_gatefix(&ctx, &mut st);
    assert_eq!(extra_u64(&st, "gatefix_fails"), Some(2));
    assert_eq!(extra_u64(&st, "gatefix_retry_at"), Some(T0 + 180));
    host.advance(120);

    write_planner_exit(&ctx, "1");
    expire_gatefix(&ctx, &mut st);

    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exceptions.contains(&format!("gatefix-no-plan {NEED}")));
    assert!(host.spawned().is_empty());
    assert!(!st.extra.contains_key("gatefix_retry_at"));
    drop(dir);
}
