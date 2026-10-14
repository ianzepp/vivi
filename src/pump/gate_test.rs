//! Tests for [`crate::pump::gate`]: the failing-name reply parser and the
//! gate start rules, driven through [`FakeHost`] replies.

use std::fs;
use std::sync::Arc;

use tempfile::tempdir;

use crate::pump::config::{GateConfig, PumpConfig};
use crate::pump::gate::parse_failing;
use crate::pump::gate::start_gate;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::{GateRun, State};

/// The fake clock's start.
const T0: u64 = 1_000_000;
/// A 40-hex remote tip.
const TIP: &str = "0123456789abcdef0123456789abcdef01234567";
/// The gate task handle the fake `task send` reports.
const TASK: &str = "1a2b3c4d";

/// A gate config with two pools, the count at 8 and the clock at 1800 s.
fn gate_cfg() -> PumpConfig {
    PumpConfig {
        branch: String::from("next"),
        gate: GateConfig {
            command: String::from("stage 4b"),
            every_merges: 8,
            every_seconds: 1800,
            pools: vec![String::from("flyfull"), String::from("flygate")],
            model: String::from("zai/glm-5.3-flash"),
            budget: 1800,
        },
        ..PumpConfig::default()
    }
}

/// A context with [`gate_cfg`], home at `dir/home`.
fn setup() -> (tempfile::TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), gate_cfg());
    (dir, ctx, host)
}

/// A state with a baseline and a past gate at the same merge count.
fn fresh_state() -> State {
    State {
        merges: 4,
        baseline: Some(vec![String::from("stale crate::test::old")]),
        last_gate: GateRun {
            at: T0,
            merges: 4,
            task: None,
            tip: None,
            pool: None,
        },
        ..State::default()
    }
}

/// Replies for a full gate start: flyfull busy, flygate free, a tip, a task
/// send that reports the handle, and a successful dispatch.
fn wire(host: &Arc<FakeHost>) {
    host.reply(
        "pool status",
        0,
        "slot flyfull bridge slot-1 occupied\nslot flygate bridge slot-9 free\n",
    );
    host.reply("git ls-remote", 0, &format!("{TIP}\trefs/heads/next\n"));
    host.reply("task send", 0, &format!("created hand {TASK}\nnode {TASK}"));
    host.reply("pool dispatch", 0, &format!("dispatched {TASK} bridge slot-9"));
}

#[test]
fn parse_failing_reads_the_marker_block() {
    let text = "rc=0\nFAILING-BEGIN\nvivi pump::gate::a\n  vivi pump::gate::b  \n\
                FAILING-END\n120 passed\n";
    assert_eq!(
        parse_failing(text),
        Some(vec![
            String::from("vivi pump::gate::a"),
            String::from("vivi pump::gate::b"),
        ])
    );
}

#[test]
fn parse_failing_takes_the_last_usable_block() {
    let text = "FAILING-BEGIN\nvivi pump::gate::old\nFAILING-END\n\
                FAILING-BEGIN\n<crate> <test name>\nFAILING-END\n";
    assert_eq!(
        parse_failing(text),
        Some(vec![String::from("vivi pump::gate::old")])
    );
}

#[test]
fn parse_failing_an_empty_block_is_zero_failures() {
    assert_eq!(parse_failing("FAILING-BEGIN\nFAILING-END\n"), Some(vec![]));
    assert_eq!(parse_failing("FAILING-BEGIN\n \n\t\nFAILING-END\n"), Some(vec![]));
}

#[test]
fn parse_failing_build_failed_marker() {
    let text = "rc=1\nFAILING-BEGIN\nBUILD-FAILED\nFAILING-END\n\
                error: could not compile `vivi` (lib)\n";
    assert_eq!(parse_failing(text), Some(vec![String::from("BUILD-FAILED")]));
}

#[test]
fn parse_failing_falls_back_to_crate_and_path_pairs_sorted_and_deduplicated() {
    let text = "failures: (`vivi pump::gate::b`, vivi pump::gate::b)\n\
                other crate plain main single and x y\n\
                zed tests::x works; (vivi pump::gate::a)\n";
    assert_eq!(
        parse_failing(text),
        Some(vec![
            String::from("vivi pump::gate::a"),
            String::from("vivi pump::gate::b"),
            String::from("zed tests::x"),
        ])
    );
}

#[test]
fn parse_failing_returns_none_for_prose() {
    assert_eq!(parse_failing("all 120 tests passed, nothing to see\n"), None);
    assert_eq!(
        parse_failing("FAILING-BEGIN\n<crate> <test name>\nFAILING-END\n"),
        None
    );
}

#[test]
fn start_gate_is_not_due_before_the_merge_count_or_the_clock() {
    let (dir, ctx, host) = setup();
    let mut st = fresh_state();
    host.advance(1000);
    start_gate(&ctx, &mut st);
    assert!(host.commands().is_empty());
    st.merges = 5;
    start_gate(&ctx, &mut st);
    assert!(host.commands().is_empty());
    assert!(st.last_gate.task.is_none());
    drop(dir);
}

#[test]
fn start_gate_is_due_with_no_baseline_and_dispatches_read_only_to_the_first_pool_with_a_slot() {
    let (dir, ctx, host) = setup();
    wire(&host);
    let mut st = State {
        baseline: None,
        ..fresh_state()
    };
    start_gate(&ctx, &mut st);
    let gate = &st.last_gate;
    assert_eq!(gate.task.as_deref(), Some(TASK));
    assert_eq!(gate.tip.as_deref(), Some(TIP));
    assert_eq!(gate.pool.as_deref(), Some("flygate"));
    assert_eq!(gate.merges, 4);
    assert_eq!(gate.at, ctx.now());
    let commands = host.commands();
    let send = commands.iter().find(|c| c.contains("task send")).unwrap();
    assert!(send.contains("--subject '[pump gate] stage 4b at 0123456789'"));
    assert!(send.contains("--body-file"));
    let dispatch = commands
        .iter()
        .find(|c| c.contains("pool dispatch"))
        .unwrap();
    assert!(dispatch.contains(&format!("--pool flygate {TASK}")));
    assert!(dispatch.contains("--model zai/glm-5.3-flash --read-only --from"));
    assert!(dispatch.contains(&format!("--from {TIP} --budget 1800 --porcelain")));
    let brief = fs::read_to_string(ctx.home.join("briefs/gate.txt")).unwrap();
    assert!(brief.contains("FAILING-BEGIN"));
    drop(dir);
}

#[test]
fn start_gate_force_flag_is_removed_when_the_gate_dispatches() {
    let (dir, ctx, host) = setup();
    wire(&host);
    fs::write(ctx.home.join("FORCE-GATE"), "after fixes\n").unwrap();
    let mut st = fresh_state();
    start_gate(&ctx, &mut st);
    assert!(!ctx.flag("FORCE-GATE"));
    assert_eq!(st.last_gate.task.as_deref(), Some(TASK));
    assert_eq!(st.last_gate.pool.as_deref(), Some("flygate"));
    drop(dir);
}

#[test]
fn a_failed_gate_dispatch_marks_the_task_done_and_records_nothing() {
    let (dir, ctx, host) = setup();
    wire(&host);
    host.reply("pool dispatch", 1, "boom: no worker online\n");
    fs::write(ctx.home.join("FORCE-GATE"), "after fixes\n").unwrap();
    let mut st = State {
        baseline: None,
        ..fresh_state()
    };
    start_gate(&ctx, &mut st);
    let commands = host.commands();
    let done = commands.iter().find(|c| c.contains("task done")).unwrap();
    assert!(done.contains(&format!("task done {TASK} --for operator --note 'dispatch failed'")));
    assert!(host
        .logs()
        .iter()
        .any(|line| line.contains("gate dispatch failed: boom: no worker online")));
    assert!(ctx.flag("FORCE-GATE"));
    assert!(st.last_gate.task.is_none());
    drop(dir);
}
