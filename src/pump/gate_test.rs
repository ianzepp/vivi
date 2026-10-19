//! Tests for [`crate::pump::gate`]: the failing-name reply parser and the
//! gate start rules, driven through [`FakeHost`] replies.

use std::fs;
use std::sync::Arc;

use tempfile::tempdir;

use crate::pump::config::{GateConfig, PumpConfig, PumpTarget};
use crate::pump::gate::{external_gates, finish_gate, hold_cycle, parse_failing, start_gate};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::{GateRecord, GateRun, State, Status, Unit};

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
            ..GateConfig::default()
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
    host.reply(
        "pool dispatch",
        0,
        &format!("dispatched {TASK} bridge slot-9"),
    );
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
    assert_eq!(
        parse_failing("FAILING-BEGIN\n \n\t\nFAILING-END\n"),
        Some(vec![])
    );
}

#[test]
fn parse_failing_build_failed_marker() {
    let text = "rc=1\nFAILING-BEGIN\nBUILD-FAILED\nFAILING-END\n\
                error: could not compile `vivi` (lib)\n";
    assert_eq!(
        parse_failing(text),
        Some(vec![String::from("BUILD-FAILED")])
    );
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
    assert_eq!(
        parse_failing("all 120 tests passed, nothing to see\n"),
        None
    );
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
fn a_named_target_gate_uses_its_branch_and_persists_its_own_run() {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        targets: [(
            String::from("radix"),
            PumpTarget {
                path: String::from("radix"),
                branch: String::from("radix-next"),
                gate: Some(GateConfig {
                    command: String::from("./gate"),
                    pools: vec![String::from("flygate")],
                    model: String::from("m/gate"),
                    ..GateConfig::default()
                }),
                ..PumpTarget::default()
            },
        )]
        .into(),
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
    let target_ctx = ctx.for_target("radix").unwrap();
    host.reply("pool status", 0, "slot flygate bridge slot-1 free\n");
    host.reply(
        "git ls-remote",
        0,
        &format!("{TIP}\trefs/heads/radix-next\n"),
    );
    host.reply("task send", 0, &format!("created hand {TASK}\nnode {TASK}"));
    host.reply(
        "pool dispatch",
        0,
        &format!("dispatched {TASK} bridge slot-1"),
    );
    let mut state = State::default();
    crate::pump::state::with_target_state(&mut state, "radix", |target| {
        start_gate(&target_ctx, target);
    });
    assert_eq!(state.last_gate.task, None);
    assert_eq!(
        state.target_states["radix"].last_gate.task.as_deref(),
        Some(TASK)
    );
    assert!(
        host.commands()
            .iter()
            .any(|command| command.contains("refs/heads/radix-next"))
    );
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
    assert!(done.contains(&format!(
        "task done {TASK} --for operator --note 'dispatch failed'"
    )));
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("gate dispatch failed: boom: no worker online"))
    );
    assert!(ctx.flag("FORCE-GATE"));
    assert!(st.last_gate.task.is_none());
    drop(dir);
}

/// A second 40-hex tip: the last clean gate.
const CLEAN: &str = "fedcba9876543210fedcba9876543210fedcba98";

/// A planner command so gatefix spawns are visible.
fn planner_cfg() -> PumpConfig {
    PumpConfig {
        planner: crate::pump::config::PlannerConfig {
            run: String::from("claude -p --model claude-sonnet-5-5 {charter}"),
            charter: String::from("planner-charter.md"),
            ..crate::pump::config::PlannerConfig::default()
        },
        ..gate_cfg()
    }
}

/// A state with a baseline, a merge count and an open gate task at `TIP`.
fn gate_state(merges: u64) -> State {
    State {
        merges,
        baseline: Some(vec![String::from("old crate::base")]),
        last_gate: GateRun {
            at: T0,
            merges,
            task: Some(String::from(TASK)),
            tip: Some(String::from(TIP)),
            pool: Some(String::from("flygate")),
        },
        ..State::default()
    }
}

/// Four merged units `u1`..`u4` in `st.order`.
fn add_merged_units(st: &mut State) {
    for uid in ["u1", "u2", "u3", "u4"] {
        st.units.insert(
            String::from(uid),
            Unit {
                status: Status::Merged,
                since: T0,
                ..Unit::default()
            },
        );
        st.order.push(String::from(uid));
    }
}

/// Replies for a successful promote.
fn wire_promote(host: &Arc<FakeHost>) {
    host.reply("git fetch", 0, "");
    host.reply("git merge-base", 0, "");
    host.reply("git push", 0, "To origin\n   abc..def  main -> main\n");
}

/// Pin a file's modification time to `secs` after the epoch.
fn set_mtime(path: &std::path::Path, secs: u64) {
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_times(
        fs::FileTimes::new()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)),
    )
    .unwrap();
}

/// Every unit id with its status, in `order` sequence.
fn statuses(st: &State) -> Vec<(String, Status)> {
    st.order
        .iter()
        .map(|uid| (uid.clone(), st.units[uid.as_str()].status))
        .collect()
}

#[test]
fn the_first_gate_sets_the_baseline_and_does_not_promote() {
    let (dir, ctx, host) = setup();
    let mut st = gate_state(4);
    st.baseline = None;
    finish_gate(
        &ctx,
        &mut st,
        "FAILING-BEGIN\nvivi pump::gate::a\nFAILING-END\n",
    );
    assert_eq!(st.baseline, Some(vec![String::from("vivi pump::gate::a")]));
    assert_eq!(st.last_gate.task, None);
    let rec = st.gates.last().unwrap();
    assert_eq!(rec.tip, TIP);
    assert_eq!(rec.merges, 4);
    assert_eq!(rec.names, Some(vec![String::from("vivi pump::gate::a")]));
    assert_eq!(rec.new, None);
    assert!(host.commands().is_empty());
    assert!(host.spawned().is_empty());
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("gate baseline at 0123456789: 1 failing names"))
    );
    drop(dir);
}

#[test]
fn names_in_the_baseline_the_ignore_file_or_owned_are_not_new() {
    let dir = tempdir().unwrap();
    let ignore = dir.path().join("ignore.txt");
    fs::write(&ignore, "# flaky\n\n  known crate::b  \n").unwrap();
    let mut cfg = gate_cfg();
    cfg.gate.ignore_file = Some(ignore.display().to_string());
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    let mut st = gate_state(4);
    st.red
        .insert(String::from("owned crate::c"), String::from("pumpfix-old"));
    let text = "FAILING-BEGIN\nold crate::base\nknown crate::b\nowned crate::c\n\
                fresh crate::d\nFAILING-END\n";
    finish_gate(&ctx, &mut st, text);
    let rec = st.gates.last().unwrap();
    assert_eq!(rec.new, Some(vec![String::from("fresh crate::d")]));
    assert!(
        !rec.names
            .as_ref()
            .unwrap()
            .iter()
            .any(|n| n == "known crate::b")
    );
    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exceptions.contains("gate-new-failures 0123456789 1 new: fresh crate::d"));
    drop(dir);
}

#[test]
fn a_new_failure_is_owned_by_the_fix_need_and_starts_a_gatefix_planner() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), planner_cfg());
    let mut st = gate_state(3);
    st.gates.push(GateRecord {
        tip: String::from(CLEAN),
        at: T0,
        merges: 1,
        names: Some(vec![]),
        new: None,
    });
    add_merged_units(&mut st);
    finish_gate(
        &ctx,
        &mut st,
        "FAILING-BEGIN\nvivi pump::gate::gone\nFAILING-END\n",
    );
    assert_eq!(
        st.red.get("vivi pump::gate::gone"),
        Some(&String::from("pumpfix-g01234567"))
    );
    let spawned = host.spawned();
    assert_eq!(spawned.len(), 1);
    assert!(spawned[0].contains("claude -p --model claude-sonnet-5-5 planner-charter.md"));
    let prompt = fs::read_to_string(ctx.home.join("gatefix/pumpfix-g01234567.prompt")).unwrap();
    assert!(prompt.contains(&format!("commit {TIP} shows these failing tests")));
    assert!(prompt.contains(&format!("(commit {CLEAN}):")));
    assert!(prompt.contains("- vivi pump::gate::gone"));
    assert!(prompt.contains(&format!("git log --oneline {CLEAN}..{TIP}")));
    assert!(prompt.contains(&format!("` in {} ", ctx.repo.display())));
    assert!(prompt.contains(&format!(
        "Write {}/plans/pumpfix-g01234567.json",
        ctx.home.display()
    )));
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("gatefix planner started for pumpfix-g01234567 (1 new)"))
    );
    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(
        exceptions.contains(
            "gate-new-failures 0123456789 1 new: vivi pump::gate::gone ; suspects: u2 u3"
        )
    );
    drop(dir);
}

#[test]
fn a_red_gate_sends_a_real_backing_need_and_plans_under_it() {
    let dir = tempdir().unwrap();
    let mut cfg = planner_cfg();
    cfg.board.from = vec![String::from("operator")];
    let (ctx, host) = test_ctx(dir.path(), cfg);
    host.reply("need send", 0, "created operator ab12cd34\nsent ab12cd34\n");
    let mut st = gate_state(3);
    st.gates.push(GateRecord {
        tip: String::from(CLEAN),
        at: T0,
        merges: 1,
        names: Some(vec![]),
        new: None,
    });
    add_merged_units(&mut st);
    finish_gate(
        &ctx,
        &mut st,
        "FAILING-BEGIN\nvivi pump::gate::gone\nFAILING-END\n",
    );
    assert_eq!(
        st.red.get("vivi pump::gate::gone"),
        Some(&String::from("ab12cd34"))
    );
    let need_send = host
        .commands()
        .into_iter()
        .find(|c| c.contains("need send"))
        .unwrap();
    assert!(need_send.contains("--from mind"));
    assert!(need_send.contains("--to operator"));
    assert!(need_send.contains("gate fix for g01234567"));
    let prompt = fs::read_to_string(ctx.home.join("gatefix/ab12cd34.prompt")).unwrap();
    assert!(prompt.contains("plans/ab12cd34.json"));
    assert!(prompt.contains("Unit ids \"pumpfix-g01234567-"));
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("gatefix planner started for ab12cd34 (1 new)"))
    );
    drop(dir);
}

#[test]
fn an_owned_name_that_passes_again_is_released() {
    let (dir, ctx, host) = setup();
    let mut st = gate_state(4);
    st.red
        .insert(String::from("old crate::base"), String::from("pumpfix-old"));
    finish_gate(&ctx, &mut st, "FAILING-BEGIN\nFAILING-END\n");
    assert!(st.red.is_empty());
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("red fixed: old crate::base (owner pumpfix-old)"))
    );
    drop(dir);
}

#[test]
fn a_gate_with_no_new_failure_promotes_main_by_fast_forward_push() {
    let (dir, ctx, host) = setup();
    wire_promote(&host);
    let mut st = gate_state(4);
    finish_gate(
        &ctx,
        &mut st,
        "FAILING-BEGIN\nold crate::base\nFAILING-END\n",
    );
    let commands = host.commands();
    let push = commands.iter().find(|c| c.contains("git push")).unwrap();
    assert_eq!(*push, format!("git push origin {TIP}:refs/heads/main"));
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("promote main -> 0123456789 rc=0"))
    );
    assert_eq!(st.promotions.len(), 1);
    assert_eq!(st.promotions[0].tip, TIP);
    assert_eq!(st.promotions[0].at, ctx.now());
    drop(dir);
}

#[test]
fn promote_is_skipped_when_main_has_commits_the_integration_branch_lacks() {
    let (dir, ctx, host) = setup();
    host.reply("git fetch", 0, "");
    host.reply("git merge-base", 1, "");
    let mut st = gate_state(4);
    finish_gate(&ctx, &mut st, "FAILING-BEGIN\nFAILING-END\n");
    assert!(host.commands().iter().all(|c| !c.contains("git push")));
    assert!(st.promotions.is_empty());
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("promote skipped: main has commits next does not have yet"))
    );
    drop(dir);
}

#[test]
fn an_unreadable_reply_raises_gate_unreadable_and_records_names_none() {
    let (dir, ctx, _host) = setup();
    let mut st = gate_state(4);
    finish_gate(&ctx, &mut st, "the worker rambled, no names here");
    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exceptions.contains("gate-unreadable 0123456789 the worker rambled, no names here"));
    let rec = st.gates.last().unwrap();
    assert_eq!(rec.names, None);
    assert_eq!(rec.new, None);
    assert_eq!(st.last_gate.task, None);
    assert!(st.red.is_empty());
    drop(dir);
}

#[test]
fn a_build_failed_reply_raises_gate_build_failed() {
    let (dir, ctx, _host) = setup();
    let mut st = gate_state(4);
    finish_gate(
        &ctx,
        &mut st,
        "rc=1\nFAILING-BEGIN\nBUILD-FAILED\nFAILING-END\nerror: could not compile\n",
    );
    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exceptions.contains("gate-build-failed 0123456789"));
    assert!(exceptions.contains("error: could not compile"));
    let rec = st.gates.last().unwrap();
    assert_eq!(rec.names, Some(vec![String::from("BUILD-FAILED")]));
    assert_eq!(rec.new, None);
    assert_eq!(st.baseline, Some(vec![String::from("old crate::base")]));
    drop(dir);
}

#[test]
fn red_after_fix_is_reported_once_when_the_fix_units_are_settled_and_the_name_still_fails() {
    let dir = tempdir().unwrap();
    let mut cfg = gate_cfg();
    cfg.gate.max_replans = 0;
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    let name = "vivi pump::gate::still_red";
    let mut st = gate_state(4);
    st.baseline = Some(vec![String::from(name)]);
    st.red
        .insert(String::from(name), String::from("pumpfix-settled"));
    st.units.insert(
        String::from("pumpfix-settled-1"),
        Unit {
            need: String::from("pumpfix-settled"),
            status: Status::Merged,
            ..Unit::default()
        },
    );
    let text = format!("FAILING-BEGIN\n{name}\nFAILING-END\n");
    finish_gate(&ctx, &mut st, &text);
    finish_gate(&ctx, &mut st, &text);
    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert_eq!(
        exceptions.matches("red-after-fix pumpfix-settled").count(),
        1
    );
    assert!(exceptions.contains(&format!("red-after-fix pumpfix-settled {name}")));
    assert_eq!(st.red_reported, vec![format!("pumpfix-settled:{name}")]);
    drop(dir);
}

#[test]
fn hold_cycle_forces_a_gate_after_the_fixes_merge_and_lifts_the_hold_on_a_clean_gate() {
    let (dir, ctx, host) = setup();
    let hold = ctx.home.join("HOLD-MERGE");
    fs::write(&hold, "hold\n").unwrap();
    set_mtime(&hold, 500);
    let mut st = State::default();
    st.units.insert(
        String::from("pumpfix-a-1"),
        Unit {
            status: Status::Merging,
            ..Unit::default()
        },
    );
    hold_cycle(&ctx, &mut st);
    assert!(!ctx.flag("FORCE-GATE"));
    st.units.insert(
        String::from("pumpfix-a-1"),
        Unit {
            status: Status::Merged,
            since: T0 + 50,
            ..Unit::default()
        },
    );
    st.gates.push(GateRecord {
        tip: String::from(TIP),
        at: T0,
        merges: 2,
        names: Some(vec![String::from("vivi pump::gate::red")]),
        new: Some(vec![String::from("vivi pump::gate::red")]),
    });
    hold_cycle(&ctx, &mut st);
    let force = ctx.home.join("FORCE-GATE");
    assert_eq!(fs::read_to_string(&force).unwrap(), "after fixes\n");
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("hold: fixes merged, forcing a gate"))
    );
    fs::remove_file(&force).unwrap();
    st.gates.push(GateRecord {
        tip: String::from(TIP),
        at: T0 + 100,
        merges: 3,
        names: Some(vec![]),
        new: None,
    });
    hold_cycle(&ctx, &mut st);
    assert!(!hold.exists());
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("hold lifted: gate at 0123456789 is clean"))
    );
    drop(dir);
}

#[test]
fn a_red_gate_never_stops_the_merge_queue() {
    let (dir, ctx, _host) = setup();
    let mut st = gate_state(4);
    add_merged_units(&mut st);
    st.units.insert(
        String::from("u5"),
        Unit {
            status: Status::Inflight,
            ..Unit::default()
        },
    );
    st.order.push(String::from("u5"));
    let before = statuses(&st);
    finish_gate(
        &ctx,
        &mut st,
        "FAILING-BEGIN\nvivi pump::gate::boom\nFAILING-END\n",
    );
    assert_eq!(statuses(&st), before);
    assert!(!st.gates.is_empty());
    drop(dir);
}

/// Replies that make a promote succeed, and a state that owns `crate::c`.
fn owned_red_state() -> State {
    let mut st = gate_state(4);
    st.baseline = Some(vec![String::from("old")]);
    st.red
        .insert(String::from("crate::c"), String::from("pumpfix-gabcdef01"));
    st
}

#[test]
fn promote_is_held_while_an_owned_failure_is_still_red() {
    let (dir, ctx, host) = setup();
    wire_promote(&host);
    let mut st = owned_red_state();
    finish_gate(&ctx, &mut st, "FAILING-BEGIN\nold\ncrate::c\nFAILING-END\n");
    assert!(host.commands().iter().all(|c| !c.contains("git push")));
    assert!(st.promotions.is_empty());
    assert!(st.red.contains_key("crate::c"));
    assert!(host.logs().iter().any(|l| {
        l.contains(
            "promote held: 1 failures not on main at 0123456789 (main never gets redder): crate::c",
        )
    }));
    assert_eq!(st.gates.last().unwrap().new, Some(Vec::new()));
    drop(dir);
}

#[test]
fn promote_goes_once_the_owned_failure_is_gone_from_the_gate() {
    let (dir, ctx, host) = setup();
    wire_promote(&host);
    let mut st = owned_red_state();
    finish_gate(&ctx, &mut st, "FAILING-BEGIN\nold\nFAILING-END\n");
    assert!(st.red.is_empty());
    assert_eq!(st.promotions.len(), 1);
    assert_eq!(st.promotions[0].tip, TIP);
    assert!(host.logs().iter().all(|l| !l.contains("promote held")));
    drop(dir);
}

/// A context whose gate takes external results from `dir/results` and waits
/// 600 s for one before a pool gate.
fn external_setup() -> (tempfile::TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let mut cfg = gate_cfg();
    cfg.gate.results_dir = Some(dir.path().join("results").display().to_string());
    cfg.gate.results_wait_seconds = 600;
    let (ctx, host) = test_ctx(dir.path(), cfg);
    (dir, ctx, host)
}

/// Write an external result for `tip` into the results directory.
fn write_result(ctx: &Ctx, tip: &str, body: &str) -> std::path::PathBuf {
    let dir = ctx.cfg.results_dir(&ctx.project).unwrap();
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{tip}.json"));
    fs::write(&path, body).unwrap();
    path
}

#[test]
fn an_external_result_for_the_tip_finishes_the_gate_and_promotes() {
    let (dir, ctx, host) = external_setup();
    wire_promote(&host);
    let mut st = gate_state(6);
    st.last_gate.task = None;
    st.baseline = Some(vec![String::from("old")]);
    let body = format!(
        r#"{{"tip": "{TIP}", "host": "runner-1", "seconds": 412, "rc": 0, "names": ["old"], "build_failed": false, "tail": ""}}"#
    );
    let path = write_result(&ctx, TIP, &body);
    external_gates(&ctx, &mut st);
    assert!(!path.exists());
    assert!(
        path.parent()
            .unwrap()
            .join(format!("done/{TIP}.json"))
            .exists()
    );
    assert_eq!(st.gates.len(), 1);
    assert_eq!(st.gates[0].tip, TIP);
    assert_eq!(st.gates[0].merges, 6);
    assert_eq!(st.last_gate.task, None);
    assert_eq!(st.last_gate.pool.as_deref(), Some("external"));
    assert_eq!(st.promotions.len(), 1);
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("external gate at 0123456789 (412s) host=runner-1"))
    );
    drop(dir);
}

#[test]
fn an_external_result_holds_promotion_while_an_owned_failure_is_red() {
    let (dir, ctx, host) = external_setup();
    wire_promote(&host);
    let mut st = owned_red_state();
    st.last_gate.task = None;
    let body = format!(
        r#"{{"tip": "{TIP}", "seconds": 1, "names": ["old", "crate::c"], "build_failed": false}}"#
    );
    write_result(&ctx, TIP, &body);
    external_gates(&ctx, &mut st);
    assert!(st.promotions.is_empty());
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("promote held: 1 failures not on main"))
    );
    drop(dir);
}

#[test]
fn an_external_build_failure_raises_gate_build_failed() {
    let (dir, ctx, host) = external_setup();
    wire_promote(&host);
    let mut st = fresh_state();
    let body = format!(
        r#"{{"tip": "{TIP}", "names": null, "build_failed": true, "tail": "error: could not compile"}}"#
    );
    write_result(&ctx, TIP, &body);
    external_gates(&ctx, &mut st);
    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exceptions.contains("gate-build-failed 0123456789"));
    assert!(exceptions.contains("error: could not compile"));
    assert!(st.promotions.is_empty());
    drop(dir);
}

#[test]
fn an_unparseable_external_result_is_set_aside_once_and_no_dir_means_no_intake() {
    let (dir, ctx, host) = external_setup();
    let mut st = fresh_state();
    let path = write_result(&ctx, "bad", "{not json");
    external_gates(&ctx, &mut st);
    assert!(!path.exists());
    assert!(
        path.parent()
            .unwrap()
            .join("bad")
            .join(path.file_name().unwrap())
            .exists()
    );
    assert!(st.gates.is_empty());
    external_gates(&ctx, &mut st);
    let noted = host
        .logs()
        .iter()
        .filter(|line| line.contains("unreadable"))
        .count();
    assert_eq!(noted, 1, "{:?}", host.logs());

    let (dir2, plain, _host2) = setup();
    let stray = plain.home.join("gate-results");
    fs::create_dir_all(&stray).unwrap();
    fs::write(
        stray.join(format!("{TIP}.json")),
        r#"{"tip": "x", "names": []}"#,
    )
    .unwrap();
    external_gates(&plain, &mut st);
    assert!(st.gates.is_empty());
    drop((dir, dir2));
}

#[test]
fn a_due_gate_waits_for_an_external_result_then_falls_back_to_a_pool() {
    let (dir, ctx, host) = external_setup();
    wire(&host);
    let mut st = State {
        baseline: None,
        ..fresh_state()
    };
    start_gate(&ctx, &mut st);
    assert!(host.commands().is_empty());
    assert_eq!(
        st.extra["external_wait_since"],
        serde_json::json!(ctx.now())
    );

    host.advance(599);
    start_gate(&ctx, &mut st);
    assert!(host.commands().is_empty());

    host.advance(1);
    start_gate(&ctx, &mut st);
    assert_eq!(st.last_gate.task.as_deref(), Some(TASK));
    assert_eq!(st.last_gate.pool.as_deref(), Some("flygate"));
    assert!(!st.extra.contains_key("external_wait_since"));
    drop(dir);
}

/// The old plan's owner in the re-plan tests.
const OLD_OWNER: &str = "pumpfix-gold";
/// A residual failing name.
const RESIDUAL: &str = "crate::residual";
/// A gate reply listing the baseline name and [`RESIDUAL`].
const RESIDUAL_REPLY: &str = "FAILING-BEGIN\nold crate::base\ncrate::residual\nFAILING-END\n";

/// A state where [`RESIDUAL`] is owned by [`OLD_OWNER`], whose one unit has
/// the given status.
fn residual_state(unit: Status) -> State {
    let mut st = gate_state(4);
    st.red
        .insert(String::from(RESIDUAL), String::from(OLD_OWNER));
    st.units.insert(
        format!("{OLD_OWNER}-1"),
        Unit {
            need: String::from(OLD_OWNER),
            status: unit,
            ..Unit::default()
        },
    );
    st
}

/// A context with a planner and promotion wired.
fn residual_setup(max_replans: u32) -> (tempfile::TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let mut cfg = planner_cfg();
    cfg.gate.max_replans = max_replans;
    let (ctx, host) = test_ctx(dir.path(), cfg);
    wire_promote(&host);
    (dir, ctx, host)
}

#[test]
fn residual_failures_of_a_settled_owner_are_replanned_and_stay_owned() {
    let (dir, ctx, host) = residual_setup(2);
    // A settled plan's file exists in the pump home, as it does in reality.
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    fs::write(ctx.home.join("plans/pumpfix-gold.json"), "{}").unwrap();
    let mut st = residual_state(Status::Merged);
    finish_gate(&ctx, &mut st, RESIDUAL_REPLY);
    assert_eq!(
        st.red.get(RESIDUAL),
        Some(&String::from("pumpfix-g01234567"))
    );
    assert_eq!(st.extra["replans"][RESIDUAL], 1);
    assert_eq!(host.spawned().len(), 1);
    let prompt = fs::read_to_string(ctx.home.join("gatefix/pumpfix-g01234567.prompt")).unwrap();
    assert!(prompt.contains(&format!("owned by fix plan {OLD_OWNER}")));
    assert!(prompt.contains(&format!("still fail at commit {TIP}")));
    assert!(prompt.contains("- crate::residual"));
    assert!(prompt.contains(&format!("log --grep {OLD_OWNER}")));
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("replan: 1 residual failures of pumpfix-gold"))
    );
    assert!(host.commands().iter().all(|c| !c.contains("git push")));
    assert!(st.promotions.is_empty());
    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap_or_default();
    assert!(!exceptions.contains("red-after-fix"));
    drop(dir);
}

#[test]
fn units_of_a_real_handle_need_settle_their_owner_even_when_uids_differ() {
    let (dir, ctx, host) = residual_setup(2);
    host.reply("need send", 0, "created operator ef012345\nsent ef012345\n");
    let mut st = gate_state(4);
    st.red
        .insert(String::from(RESIDUAL), String::from("ab12cd34"));
    st.units.insert(
        String::from("pumpfix-g01234567-1"),
        Unit {
            need: String::from("ab12cd34"),
            status: Status::Merged,
            ..Unit::default()
        },
    );
    finish_gate(&ctx, &mut st, RESIDUAL_REPLY);
    assert_eq!(st.red.get(RESIDUAL), Some(&String::from("ef012345")));
    assert_eq!(st.extra["replans"][RESIDUAL], 1);
    assert_eq!(host.spawned().len(), 1);
    drop(dir);
}

#[test]
fn residual_planning_takes_the_next_free_id_when_the_tip_plan_exists() {
    let (dir, ctx, host) = residual_setup(2);
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    fs::write(ctx.home.join("plans/pumpfix-g01234567.json"), "{}").unwrap();
    let mut st = residual_state(Status::Stuck);
    finish_gate(&ctx, &mut st, RESIDUAL_REPLY);
    assert_eq!(
        st.red.get(RESIDUAL),
        Some(&String::from("pumpfix-g01234567-r1"))
    );
    assert_eq!(host.spawned().len(), 1);
    assert!(
        ctx.home
            .join("gatefix/pumpfix-g01234567-r1.prompt")
            .exists()
    );
    drop(dir);
}

#[test]
fn new_and_residual_failures_are_planned_together() {
    let (dir, ctx, host) = residual_setup(2);
    let mut st = residual_state(Status::Merged);
    finish_gate(
        &ctx,
        &mut st,
        "FAILING-BEGIN\nold crate::base\ncrate::residual\ncrate::fresh\nFAILING-END\n",
    );
    assert_eq!(host.spawned().len(), 1);
    assert_eq!(
        st.red.get("crate::fresh"),
        st.red.get(RESIDUAL),
        "one need owns both"
    );
    let prompt = fs::read_to_string(ctx.home.join("gatefix/pumpfix-g01234567.prompt")).unwrap();
    assert!(prompt.contains("- crate::fresh"));
    assert!(prompt.contains(&format!("owned by fix plan {OLD_OWNER}")));
    drop(dir);
}

#[test]
fn residual_failures_stop_replanning_at_max_replans_and_raise_red_after_fix() {
    let (dir, ctx, host) = residual_setup(1);
    let mut st = residual_state(Status::Merged);
    st.extra
        .insert(String::from("replans"), serde_json::json!({ RESIDUAL: 1 }));
    finish_gate(&ctx, &mut st, RESIDUAL_REPLY);
    assert!(host.spawned().is_empty());
    assert_eq!(st.red.get(RESIDUAL), Some(&String::from(OLD_OWNER)));
    assert_eq!(st.extra["replans"][RESIDUAL], 1);
    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exceptions.contains(&format!("red-after-fix {OLD_OWNER} {RESIDUAL}")));
    assert!(st.promotions.is_empty());
    drop(dir);
}

#[test]
fn zero_max_replans_never_replans() {
    let (dir, ctx, host) = residual_setup(0);
    let mut st = residual_state(Status::Merged);
    finish_gate(&ctx, &mut st, RESIDUAL_REPLY);
    assert!(host.spawned().is_empty());
    assert_eq!(st.red.get(RESIDUAL), Some(&String::from(OLD_OWNER)));
    drop(dir);
}

#[test]
fn an_unsettled_owner_is_not_replanned() {
    let (dir, ctx, host) = residual_setup(2);
    let mut st = residual_state(Status::Inflight);
    finish_gate(&ctx, &mut st, RESIDUAL_REPLY);
    assert!(host.spawned().is_empty());
    assert_eq!(st.red.get(RESIDUAL), Some(&String::from(OLD_OWNER)));
    assert!(!st.extra.contains_key("replans"));
    drop(dir);
}

#[test]
fn residual_failures_replan_orphaned_red_owners() {
    let (dir, ctx, host) = residual_setup(2);
    let mut st = gate_state(4);
    st.red
        .insert(String::from(RESIDUAL), String::from(OLD_OWNER));
    finish_gate(&ctx, &mut st, RESIDUAL_REPLY);
    assert_eq!(host.spawned().len(), 1, "a fresh gate-fix planner starts");
    assert_eq!(
        st.red.get(RESIDUAL),
        Some(&String::from("pumpfix-g01234567")),
        "the new gate-fix need re-owns the name"
    );
    assert_eq!(st.extra["replans"][RESIDUAL], 1);
    let prompt = fs::read_to_string(ctx.home.join("gatefix/pumpfix-g01234567.prompt")).unwrap();
    assert!(prompt.contains("- crate::residual"));
    assert!(prompt.contains("never wrote a plan"));
    drop(dir);
}
