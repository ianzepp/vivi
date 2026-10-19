//! Tests for [`crate::pump::gate`]: the failing-name reply parser and the
//! gate start rules, driven through [`FakeHost`] replies.

use std::fs;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tempfile::tempdir;

use crate::pump::config::{GateConfig, PumpConfig, PumpTarget};
use crate::pump::gate::{
    external_gates, finish_gate, hold_cycle, parse_failing, start_gate, target_deaths,
};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobStatus, dispatch_pending};
use crate::pump::state::{GateRecord, GateRun, State, Status, Unit};
use crate::routing::{RoleRouting, RoutedRole};

/// The fake clock's start.
const T0: u64 = 1_000_000;
/// A 40-hex remote tip.
const TIP: &str = "0123456789abcdef0123456789abcdef01234567";
/// The gate task handle the fake `task send` reports.
const TASK: &str = "1a2b3c4d";

/// A gate config with two pools; the retired `every_merges` / `every_seconds`
/// stay at their defaults (8 / 1800 s) to prove they no longer matter.
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
            job_id: None,
            source_jobs: Vec::new(),
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

/// Start a gate and let the durable jobs dispatcher assign its pool slot.
fn start_and_dispatch(ctx: &Ctx, st: &mut State) {
    start_gate(ctx, st);
    dispatch_pending(ctx, st).unwrap();
}

/// The durable job associated with the latest started gate.
fn gate_job(st: &State) -> &crate::pump::jobs::PoolJob {
    let id = st.last_gate.job_id.as_deref().expect("no gate job ID");
    st.jobs.get(id).expect("gate job is not stored")
}

/// Add the release role required for dispatched promotions.
fn with_release_role(ctx: Ctx) -> Ctx {
    ctx.with_routing(RoleRouting {
        roles: vec![RoutedRole {
            name: String::from("release"),
            kind: Some(String::from("merge")),
            status: String::from("active"),
            pool: Some(String::from("flygate")),
            read_only: None,
            model: None,
            models: Vec::new(),
        }],
        pools: std::collections::BTreeMap::new(),
    })
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
fn target_deaths_reads_the_marker_block_and_parse_failing_skips_them() {
    let mixed = "rc=101\nFAILING-BEGIN\nvivi pump::a\nTARGET-DIED radix-cli\nFAILING-END\n\
                 error: test failed\n";
    assert_eq!(
        parse_failing(mixed),
        Some(vec![String::from("vivi pump::a")])
    );
    assert_eq!(target_deaths(mixed), vec![String::from("radix-cli")]);

    let only_dead = "rc=101\nFAILING-BEGIN\n  TARGET-DIED radix-cli  \nFAILING-END\n\
                     120 passed; failures: (vivi pump::ghost)\n";
    assert_eq!(parse_failing(only_dead), Some(vec![]));
    assert_eq!(target_deaths(only_dead), vec![String::from("radix-cli")]);
}

#[test]
fn missing_tool_failures_are_environment_not_new_reds() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), gate_cfg());
    let mut st = fresh_state();
    finish_gate(
        &ctx,
        &mut st,
        "FAILING-BEGIN\nENV radix-hir-ts a::b :: node not found\nvivi pump::real\nFAILING-END\n",
    );
    let rec = st.gates.last().unwrap();
    assert_eq!(rec.names, Some(vec![String::from("vivi pump::real")]));
    assert!(!st.red.contains_key("radix-hir-ts a::b"));
    assert_eq!(
        st.red.get("vivi pump::real"),
        Some(&String::from("pumpfix-g"))
    );
    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exceptions.contains("gate-environment"));
    assert!(exceptions.contains("node not found"));
    assert!(exceptions.contains("1 new: vivi pump::real"));
    let new_line = exceptions
        .lines()
        .find(|line| line.contains("gate-new-failures"))
        .unwrap();
    assert!(new_line.contains("1 new: vivi pump::real"));
    assert!(!new_line.contains("radix-hir-ts"));
    assert!(host.spawned().is_empty(), "the gate never starts a planner");
    drop(dir);
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
fn start_gate_is_not_due_when_no_merge_landed_since_the_last_gate() {
    let (dir, ctx, host) = setup();
    let mut st = fresh_state();
    host.advance(1000);
    start_and_dispatch(&ctx, &mut st);
    assert!(host.commands().is_empty());
    assert!(st.last_gate.task.is_none());
    drop(dir);
}

#[test]
fn a_gate_is_due_after_every_landed_batch() {
    let (dir, ctx, host) = setup();
    let mut st = fresh_state();
    st.last_gate.at = ctx.now();
    start_and_dispatch(&ctx, &mut st);
    assert!(host.commands().is_empty());
    assert!(st.last_gate.task.is_none());
    st.merges = 3;
    st.last_gate.merges = 2;
    wire(&host);
    start_and_dispatch(&ctx, &mut st);
    assert!(st.last_gate.job_id.is_some());
    assert_eq!(st.last_gate.tip.as_deref(), Some(TIP));
    assert_eq!(gate_job(&st).task.as_deref(), Some(TASK));
    let dispatched: Vec<String> = host
        .commands()
        .into_iter()
        .filter(|cmd| cmd.contains("pool dispatch"))
        .collect();
    assert_eq!(dispatched.len(), 1, "one dispatch for one merge batch");
    drop(dir);
}

/// A batch landing while a gate is running must not stack a second gate:
/// the start is refused until the open gate finishes, and exactly one
/// follow-up gate then runs, at the newest tip.
#[test]
fn a_second_batch_while_a_gate_runs_coalesces_to_one_followup() {
    let (dir, ctx, host) = setup();
    let mut st = gate_state(4);
    st.merges = 5;
    start_and_dispatch(&ctx, &mut st);
    assert!(host.commands().is_empty(), "refused while the gate is open");
    assert!(st.last_gate.task.as_deref().is_some());

    st.last_gate.task = None;
    wire(&host);
    host.reply("git ls-remote", 0, &format!("{NEW_TIP}\trefs/heads/next\n"));
    start_and_dispatch(&ctx, &mut st);
    let dispatched: Vec<String> = host
        .commands()
        .into_iter()
        .filter(|cmd| cmd.contains("pool dispatch"))
        .collect();
    assert_eq!(dispatched.len(), 1, "one follow-up gate, not a stack");
    assert!(st.last_gate.job_id.is_some());
    assert_eq!(st.last_gate.tip.as_deref(), Some(NEW_TIP));
    assert_eq!(gate_job(&st).task.as_deref(), Some(TASK));
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
    start_and_dispatch(&ctx, &mut st);
    let gate = &st.last_gate;
    assert!(gate.job_id.is_some());
    assert_eq!(gate.tip.as_deref(), Some(TIP));
    assert_eq!(gate.merges, 4);
    assert_eq!(gate.at, ctx.now());
    assert_eq!(gate_job(&st).task.as_deref(), Some(TASK));
    assert_eq!(gate_job(&st).pool.as_deref(), Some("flygate"));
    let commands = host.commands();
    let send = commands.iter().find(|c| c.contains("task send")).unwrap();
    assert!(send.contains("[pump gate] stage 4b at 0123456789"));
    assert!(send.contains("--body-file"));
    let dispatch = commands
        .iter()
        .find(|c| c.contains("pool dispatch"))
        .unwrap();
    assert!(dispatch.contains(&format!("--pool flygate {TASK}")));
    assert!(dispatch.contains("--model zai/glm-5.3-flash"));
    assert!(dispatch.contains("--read-only"));
    assert!(dispatch.contains(&format!("--from {TIP} --budget 1800")));
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
        start_and_dispatch(&target_ctx, target);
    });
    assert_eq!(state.last_gate.task, None);
    let target_st = &state.target_states["radix"];
    assert!(target_st.last_gate.job_id.is_some());
    assert_eq!(state.jobs[target_st.last_gate.job_id.as_ref().unwrap()].task.as_deref(), Some(TASK));
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
    start_and_dispatch(&ctx, &mut st);
    assert!(!ctx.flag("FORCE-GATE"));
    assert!(st.last_gate.job_id.is_some());
    assert_eq!(gate_job(&st).task.as_deref(), Some(TASK));
    assert_eq!(gate_job(&st).pool.as_deref(), Some("flygate"));
    drop(dir);
}

#[test]
fn a_failed_gate_job_dispatch_stays_pending_for_retry() {
    let (dir, ctx, host) = setup();
    wire(&host);
    host.reply("pool dispatch", 1, "boom: no worker online\n");
    fs::write(ctx.home.join("FORCE-GATE"), "after fixes\n").unwrap();
    let mut st = State {
        baseline: None,
        ..fresh_state()
    };
    start_and_dispatch(&ctx, &mut st);
    let job = gate_job(&st);
    assert_eq!(job.status, crate::pump::jobs::JobStatus::Pending);
    assert_eq!(job.task.as_deref(), Some(TASK));
    assert!(st.last_gate.job_id.is_some());
    assert!(!ctx.flag("FORCE-GATE"));
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("job dispatch deferred"))
    );
    drop(dir);
}

/// A second 40-hex tip: the last clean gate.
const CLEAN: &str = "fedcba9876543210fedcba9876543210fedcba98";
/// A newer 40-hex tip that lands while a gate is running.
const NEW_TIP: &str = "9876543210987654321098765432109876543210";

/// A state with a baseline, a merge count and an open gate task at `TIP`.
fn gate_state(merges: u64) -> State {
    State {
        merges,
        baseline: Some(vec![String::from("old crate::base")]),
        last_gate: GateRun {
            at: T0,
            merges,
            task: Some(String::from(TASK)),
            job_id: None,
            source_jobs: Vec::new(),
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

/// The task-send and pool-dispatch lines of the one audit a gate started.
fn one_audit(host: &Arc<FakeHost>) -> (String, String) {
    let commands = host.commands();
    let sends: Vec<&String> = commands
        .iter()
        .filter(|cmd| cmd.contains("task send") && cmd.contains("[pump audit]"))
        .collect();
    assert_eq!(sends.len(), 1, "audit task sends: {sends:?}");
    let dispatches: Vec<&String> = commands
        .iter()
        .filter(|cmd| {
            cmd.contains("pool dispatch")
                && cmd.contains("--read-only")
                && cmd.contains("--model")
                && cmd.contains(&format!("--from {TIP}"))
        })
        .collect();
    assert_eq!(dispatches.len(), 1, "audit dispatches: {dispatches:?}");
    assert!(dispatches[0].contains(&format!("--from {TIP}")));
    assert!(
        dispatches[0].contains("--pool flygate"),
        "{}",
        dispatches[0]
    );
    ((*sends[0]).clone(), (*dispatches[0]).clone())
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
fn a_new_failure_is_owned_by_the_fix_need_and_starts_an_auditor() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), gate_cfg());
    wire(&host);
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
    dispatch_pending(&ctx, &mut st).unwrap();
    assert_eq!(
        st.red.get("vivi pump::gate::gone"),
        Some(&String::from("pumpfix-g01234567"))
    );
    assert!(host.spawned().is_empty(), "no planner process starts");
    let (send, dispatch) = one_audit(&host);
    assert!(
        send.contains("[pump audit] 1 failures at 0123456789"),
        "{send}"
    );
    assert!(dispatch.contains("--pool flygate"), "{dispatch}");
    assert!(ctx.home.join("HOLD-MERGE").is_file());
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("audit queued for pumpfix-g01234567 (1 new"))
    );
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("hold set automatically: gate red at 0123456789"))
    );
    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exceptions.contains("gate-new-failures 0123456789 1 new: vivi pump::gate::gone"));
    assert!(!exceptions.contains("suspects:"));
    drop(dir);
}

#[test]
fn a_red_gate_sends_a_real_backing_need_and_audits_under_it() {
    let dir = tempdir().unwrap();
    let mut cfg = gate_cfg();
    cfg.board.from = vec![String::from("operator")];
    let (ctx, host) = test_ctx(dir.path(), cfg);
    wire(&host);
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
    dispatch_pending(&ctx, &mut st).unwrap();
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
    assert!(host.spawned().is_empty());
    let (send, _) = one_audit(&host);
    assert!(
        send.contains("[pump audit] 1 failures at 0123456789"),
        "{send}"
    );
    assert!(ctx.home.join("HOLD-MERGE").is_file());
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("audit queued for ab12cd34 (1 new"))
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
fn a_gate_with_no_new_failure_enqueues_a_fast_forward_promotion_job() {
    let (dir, base, host) = setup();
    let ctx = with_release_role(base);
    let mut st = gate_state(4);
    finish_gate(
        &ctx,
        &mut st,
        "FAILING-BEGIN\nold crate::base\nFAILING-END\n",
    );
    let promotion = st
        .jobs
        .values()
        .find(|job| job.kind == "promote")
        .expect("green gate should enqueue promotion");
    assert_eq!(promotion.role, "release");
    assert_eq!(promotion.from_ref.as_deref(), Some(TIP));
    assert_eq!(promotion.branch.as_deref(), Some("main"));
    assert!(host.commands().iter().all(|cmd| !cmd.contains("git push")));
    assert!(st.promotions.is_empty(), "receipt has not landed yet");
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("promote queued: main -> 0123456789"))
    );
    let job_id = promotion.id.clone();
    let job = st.jobs.get_mut(&job_id).unwrap();
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: format!("pushed main {}", &TIP[..10]),
        receipt: format!("PUMP_PROMOTE_RESULT={{\"status\":\"pushed\",\"tip\":\"{TIP}\"}}"),
        at: ctx.now(),
    });
    crate::pump::gate::collect_completed(&ctx, &mut st);
    assert_eq!(st.promotions.len(), 1);
    assert_eq!(st.promotions[0].tip, TIP);
    assert_eq!(
        st.promotions[0].failing,
        Some(vec![String::from("old crate::base")])
    );
    drop(dir);
}

#[test]
fn a_non_fast_forward_decision_is_delegated_to_the_promotion_job() {
    let (dir, base, host) = setup();
    let ctx = with_release_role(base);
    let mut st = gate_state(4);
    finish_gate(&ctx, &mut st, "FAILING-BEGIN\nFAILING-END\n");
    assert!(st.jobs.values().any(|job| job.kind == "promote"));
    assert!(host.commands().iter().all(|c| !c.contains("git push")));
    assert!(st.promotions.is_empty());
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
    assert!(ctx.flag("FORCE-GATE"), "unreadable receipts are retried");
    drop(dir);
}

#[test]
fn a_build_failed_reply_raises_gate_build_failed() {
    let (dir, ctx, host) = setup();
    wire(&host);
    let mut st = gate_state(4);
    finish_gate(
        &ctx,
        &mut st,
        "rc=1\nFAILING-BEGIN\nBUILD-FAILED\nFAILING-END\nerror: could not compile\n",
    );
    dispatch_pending(&ctx, &mut st).unwrap();
    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exceptions.contains("gate-build-failed 0123456789"));
    assert!(exceptions.contains("error: could not compile"));
    let rec = st.gates.last().unwrap();
    assert_eq!(rec.names, Some(vec![String::from("BUILD-FAILED")]));
    assert_eq!(rec.new, Some(vec![String::from("BUILD-FAILED")]));
    assert_eq!(st.baseline, Some(vec![String::from("old crate::base")]));
    assert_eq!(
        st.red.get("BUILD-FAILED"),
        Some(&String::from("pumpfix-g01234567"))
    );
    assert!(
        one_audit(&host)
            .0
            .contains("[pump audit] 1 failures at 0123456789")
    );
    assert!(ctx.home.join("HOLD-MERGE").is_file());
    assert!(st.promotions.is_empty());
    drop(dir);
}

#[test]
fn a_build_failed_first_gate_is_audited_without_becoming_a_clean_baseline() {
    let (dir, ctx, host) = setup();
    wire(&host);
    let mut st = gate_state(4);
    st.baseline = None;
    finish_gate(
        &ctx,
        &mut st,
        "rc=1\nFAILING-BEGIN\nBUILD-FAILED\nFAILING-END\nerror: could not compile\n",
    );
    dispatch_pending(&ctx, &mut st).unwrap();

    assert_eq!(st.baseline, Some(Vec::new()));
    assert_eq!(
        st.gates.last().unwrap().new,
        Some(vec![String::from("BUILD-FAILED")])
    );
    assert_eq!(
        st.red.get("BUILD-FAILED"),
        Some(&String::from("pumpfix-g01234567"))
    );
    assert!(
        one_audit(&host)
            .0
            .contains("[pump audit] 1 failures at 0123456789")
    );
    assert!(st.promotions.is_empty());
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
    assert!(exceptions.contains(&format!(
        "red-after-fix pumpfix-settled {name}; audits for this name: 0; last report: {}/audit/pumpfix-settled.report",
        ctx.home.display()
    )));
    assert_eq!(st.red_reported, vec![format!("pumpfix-settled:{name}")]);
    drop(dir);
}

#[test]
fn hold_cycle_forces_a_gate_after_the_fixes_merge_and_lifts_the_hold_on_a_clean_gate() {
    let (dir, ctx, host) = setup();
    let hold = ctx.home.join("HOLD-MERGE");
    fs::write(&hold, "gate red\n").unwrap();
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
fn a_queued_gate_keeps_hold_cycle_from_lifting_a_hold() {
    let (dir, ctx, _host) = setup();
    let hold = ctx.home.join("HOLD-MERGE");
    fs::write(&hold, "gate red\n").unwrap();
    set_mtime(&hold, 500);
    let mut st = State {
        last_gate: GateRun {
            job_id: Some(String::from("gate-job")),
            ..GateRun::default()
        },
        ..State::default()
    };
    st.gates.push(GateRecord {
        tip: String::from(TIP),
        at: T0,
        merges: 1,
        names: Some(Vec::new()),
        new: Some(Vec::new()),
    });

    hold_cycle(&ctx, &mut st);

    assert!(
        hold.is_file(),
        "the queued gate has not proved a clean result"
    );
    drop(dir);
}

#[test]
fn hold_on_red_writes_hold_merge_while_reds_are_owned() {
    let dir = tempdir().unwrap();
    let mut cfg = gate_cfg();
    cfg.gate.hold_on_red = true;
    let (ctx, host) = test_ctx(dir.path(), cfg);
    let mut st = State::default();
    st.red
        .insert(String::from("vivi pump::gate::red"), String::from("u1"));
    hold_cycle(&ctx, &mut st);
    let hold = ctx.home.join("HOLD-MERGE");
    assert_eq!(fs::read_to_string(&hold).unwrap(), "gate red\n");
    assert!(
        host.logs().iter().any(|line| {
            line.contains("hold on red: 1 owned names unfixed; HOLD-MERGE written")
        })
    );
    set_mtime(&hold, 500);
    hold_cycle(&ctx, &mut st);
    assert_eq!(
        fs::metadata(&hold).unwrap().modified().unwrap(),
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(500)
    );
    drop(dir);
}

#[test]
fn a_clean_gate_preserves_an_operator_owned_hold() {
    let (dir, ctx, _host) = setup();
    let hold = ctx.home.join("HOLD-MERGE");
    fs::write(&hold, "operator hold\n").unwrap();
    set_mtime(&hold, 500);
    let mut st = State::default();
    st.gates.push(GateRecord {
        tip: String::from(TIP),
        at: T0,
        merges: 1,
        names: Some(vec![]),
        new: Some(vec![]),
    });

    hold_cycle(&ctx, &mut st);

    assert_eq!(fs::read_to_string(&hold).unwrap(), "operator hold\n");
    drop(dir);
}

#[test]
fn a_red_gate_does_not_overwrite_an_operator_owned_hold() {
    let (dir, ctx, _host) = setup();
    let hold = ctx.home.join("HOLD-MERGE");
    fs::write(&hold, "operator hold\n").unwrap();
    let mut st = gate_state(4);
    finish_gate(
        &ctx,
        &mut st,
        "FAILING-BEGIN\nfresh crate::red\nFAILING-END\n",
    );
    assert_eq!(fs::read_to_string(&hold).unwrap(), "operator hold\n");
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

#[test]
fn a_dead_target_keeps_its_owned_names_red() {
    let (dir, ctx, host) = setup();
    let mut st = gate_state(4);
    st.baseline = Some(vec![String::from("old")]);
    st.red.insert(
        String::from("radix-cli a::b"),
        String::from("pumpfix-g01234567"),
    );
    finish_gate(
        &ctx,
        &mut st,
        "FAILING-BEGIN\nvivi pump::x\nTARGET-DIED radix-cli\nFAILING-END\n",
    );
    assert_eq!(
        st.red.get("radix-cli a::b"),
        Some(&String::from("pumpfix-g01234567"))
    );
    assert!(
        host.logs()
            .iter()
            .all(|l| !l.contains("red fixed: radix-cli a::b"))
    );
    assert!(host.logs().iter().any(|l| {
        l.contains("gate-target-died: radix-cli kept 1 owned reds (target died, not fixed)")
    }));
    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap_or_default();
    assert!(exceptions.contains("gate-target-died"));
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

/// A context whose gate takes external results from `dir/results` and waits
/// 300 s for one, so a heartbeat decides whether that wait happens at all.
fn heartbeat_setup() -> (tempfile::TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let mut cfg = gate_cfg();
    cfg.gate.results_dir = Some(dir.path().join("results").display().to_string());
    cfg.gate.results_wait_seconds = 300;
    let (ctx, host) = test_ctx(dir.path(), cfg);
    (dir, ctx, host)
}

/// Write the external runner's `heartbeat` file, as a live runner would.
fn write_heartbeat(ctx: &Ctx) -> std::path::PathBuf {
    let dir = ctx.cfg.results_dir(&ctx.project).unwrap();
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("heartbeat");
    fs::write(&path, "alive\n").unwrap();
    path
}

/// The real wall clock in epoch seconds, for tests that compare a file mtime
/// against the fake clock.
fn real_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |span| span.as_secs())
}

#[test]
fn no_heartbeat_means_no_external_wait() {
    let (dir, ctx, host) = heartbeat_setup();
    fs::create_dir_all(ctx.cfg.results_dir(&ctx.project).unwrap()).unwrap();
    host.set_now(real_now());
    wire(&host);
    let mut st = State {
        baseline: None,
        ..fresh_state()
    };
    start_and_dispatch(&ctx, &mut st);
    assert!(!st.extra.contains_key("external_wait_since"));
    assert!(st.last_gate.job_id.is_some());
    assert_eq!(gate_job(&st).task.as_deref(), Some(TASK));
    assert!(host.logs().iter().any(|line| {
        line.contains("external gate runner heartbeat missing; not waiting for results_dir")
    }));
    drop(dir);
}

#[test]
fn a_fresh_heartbeat_waits() {
    let (dir, ctx, host) = heartbeat_setup();
    write_heartbeat(&ctx);
    host.set_now(real_now());
    wire(&host);
    let mut st = State {
        baseline: None,
        ..fresh_state()
    };
    start_and_dispatch(&ctx, &mut st);
    assert!(host.commands().is_empty());
    assert_eq!(
        st.extra["external_wait_since"],
        serde_json::json!(ctx.now())
    );
    drop(dir);
}

#[test]
fn a_stale_heartbeat_does_not_wait() {
    let (dir, ctx, host) = heartbeat_setup();
    let path = write_heartbeat(&ctx);
    let file = fs::File::options().write(true).open(&path).unwrap();
    let aged = SystemTime::now() - Duration::from_secs(1200);
    file.set_times(fs::FileTimes::new().set_modified(aged))
        .unwrap();
    host.set_now(real_now());
    wire(&host);
    let mut st = State {
        baseline: None,
        ..fresh_state()
    };
    st.extra.insert(
        String::from("external_wait_since"),
        serde_json::json!(ctx.now()),
    );
    start_and_dispatch(&ctx, &mut st);
    assert!(!st.extra.contains_key("external_wait_since"));
    assert!(st.last_gate.job_id.is_some());
    assert_eq!(gate_job(&st).task.as_deref(), Some(TASK));
    assert!(host.logs().iter().any(|line| {
        line.contains("external gate runner heartbeat stale; not waiting for results_dir")
    }));
    drop(dir);
}

#[test]
fn an_idle_external_runner_does_not_hold_the_gate() {
    // A context with an external wait far longer than the idle window.
    let idle_setup = |last: u64| {
        let dir = tempdir().unwrap();
        let mut cfg = gate_cfg();
        cfg.gate.results_dir = Some(dir.path().join("results").display().to_string());
        cfg.gate.results_wait_seconds = 2700;
        cfg.gate.results_idle_seconds = 3600;
        let (ctx, host) = test_ctx(dir.path(), cfg);
        let now = real_now();
        write_heartbeat(&ctx);
        host.set_now(now);
        wire(&host);
        let mut st = State {
            baseline: None,
            ..fresh_state()
        };
        st.extra.insert(
            String::from("last_external_result_at"),
            serde_json::json!(now - last),
        );
        (dir, ctx, host, st)
    };

    // Control: a result 60 s ago means a runner is alive, so the gate waits.
    let (dir, ctx, host, mut st) = idle_setup(60);
    start_and_dispatch(&ctx, &mut st);
    assert!(host.commands().is_empty());
    assert!(st.last_gate.task.is_none());
    assert_eq!(
        st.extra["external_wait_since"],
        serde_json::json!(ctx.now())
    );

    // No result for 7200 s (past the 3600 s idle window): a runner that has
    // produced nothing is not running, so the pool gate goes out at once.
    let (idle_dir, idle, idle_host, mut idle_st) = idle_setup(7200);
    start_and_dispatch(&idle, &mut idle_st);
    assert!(!idle_st.extra.contains_key("external_wait_since"));
    assert!(idle_st.last_gate.job_id.is_some());
    assert_eq!(gate_job(&idle_st).task.as_deref(), Some(TASK));
    assert_eq!(gate_job(&idle_st).pool.as_deref(), Some("flygate"));
    assert!(
        idle_host
            .logs()
            .iter()
            .any(|line| line.contains("external gate runner idle over 3600s"))
    );
    drop((dir, idle_dir));
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
fn an_external_result_waits_while_a_durable_gate_job_is_open() {
    let (dir, ctx, _host) = external_setup();
    let body = format!(r#"{{"tip": "{TIP}", "seconds": 1, "names": [], "build_failed": false}}"#);
    let path = write_result(&ctx, TIP, &body);
    let mut st = gate_state(6);
    st.last_gate.task = None;
    st.last_gate.job_id = Some(String::from("gate-job"));

    external_gates(&ctx, &mut st);

    assert!(path.exists(), "the pool gate still owns this result slot");
    assert!(st.gates.is_empty());
    assert_eq!(st.last_gate.job_id.as_deref(), Some("gate-job"));
    drop(dir);
}

#[test]
fn a_duplicate_external_tip_does_not_finish_a_second_gate() {
    let (dir, ctx, host) = external_setup();
    let body = format!(r#"{{"tip": "{TIP}", "seconds": 1, "names": [], "build_failed": false}}"#);
    let path = write_result(&ctx, TIP, &body);
    let mut st = gate_state(6);
    st.last_gate.task = None;
    st.gates.push(GateRecord {
        tip: String::from(TIP),
        at: T0,
        merges: 6,
        names: Some(Vec::new()),
        new: Some(Vec::new()),
    });

    external_gates(&ctx, &mut st);

    assert!(!path.exists());
    assert!(
        path.parent()
            .unwrap()
            .join(format!("done/{TIP}.json"))
            .exists()
    );
    assert_eq!(st.gates.len(), 1);
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("duplicate ignored"))
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
    write_heartbeat(&ctx);
    host.set_now(real_now());
    wire(&host);
    let mut st = State {
        baseline: None,
        ..fresh_state()
    };
    start_and_dispatch(&ctx, &mut st);
    assert!(host.commands().is_empty());
    assert_eq!(
        st.extra["external_wait_since"],
        serde_json::json!(ctx.now())
    );

    // The wait window is `results_wait_seconds` (600) and the heartbeat stays
    // fresh for twice that; a gate still waiting inside the window is quiet.
    host.advance(599);
    start_and_dispatch(&ctx, &mut st);
    assert!(host.commands().is_empty());

    // Past twice the window the heartbeat is stale: the pool gate goes out.
    host.advance(602);
    start_and_dispatch(&ctx, &mut st);
    assert!(st.last_gate.job_id.is_some());
    assert_eq!(gate_job(&st).task.as_deref(), Some(TASK));
    assert_eq!(gate_job(&st).pool.as_deref(), Some("flygate"));
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

/// A context with the audit pool and promotion wired.
fn residual_setup(max_replans: u32) -> (tempfile::TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let mut cfg = gate_cfg();
    cfg.gate.max_replans = max_replans;
    let (ctx, host) = test_ctx(dir.path(), cfg);
    wire(&host);
    wire_promote(&host);
    (dir, ctx, host)
}

/// Record an existing owner plan in the pump home.
fn write_owner_plan(ctx: &Ctx, owner: &str) {
    let plans = ctx.home.join("plans");
    fs::create_dir_all(&plans).unwrap();
    fs::write(plans.join(format!("{owner}.json")), "{}").unwrap();
}

#[test]
fn residual_failures_of_a_settled_owner_are_handed_to_the_audit_need() {
    let (dir, ctx, host) = residual_setup(2);
    write_owner_plan(&ctx, OLD_OWNER);
    fs::create_dir_all(ctx.home.join("audit")).unwrap();
    fs::write(
        ctx.home.join("audit").join(format!("{OLD_OWNER}.report")),
        "prior cause: off-by-one in the bounds check",
    )
    .unwrap();
    let mut st = residual_state(Status::Merged);
    finish_gate(&ctx, &mut st, RESIDUAL_REPLY);
    dispatch_pending(&ctx, &mut st).unwrap();
    assert_eq!(
        st.red.get(RESIDUAL),
        Some(&String::from("pumpfix-g01234567"))
    );
    assert_eq!(st.extra["replans"][RESIDUAL], 1);
    assert!(host.spawned().is_empty(), "no planner process starts");
    let (send, _) = one_audit(&host);
    assert!(
        send.contains("[pump audit] 1 failures at 0123456789"),
        "{send}"
    );
    let brief = fs::read_to_string(ctx.home.join("briefs/audit.txt")).unwrap();
    assert!(brief.contains("prior cause: off-by-one in the bounds check"));
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("replan: 1 residual failures of pumpfix-gold"))
    );
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("audit queued for pumpfix-g01234567 (0 new, 1 residual)"))
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
    write_owner_plan(&ctx, "ab12cd34");
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
    dispatch_pending(&ctx, &mut st).unwrap();
    assert_eq!(st.red.get(RESIDUAL), Some(&String::from("ef012345")));
    assert_eq!(st.extra["replans"][RESIDUAL], 1);
    assert!(host.spawned().is_empty());
    let (send, _) = one_audit(&host);
    assert!(
        send.contains("[pump audit] 1 failures at 0123456789"),
        "{send}"
    );
    drop(dir);
}

#[test]
fn residual_auditing_uses_the_fix_need_even_when_the_tip_plan_exists() {
    let (dir, ctx, host) = residual_setup(2);
    write_owner_plan(&ctx, OLD_OWNER);
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    fs::write(ctx.home.join("plans/pumpfix-g01234567.json"), "{}").unwrap();
    let mut st = residual_state(Status::Stuck);
    finish_gate(&ctx, &mut st, RESIDUAL_REPLY);
    dispatch_pending(&ctx, &mut st).unwrap();
    assert_eq!(
        st.red.get(RESIDUAL),
        Some(&String::from("pumpfix-g01234567"))
    );
    assert!(host.spawned().is_empty());
    let (_, dispatch) = one_audit(&host);
    assert!(dispatch.contains("--pool flygate"), "{dispatch}");
    drop(dir);
}

#[test]
fn new_and_residual_failures_are_audited_together() {
    let (dir, ctx, host) = residual_setup(2);
    write_owner_plan(&ctx, OLD_OWNER);
    let mut st = residual_state(Status::Merged);
    finish_gate(
        &ctx,
        &mut st,
        "FAILING-BEGIN\nold crate::base\ncrate::residual\ncrate::fresh\nFAILING-END\n",
    );
    dispatch_pending(&ctx, &mut st).unwrap();
    assert!(host.spawned().is_empty());
    assert_eq!(
        st.red.get("crate::fresh"),
        st.red.get(RESIDUAL),
        "one need owns both"
    );
    let (send, _) = one_audit(&host);
    assert!(
        send.contains("[pump audit] 2 failures at 0123456789"),
        "{send}"
    );
    assert_eq!(
        st.red.get("crate::fresh"),
        Some(&String::from("pumpfix-g01234567"))
    );
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("audit queued for pumpfix-g01234567 (1 new, 1 residual)"))
    );
    drop(dir);
}

#[test]
fn residual_failures_stop_replanning_at_max_replans_and_raise_red_after_fix() {
    let (dir, ctx, host) = residual_setup(1);
    let mut st = residual_state(Status::Merged);
    st.extra
        .insert(String::from("replans"), serde_json::json!({ RESIDUAL: 1 }));
    finish_gate(&ctx, &mut st, RESIDUAL_REPLY);
    dispatch_pending(&ctx, &mut st).unwrap();
    assert!(host.spawned().is_empty());
    assert_eq!(st.red.get(RESIDUAL), Some(&String::from(OLD_OWNER)));
    assert_eq!(st.extra["replans"][RESIDUAL], 1);
    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exceptions.contains(&format!("red-after-fix {OLD_OWNER} {RESIDUAL}")));
    assert!(exceptions.contains("audits for this name: 1"));
    assert!(exceptions.contains("/audit/pumpfix-gold.report"));
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
fn residual_failures_are_handed_to_the_audit_for_orphaned_red_owners() {
    let (dir, ctx, host) = residual_setup(2);
    let mut st = gate_state(4);
    st.red
        .insert(String::from(RESIDUAL), String::from(OLD_OWNER));
    finish_gate(&ctx, &mut st, RESIDUAL_REPLY);
    dispatch_pending(&ctx, &mut st).unwrap();
    assert!(host.spawned().is_empty());
    assert_eq!(
        st.red.get(RESIDUAL),
        Some(&String::from("pumpfix-g01234567")),
        "the new gate-fix need re-owns the name"
    );
    assert_eq!(st.extra["replans"][RESIDUAL], 1);
    let (send, _) = one_audit(&host);
    assert!(
        send.contains("[pump audit] 1 failures at 0123456789"),
        "{send}"
    );
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
