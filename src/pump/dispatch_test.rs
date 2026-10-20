//! Tests for [`crate::pump::dispatch`]: the handle parser, the slot counter,
//! and the dispatch state machine driven through [`FakeHost`] replies.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use tempfile::tempdir;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::pump::config::{DispatchConfig, Kind, Lane, PumpConfig, PumpTarget};
use crate::pump::dispatch::*;
use crate::pump::host::{CmdOutput, Ctx, FakeHost, Host, test_ctx};
use crate::pump::state::{Attempt, EnvPause, State, Status, Unit};
use crate::routing::{RoleRouting, RoutedPool, RoutedRole};

/// The fake clock's start; units are created "now".
const T0: u64 = 1_000_000;

/// A kind dispatching to `pool`, capped at `max` units in flight.
fn kind(pool: &str, max: Option<u32>) -> Kind {
    kind_of_pools(&[pool], max)
}

/// A kind dispatching to several `pools`, capped at `max` units in flight.
fn kind_of_pools(pools: &[&str], max: Option<u32>) -> Kind {
    Kind {
        pools: pools.iter().map(|p| (*p).to_owned()).collect(),
        max,
        ..Kind::default()
    }
}

/// A dispatch config with two lanes, both pools and the default per-tick cap.
fn dispatch_cfg() -> PumpConfig {
    PumpConfig {
        branch: String::from("next"),
        dispatch: DispatchConfig {
            lanes: vec![
                Lane {
                    model: String::from("m/a"),
                    cap: 2,
                },
                Lane {
                    model: String::from("m/b"),
                    cap: 2,
                },
            ],
            last_resort: String::from("m/last"),
            kinds: BTreeMap::from([
                (String::from("plain"), kind("fly4", None)),
                (String::from("full"), kind("flyfull", None)),
            ]),
            default_kind: String::from("plain"),
            ..DispatchConfig::default()
        },
        ..PumpConfig::default()
    }
}

/// A config like [`dispatch_cfg`] with `per_tick` overridden.
fn cfg_with(per_tick: u32) -> PumpConfig {
    let mut cfg = dispatch_cfg();
    cfg.dispatch.per_tick = per_tick;
    cfg
}

/// A context with the given config, home at `dir/home`.
fn setup_with(cfg: PumpConfig) -> (tempfile::TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg);
    (dir, ctx, host)
}

/// A context with [`dispatch_cfg`].
fn setup() -> (tempfile::TempDir, Ctx, Arc<FakeHost>) {
    setup_with(dispatch_cfg())
}

/// The standard happy-path replies: one free slot per pool, a task send that
/// reports the handle, and a successful pool dispatch.
fn wire(host: &Arc<FakeHost>) {
    host.reply(
        "pool status",
        0,
        "slot fly4 bridge slot-1 free\nslot fly4 bridge slot-2 free\n\
         slot flyfull bridge slot-9 free\n",
    );
    host.reply(
        "task send",
        0,
        "created hand 1a2b3c4d\nnode 1a2b3c4d\nsent 5e6f7a8b",
    );
    host.reply(
        "pool dispatch",
        0,
        "dispatched 1a2b3c4d bridge slot-1 m/a unit/x-a1",
    );
}

/// A pending plain-tools unit titled `Do <uid>` with its own write scope.
fn pending_unit(uid: &str) -> Unit {
    Unit {
        need: String::from("n1"),
        title: format!("Do {uid}"),
        brief: String::from("the body"),
        write_scope: vec![format!("src/{uid}/")],
        tools: String::from("plain"),
        since: T0,
        ..Unit::default()
    }
}

/// Put `uids` on `st.order` and insert a [`pending_unit`] for each.
fn ordered(st: &mut State, uids: &[&str]) {
    st.order = uids.iter().map(|uid| (*uid).to_owned()).collect();
    for uid in uids {
        st.units
            .entry((*uid).to_owned())
            .or_insert_with(|| pending_unit(uid));
    }
}

/// Every recorded command line containing `fragment`.
fn commands_with(host: &Arc<FakeHost>, fragment: &str) -> Vec<String> {
    host.commands()
        .into_iter()
        .filter(|cmd| cmd.contains(fragment))
        .collect()
}

#[test]
fn parse_created_handle_reads_node_and_created_lines_and_ignores_sent() {
    let out = "created hand 1a2b3c4d\nnode 1a2b3c4d\nsent 5e6f7a8b";
    assert_eq!(parse_created_handle(out).as_deref(), Some("1a2b3c4d"));
    assert_eq!(
        parse_created_handle("sent 5e6f7a8b\nnode beef1234\n").as_deref(),
        Some("beef1234")
    );
    assert_eq!(
        parse_created_handle("created 1a2b3c4d").as_deref(),
        Some("1a2b3c4d")
    );
    assert_eq!(parse_created_handle("created 1a2b3c4"), None);
    assert_eq!(parse_created_handle("node ABCD1234"), None);
    assert_eq!(parse_created_handle("node 1a2b3c4d extra 12345678"), None);
    assert_eq!(parse_created_handle("sent 1a2b3c4d"), None);
    assert_eq!(parse_created_handle("createdx 1a2b3c4d"), None);
    assert_eq!(parse_created_handle(""), None);
}

#[test]
fn slots_counts_free_slots_per_pool() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool status",
        0,
        "slot fly4 bridge slot-1 free\nslot fly4 bridge slot-2 busy\n\
         slot fly4 bridge slot-3 free\nslot flyfull bridge slot-9 free\nnoise line here",
    );
    let free = slots(&ctx);
    assert_eq!(free.get("fly4"), Some(&2));
    assert_eq!(free.get("flyfull"), Some(&1));
    assert_eq!(free.len(), 2);
    assert!(
        host.commands()
            .iter()
            .any(|cmd| cmd.contains("pool status --porcelain"))
    );
}

#[test]
fn a_ready_unit_becomes_inflight_with_task_branch_model_and_pool() {
    let (dir, ctx, host) = setup();
    wire(&host);
    let mut st = State::default();
    ordered(&mut st, &["u1"]);
    dispatch(&ctx, &mut st);
    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Inflight);
    assert_eq!(unit.attempts, 1);
    assert_eq!(unit.task.as_deref(), Some("1a2b3c4d"));
    assert_eq!(unit.branch.as_deref(), Some("vivi/unit/u1-a1"));
    assert_eq!(unit.model.as_deref(), Some("m/a"));
    assert_eq!(unit.pool.as_deref(), Some("fly4"));
    assert_eq!(unit.since, T0);
    assert_eq!(unit.since_dispatch, Some(T0));
    let sent = commands_with(&host, "task send --from operator --to hand");
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains("--subject '[u1 a1] Do u1'"));
    assert!(sent[0].contains("--body-file"));
    let disp = commands_with(&host, "pool dispatch --pool fly4 1a2b3c4d");
    assert_eq!(disp.len(), 1);
    assert!(
        disp[0].contains("--need n1 --kind plain --target default --model m/a --from next --branch vivi/unit/u1-a1 --publish-ref refs/heads/vivi/unit/u1-a1 --budget 600 --porcelain")
    );
    let brief = fs::read_to_string(dir.path().join("home/briefs/u1.txt")).unwrap();
    assert!(brief.contains("UNIT u1: Do u1"));
}

#[test]
fn deleted_proof_scripts_are_parked_before_dispatch_without_charging_attempts() {
    let (_dir, ctx, host) = setup();
    let script = ctx.project.join("scripta/proof.sh");
    fs::create_dir_all(script.parent().unwrap()).unwrap();
    fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).unwrap();
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    fs::write(
        ctx.home.join("plans/n1.json"),
        r#"{"need":"n1","verdict":"units","units":[{"id":"n1-1","brief":"one","write_scope":["src/a"],"proof":["scripta/proof.sh"]},{"id":"n1-2","brief":"two","write_scope":["src/b"],"proof":["scripta/proof.sh"]}]}"#,
    )
    .unwrap();
    let mut st = State::default();
    crate::pump::plan::ingest(&ctx, &mut st);
    assert_eq!(st.units.len(), 2);
    fs::remove_file(script).unwrap();
    host.reply("pool status", 0, "slot fly4 bridge slot-1 free\n");

    dispatch(&ctx, &mut st);

    for unit in st.units.values() {
        assert_eq!(unit.status, Status::NeedsReplan);
        assert_eq!(unit.attempts, 0);
        assert_eq!(unit.task, None);
    }
    assert!(commands_with(&host, "task send").is_empty());
    let recorded = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    let needs: Vec<&str> = recorded
        .lines()
        .filter(|line| line.contains("needs-replan n1"))
        .collect();
    assert_eq!(needs.len(), 1, "exceptions: {recorded}");
    assert!(needs[0].contains("2 unit(s)"), "exceptions: {recorded}");
    assert!(
        needs[0].contains("scripta/proof.sh"),
        "exceptions: {recorded}"
    );
    assert!(needs[0].contains("n1-1") && needs[0].contains("n1-2"));
}

#[test]
fn closed_needs_withdraw_pending_delivered_merging_and_inflight_units() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    ordered(
        &mut st,
        &[
            "n1-pending",
            "n1-delivered",
            "n1-merging",
            "n1-inflight",
            "n1-merged",
        ],
    );
    st.units.get_mut("n1-delivered").unwrap().status = Status::Delivered;
    st.units.get_mut("n1-delivered").unwrap().branch = Some(String::from("unit/n1-delivered"));
    st.units.get_mut("n1-merging").unwrap().status = Status::Merging;
    st.units.get_mut("n1-merging").unwrap().branch = Some(String::from("unit/n1-merging"));
    let inflight = st.units.get_mut("n1-inflight").unwrap();
    inflight.status = Status::Inflight;
    inflight.task = Some(String::from("live-task"));
    inflight.branch = Some(String::from("unit/n1-inflight-a1"));
    st.units.get_mut("n1-merged").unwrap().status = Status::Merged;
    host.reply(
        "need list --status done",
        0,
        r#"[{"handle":"n1","date":"2026-10-19","subject":"Done","marks":[]}]"#,
    );
    host.reply("pool status", 0, "slot fly4 bridge slot-1 free\n");

    dispatch(&ctx, &mut st);

    assert_eq!(st.units["n1-pending"].status, Status::Withdrawn);
    assert_eq!(st.units["n1-delivered"].status, Status::Withdrawn);
    assert_eq!(st.units["n1-merging"].status, Status::Withdrawn);
    assert_eq!(st.units["n1-inflight"].status, Status::Withdrawn);
    assert_eq!(st.units["n1-merged"].status, Status::Merged);
    assert!(commands_with(&host, "task send").is_empty());
}

#[test]
fn manually_done_needs_withdraw_unfinished_units_and_preserve_merged() {
    let (_dir, ctx, host) = setup();
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    fs::write(
        ctx.home.join("plans/n1.json"),
        r#"{"need":"n1","verdict":"units","units":[{"id":"n1-1","brief":"queued","write_scope":["src/a"]},{"id":"n1-2","brief":"merged","write_scope":["src/b"]},{"id":"n1-3","brief":"live","write_scope":["src/c"]},{"id":"n1-4","brief":"retried","write_scope":["src/d"]}]}"#,
    )
    .unwrap();
    let mut st = State::default();
    crate::pump::plan::ingest(&ctx, &mut st);
    st.units.get_mut("n1-2").unwrap().status = Status::Merged;
    let live = st.units.get_mut("n1-3").unwrap();
    live.status = Status::Inflight;
    live.task = Some(String::from("live-task"));
    live.branch = Some(String::from("unit/n1-3-a1"));
    let retried = st.units.get_mut("n1-4").unwrap();
    retried.attempts = 1;
    retried.history.push(Attempt {
        why: String::from("finished/agent exit 1"),
        ..Attempt::default()
    });
    host.reply(
        "need list --status done",
        0,
        r#"[{"handle":"n1","date":"2026-10-19","subject":"Done","marks":[]}]"#,
    );
    host.reply("pool status", 0, "slot fly4 bridge slot-1 free\n");

    dispatch(&ctx, &mut st);

    assert_eq!(st.units["n1-1"].status, Status::Withdrawn);
    assert_eq!(st.units["n1-2"].status, Status::Merged);
    assert_eq!(st.units["n1-3"].status, Status::Withdrawn);
    assert_eq!(st.units["n1-4"].status, Status::Withdrawn);
    assert!(
        st.closed_needs.is_empty(),
        "manual completion is not a merge receipt"
    );
    assert_eq!(st.merges, 0);
    assert!(commands_with(&host, "task send").is_empty());
}

#[test]
fn unreadable_done_need_query_pauses_dispatch() {
    let (_dir, ctx, host) = setup();
    wire(&host);
    let mut st = State::default();
    ordered(&mut st, &["u1"]);
    host.reply(
        "need list --status done",
        1,
        r#"[{"handle":"u1","date":"2026-10-19","subject":"Done","marks":[]}]"#,
    );

    dispatch(&ctx, &mut st);

    assert_eq!(st.units["u1"].status, Status::Pending);
    assert_eq!(st.units["u1"].attempts, 0);
    assert!(commands_with(&host, "task send").is_empty());
}

/// A state with `fly4` paused since `T0` under `probe`.
fn paused_fly4(probe: Option<String>) -> State {
    let mut st = State::default();
    ordered(&mut st, &["u1"]);
    st.env_pauses.insert(
        String::from("fly4"),
        EnvPause {
            fails: vec![T0, T0],
            paused_at: Some(T0),
            probe,
            ..EnvPause::default()
        },
    );
    st
}

#[test]
fn a_paused_pool_is_not_dispatched_to() {
    let (_dir, ctx, host) = setup();
    wire(&host);
    host.reply("command -v pi", 1, "");
    let mut st = paused_fly4(Some(String::from("command -v pi")));
    dispatch(&ctx, &mut st);
    assert!(commands_with(&host, "pool dispatch --pool fly4").is_empty());
    assert_eq!(st.units["u1"].status, Status::Pending);
    let pause = st.env_pauses.get("fly4").expect("pause survives");
    assert_eq!(pause.probe.as_deref(), Some("command -v pi"));
    assert_eq!(pause.paused_at, Some(T0));
}

#[test]
fn a_paused_pool_resumes_when_the_probe_passes() {
    let (_dir, ctx, host) = setup();
    wire(&host);
    host.reply("command -v pi", 0, "/opt/homebrew/bin/pi");
    let mut st = paused_fly4(Some(String::from("command -v pi")));
    dispatch(&ctx, &mut st);
    assert_eq!(commands_with(&host, "pool dispatch --pool fly4").len(), 1);
    assert_eq!(st.units["u1"].status, Status::Inflight);
    assert!(st.env_pauses.is_empty());
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("environment cleared for pool fly4"))
    );
}

#[test]
fn a_paused_pool_without_probe_resumes_once_the_fail_window_passes() {
    let (_dir, ctx, host) = setup();
    wire(&host);
    let mut st = paused_fly4(None);
    host.advance(301);
    dispatch(&ctx, &mut st);
    assert_eq!(commands_with(&host, "pool dispatch --pool fly4").len(), 1);
    assert_eq!(st.units["u1"].status, Status::Inflight);
    assert!(st.env_pauses.is_empty());
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("environment cleared for pool fly4"))
    );
}

#[test]
fn a_unit_whose_target_was_removed_is_parked_instead_of_falling_back() {
    let mut cfg = dispatch_cfg();
    cfg.targets = BTreeMap::from([(
        String::from("radix"),
        PumpTarget {
            path: String::from("radix"),
            branch: String::from("next"),
            ..PumpTarget::default()
        },
    )]);
    let (_dir, ctx, host) = setup_with(cfg);
    let mut st = State::default();
    ordered(&mut st, &["n-1"]);
    st.units.get_mut("n-1").unwrap().target = String::from("removed");
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["n-1"].status, Status::Held);
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("EXCEPTION unknown-target n-1"))
    );
    assert!(
        host.commands()
            .iter()
            .all(|line| !line.contains("pool dispatch"))
    );
    let mut restored = dispatch_cfg();
    restored.targets = BTreeMap::from([(
        String::from("removed"),
        PumpTarget {
            path: String::from("restored"),
            branch: String::from("main"),
            ..PumpTarget::default()
        },
    )]);
    let (_dir, restored_ctx, _) = setup_with(restored);
    dispatch(&restored_ctx, &mut st);
    assert_eq!(st.units["n-1"].status, Status::Pending);
}

#[test]
fn named_target_dispatch_starts_from_its_own_integration_branch() {
    let mut cfg = dispatch_cfg();
    cfg.targets.insert(
        String::from("radix"),
        PumpTarget {
            path: String::from("radix"),
            branch: String::from("radix-next"),
            ..PumpTarget::default()
        },
    );
    let (_dir, ctx, host) = setup_with(cfg);
    wire(&host);
    let mut st = State::default();
    ordered(&mut st, &["u1"]);
    st.units.get_mut("u1").unwrap().target = String::from("radix");
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Inflight);
    let command = commands_with(&host, "pool dispatch").remove(0);
    assert!(
        command.contains("--target radix --model m/a --from radix-next"),
        "{command}"
    );
}

#[test]
fn a_full_tools_unit_goes_to_the_full_pool() {
    let (_dir, ctx, host) = setup();
    wire(&host);
    let mut st = State::default();
    ordered(&mut st, &["u1"]);
    st.units.get_mut("u1").unwrap().tools = String::from("full");
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["u1"].pool.as_deref(), Some("flyfull"));
    assert_eq!(
        commands_with(&host, "pool dispatch --pool flyfull").len(),
        1
    );
    assert!(commands_with(&host, "pool dispatch --pool fly4").is_empty());
}

#[test]
fn a_unit_waits_while_its_kinds_max_units_are_inflight() {
    let mut cfg = dispatch_cfg();
    cfg.dispatch.per_tick = 3;
    cfg.dispatch.kinds.get_mut("full").unwrap().max = Some(2);
    let (_dir, ctx, host) = setup_with(cfg);
    // Three free slots on the full pool, so only the kind `max` can gate.
    host.reply(
        "pool status",
        0,
        "slot fly4 bridge slot-1 free\nslot flyfull bridge slot-1 free\n\
         slot flyfull bridge slot-2 free\nslot flyfull bridge slot-3 free\n",
    );
    host.reply("task send", 0, "created hand 1a2b3c4d\nnode 1a2b3c4d");
    host.reply(
        "pool dispatch",
        0,
        "dispatched 1a2b3c4d bridge slot-1 m/a unit/x-a1",
    );
    let mut st = State::default();
    ordered(&mut st, &["u1", "u2", "u3", "u4"]);
    for uid in ["u1", "u2", "u3"] {
        st.units.get_mut(uid).unwrap().tools = String::from("full");
    }
    dispatch(&ctx, &mut st);
    // u1 and u2 fill the cap (same-tick starts count); u3 waits, unsent and
    // unstarted; the plain unit u4 is not held back by the full kind cap.
    assert_eq!(st.units["u1"].status, Status::Inflight);
    assert_eq!(st.units["u2"].status, Status::Inflight);
    assert_eq!(st.units["u3"].status, Status::Pending);
    assert_eq!(st.units["u3"].task, None);
    assert_eq!(st.units["u4"].status, Status::Inflight);
    assert_eq!(st.units["u4"].pool.as_deref(), Some("fly4"));
    assert_eq!(commands_with(&host, "task send").len(), 3);
    assert_eq!(
        commands_with(&host, "pool dispatch --pool flyfull").len(),
        2
    );
    assert_eq!(commands_with(&host, "pool dispatch --pool fly4").len(), 1);
}

#[test]
fn a_unit_without_tools_takes_the_default_kind() {
    let (_dir, ctx, host) = setup();
    wire(&host);
    let mut st = State::default();
    ordered(&mut st, &["u1"]);
    st.units.get_mut("u1").unwrap().tools = String::new();
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["u1"].pool.as_deref(), Some("fly4"));
}

#[test]
fn a_unit_naming_an_unknown_kind_is_not_dispatched_and_reported_once() {
    let (dir, ctx, host) = setup();
    wire(&host);
    let mut st = State::default();
    ordered(&mut st, &["u1", "u2"]);
    st.units.get_mut("u1").unwrap().tools = String::from("exotic");
    dispatch(&ctx, &mut st);
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Pending);
    assert_eq!(st.units["u1"].task, None);
    assert_eq!(st.units["u2"].status, Status::Inflight, "others still go");
    let text = fs::read_to_string(dir.path().join("home/exceptions.txt")).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains("unknown-kind u1"), "{text}");
    assert!(text.contains("exotic"), "{text}");
}

#[test]
fn a_kind_without_max_has_no_cap() {
    let (_dir, ctx, host) = setup();
    // `dispatch_cfg` gives the kinds no `max`; give the full pool
    // three free slots so nothing but the cap could hold a unit back.
    host.reply(
        "pool status",
        0,
        "slot fly4 bridge slot-1 free\nslot flyfull bridge slot-1 free\n\
         slot flyfull bridge slot-2 free\nslot flyfull bridge slot-3 free\n",
    );
    host.reply("task send", 0, "created hand 1a2b3c4d\nnode 1a2b3c4d");
    host.reply(
        "pool dispatch",
        0,
        "dispatched 1a2b3c4d bridge slot-1 m/a unit/x-a1",
    );
    let mut st = State::default();
    ordered(&mut st, &["u1", "u2", "u3"]);
    for uid in ["u1", "u2", "u3"] {
        st.units.get_mut(uid).unwrap().tools = String::from("full");
    }
    dispatch(&ctx, &mut st);
    assert!(
        ["u1", "u2", "u3"]
            .iter()
            .all(|uid| st.units[*uid].status == Status::Inflight)
    );
    assert!(
        ["u1", "u2", "u3"]
            .iter()
            .all(|uid| st.units[*uid].pool.as_deref() == Some("flyfull"))
    );
    assert_eq!(
        commands_with(&host, "pool dispatch --pool flyfull").len(),
        3
    );
}

#[test]
fn a_pool_without_a_free_slot_leaves_the_unit_pending() {
    let (_dir, ctx, host) = setup();
    wire(&host);
    // A later rule with the same fragment replaces `wire`'s: no free slots.
    host.reply("pool status", 0, "slot fly4 bridge slot-1 busy\n");
    let mut st = State::default();
    ordered(&mut st, &["u1"]);
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Pending);
    assert!(commands_with(&host, "task send").is_empty());
    assert!(commands_with(&host, "pool dispatch").is_empty());
}

#[test]
fn per_tick_caps_the_number_of_started_units() {
    let (_dir, ctx, host) = setup_with(cfg_with(2));
    wire(&host);
    let mut st = State::default();
    ordered(&mut st, &["u1", "u2", "u3"]);
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Inflight);
    assert_eq!(st.units["u2"].status, Status::Inflight);
    assert_eq!(st.units["u3"].status, Status::Pending);
    assert_eq!(
        commands_with(&host, "pool dispatch --pool fly4 1a2b3c4d").len(),
        2
    );
}

/// A [`FakeHost`] gate proving concurrency: the first `pool dispatch` parks
/// until a second one arrives, or a 3-second deadline passes so a serial
/// regression fails the test instead of hanging the suite. Every other
/// call delegates straight to the inner host.
struct GateHost {
    inner: Arc<FakeHost>,
    arrived: AtomicUsize,
    open: Mutex<bool>,
    signal: Condvar,
}

impl Host for GateHost {
    fn now(&self) -> u64 {
        self.inner.now()
    }

    fn run(&self, cmd: &str, cwd: Option<&Path>, timeout_secs: u64) -> CmdOutput {
        if !cmd.contains("pool dispatch") {
            return self.inner.run(cmd, cwd, timeout_secs);
        }
        let n = self.arrived.fetch_add(1, Ordering::SeqCst);
        let mut open = self.open.lock().unwrap();
        if n == 0 {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !*open {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    break;
                };
                let (guard, _) = self.signal.wait_timeout(open, remaining).unwrap();
                open = guard;
            }
        } else {
            *open = true;
            self.signal.notify_all();
        }
        self.inner.run(cmd, cwd, timeout_secs)
    }

    fn spawn(
        &self,
        cmd: &str,
        cwd: Option<&Path>,
        stdin: Option<&Path>,
        out: &Path,
    ) -> Option<u32> {
        self.inner.spawn(cmd, cwd, stdin, out)
    }

    fn log(&self, line: &str) {
        self.inner.log(line)
    }
}

#[test]
fn dispatches_for_different_slots_run_concurrently() {
    let (_dir, ctx, host) = setup_with(cfg_with(2));
    wire(&host);
    let gate = Arc::new(GateHost {
        inner: host.clone(),
        arrived: AtomicUsize::new(0),
        open: Mutex::new(false),
        signal: Condvar::new(),
    });
    let ctx = Ctx { host: gate, ..ctx };
    let mut st = State::default();
    ordered(&mut st, &["u1", "u2"]);
    let began = Instant::now();
    dispatch(&ctx, &mut st);
    let elapsed = began.elapsed();
    // The task sends are instant through the fake, so a concurrent tick has
    // the second pool dispatch open the gate almost immediately; a serial
    // regression parks the first pool dispatch the full 3 seconds.
    assert!(
        elapsed < Duration::from_secs(2),
        "dispatch took {elapsed:?}; the pool dispatches did not run concurrently"
    );
    assert_eq!(st.units["u1"].status, Status::Inflight);
    assert_eq!(st.units["u2"].status, Status::Inflight);
    assert_eq!(commands_with(&host, "pool dispatch").len(), 2);
    let started = host
        .logs()
        .into_iter()
        .filter(|line| line.starts_with("dispatch "))
        .collect::<Vec<_>>();
    assert_eq!(started.len(), 2, "{started:?}");
}

#[test]
fn fix_units_are_dispatched_before_ordinary_units() {
    let (_dir, ctx, host) = setup_with(cfg_with(1));
    wire(&host);
    let mut st = State::default();
    ordered(&mut st, &["u1", "pumpfix-1"]);
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["pumpfix-1"].status, Status::Inflight);
    assert_eq!(st.units["u1"].status, Status::Pending);
    let sent = commands_with(&host, "task send --from operator --to hand");
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains("[pumpfix-1 a1]"));
}

/// Give each need in `needs` priority level 1.
fn write_priority(ctx: &Ctx, needs: &[&str]) {
    for need in needs {
        crate::pump::priority::set(&ctx.home, need, 1).unwrap();
    }
}

#[test]
fn fix_units_dispatch_before_priority_units_and_the_rest() {
    let (_dir, ctx, host) = setup_with(cfg_with(1));
    wire(&host);
    write_priority(&ctx, &["hot-need"]);
    let mut st = State::default();
    ordered(&mut st, &["u1", "pumpfix-1", "u2"]);
    st.units.get_mut("u2").unwrap().need = String::from("hot-need");
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["pumpfix-1"].status, Status::Inflight);
    assert_eq!(st.units["u2"].status, Status::Pending);
    assert_eq!(st.units["u1"].status, Status::Pending);
    let sent = commands_with(&host, "task send --from operator --to hand");
    assert!(sent[0].contains("[pumpfix-1 a1]"));
}

#[test]
fn a_task_send_without_a_handle_leaves_the_unit_pending() {
    let (_dir, ctx, host) = setup();
    wire(&host);
    host.reply_once("task send", 0, "queued with no receipt");
    let mut st = State::default();
    ordered(&mut st, &["u1"]);
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Pending);
    assert!(commands_with(&host, "pool dispatch").is_empty());
    assert!(
        host.logs()
            .iter()
            .any(|line| line == "task send failed u1: queued with no receipt")
    );
}

#[test]
fn a_config_class_task_send_failure_raises_one_exception_and_recovers() {
    let (dir, ctx, host) = setup();
    wire(&host);
    host.reply(
        "task send",
        1,
        "error: message error: unknown local role 'hand'; add it with `vivi role add hand`",
    );
    let mut st = State::default();
    ordered(&mut st, &["u1"]);
    dispatch(&ctx, &mut st);
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Pending);
    let text = fs::read_to_string(dir.path().join("home/exceptions.txt")).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains("unknown-role u1"), "{text}");
    assert!(text.contains("vivi role add hand"), "{text}");
    assert!(st.units["u1"].send_reported);
    host.reply("task send", 0, "created hand 1a2b3c4d\nnode 1a2b3c4d");
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Inflight);
    assert!(!st.units["u1"].send_reported);
}

#[test]
fn a_failed_pool_dispatch_marks_the_task_done_and_leaves_the_unit_pending() {
    let (_dir, ctx, host) = setup();
    wire(&host);
    host.reply("pool dispatch", 1, "remote refused");
    let mut st = State::default();
    ordered(&mut st, &["u1"]);
    dispatch(&ctx, &mut st);
    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Pending);
    assert_eq!(unit.attempts, 0);
    assert_eq!(unit.task, None);
    let done = commands_with(&host, "task done");
    assert_eq!(done.len(), 1);
    assert!(
        done[0].contains("task done 1a2b3c4d --for operator --note 'dispatch failed; superseded'")
    );
    assert!(
        host.logs()
            .iter()
            .any(|line| line == "dispatch failed u1: remote refused")
    );
}

#[test]
fn existing_attempt_branches_advance_until_dispatch_succeeds_without_extra_charge() {
    let (dir, ctx, host) = setup();
    wire(&host);
    host.reply_once(
        "pool dispatch --pool fly4",
        1,
        "error: message error: branch \"vivi/unit/u1-a2\" already exists and --from is \"origin/next\": refusing to move or reset it",
    );
    host.reply_once(
        "pool dispatch --pool fly4",
        1,
        "error: message error: branch \"vivi/unit/u1-a3\" already exists and --from is \"origin/next\": refusing to move or reset it",
    );
    let mut st = State::default();
    let mut unit = pending_unit("u1");
    unit.attempts = 1;
    unit.branch = Some(String::from("unit/u1-a1"));
    ordered(&mut st, &["u1"]);
    st.units.insert(String::from("u1"), unit);

    dispatch(&ctx, &mut st);
    dispatch(&ctx, &mut st);
    dispatch(&ctx, &mut st);

    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Inflight);
    assert_eq!(
        unit.attempts, 2,
        "only the successful start charged a hand attempt"
    );
    assert!(unit.history.is_empty());
    assert_eq!(unit.branch.as_deref(), Some("vivi/unit/u1-a4"));
    assert_eq!(unit.next_branch_suffix, None);
    assert_eq!(commands_with(&host, "task send").len(), 3);
    let dispatches = commands_with(&host, "pool dispatch --pool fly4");
    assert_eq!(dispatches.len(), 3);
    for (dispatch, suffix) in dispatches.iter().zip(["a2", "a3", "a4"]) {
        assert!(
            dispatch.contains(&format!("vivi/unit/u1-{suffix}")),
            "{dispatch}"
        );
    }
    assert_eq!(commands_with(&host, "task done").len(), 2);
    let exceptions = fs::read_to_string(dir.path().join("home/exceptions.txt")).unwrap();
    assert_eq!(
        exceptions.matches("branch-exists u1").count(),
        1,
        "{exceptions}"
    );
    assert!(exceptions.contains("dispatch will advance to the next suffix"));
}

#[test]
fn no_free_slot_in_the_failure_text_stops_further_dispatch_to_that_pool() {
    let (_dir, ctx, host) = setup();
    host.reply("pool status", 0, "slot fly4 bridge slot-1 free\n");
    host.reply("task send", 0, "created hand 1a2b3c4d\nnode 1a2b3c4d");
    host.reply("pool dispatch", 0, "error: no free slot on fly4");
    let mut st = State::default();
    ordered(&mut st, &["u1", "u2"]);
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Pending);
    assert_eq!(st.units["u2"].status, Status::Pending);
    assert_eq!(commands_with(&host, "task send").len(), 1);
    assert_eq!(commands_with(&host, "pool dispatch --pool fly4").len(), 1);
}

#[test]
fn the_second_attempt_uses_a_new_branch_name_and_carries_the_failure_in_the_brief_file() {
    let (dir, ctx, host) = setup();
    wire(&host);
    let mut st = State::default();
    ordered(&mut st, &["u1"]);
    let unit = st.units.get_mut("u1").unwrap();
    unit.attempts = 1;
    unit.history = vec![Attempt {
        attempt: 1,
        model: Some(String::from("m/b")),
        why: String::from("proof-failed"),
        detail: String::from("cargo test exploded"),
        at: T0,
        ..Attempt::default()
    }];
    dispatch(&ctx, &mut st);
    let unit = &st.units["u1"];
    assert_eq!(unit.attempts, 2);
    assert_eq!(unit.branch.as_deref(), Some("vivi/unit/u1-a2"));
    let brief = fs::read_to_string(dir.path().join("home/briefs/u1.txt")).unwrap();
    assert!(brief.contains("- attempt 1 (m/b) ended: proof-failed; branch vivi/unit/u1-a1"));
    assert!(brief.contains("cargo test exploded"));
}

#[test]
fn a_final_attempt_unit_without_a_last_resort_does_not_block_later_units() {
    let mut cfg = dispatch_cfg();
    cfg.dispatch.last_resort = String::new();
    let (_dir, ctx, host) = setup_with(cfg);
    wire(&host);
    let mut st = State::default();
    ordered(&mut st, &["u1", "u2"]);
    st.units.get_mut("u1").unwrap().attempts = 2; // the final attempt
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Inflight);
    assert_eq!(st.units["u1"].attempts, 3);
    assert_eq!(st.units["u2"].status, Status::Inflight);
}

#[test]
fn a_need_dependency_hold_keeps_a_unit_pending_and_logs_once() {
    let (_dir, ctx, host) = setup();
    wire(&host);
    host.reply(
        "graph blocked",
        0,
        r#"[{"source_id":"n","kind":"need","label":"N","blocked_by":["dep"]}]"#,
    );
    let mut st = State::default();
    ordered(&mut st, &["n-1"]);
    let held = st.units.get_mut("n-1").unwrap();
    held.need = String::from("n");
    held.ready_at = Some(T0 - 100);

    dispatch(&ctx, &mut st);
    assert_eq!(st.units["n-1"].status, Status::Pending);
    assert_eq!(st.units["n-1"].held_on.as_deref(), Some("dep"));
    assert_eq!(st.units["n-1"].ready_at, None);
    assert!(commands_with(&host, "task send").is_empty());

    dispatch(&ctx, &mut st);
    let holds = host
        .logs()
        .into_iter()
        .filter(|line| line.starts_with("hold n-1:"))
        .count();
    assert_eq!(holds, 1, "the hold is logged on the first held tick only");

    // The dependency lands: the next tick dispatches the unit.
    host.reply("graph blocked", 0, "[]");
    host.advance(10);
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["n-1"].status, Status::Inflight);
    assert!(st.units["n-1"].held_on.is_none());
    assert_eq!(st.units["n-1"].ready_at, Some(T0 + 10));
    assert_eq!(commands_with(&host, "task send").len(), 1);
}

/// A config whose `full` kind spans pools `a` and `b` (optionally capped).
fn two_pool_cfg(max: Option<u32>) -> PumpConfig {
    let mut cfg = dispatch_cfg();
    cfg.dispatch.per_tick = 3;
    cfg.dispatch
        .kinds
        .insert(String::from("full"), kind_of_pools(&["a", "b"], max));
    cfg
}

/// Reply to `task send` and `pool dispatch` so units can start.
fn wire_sends(host: &FakeHost) {
    host.reply("task send", 0, "created hand 1a2b3c4d\nnode 1a2b3c4d");
    host.reply(
        "pool dispatch",
        0,
        "dispatched 1a2b3c4d bridge slot-1 m/a unit/x-a1",
    );
}

/// Run one dispatch tick over `uids` (all of kind `full`) against `status`.
fn run_full_units(cfg: PumpConfig, status: &str, uids: &[&str]) -> State {
    let (_dir, ctx, host) = setup_with(cfg);
    host.reply("pool status", 0, status);
    wire_sends(&host);
    let mut st = State::default();
    ordered(&mut st, uids);
    for uid in uids {
        st.units.get_mut(*uid).unwrap().tools = String::from("full");
    }
    dispatch(&ctx, &mut st);
    st
}

#[test]
fn a_multi_pool_kind_picks_the_pool_with_most_free_slots() {
    let status =
        "slot a bridge slot-1 free\nslot b bridge slot-1 free\nslot b bridge slot-2 free\n";
    let st = run_full_units(two_pool_cfg(None), status, &["u1"]);
    assert_eq!(st.units["u1"].pool.as_deref(), Some("b"));
}

#[test]
fn a_multi_pool_kind_breaks_ties_by_listed_order() {
    let status = "slot b bridge slot-1 free\nslot a bridge slot-1 free\n";
    let st = run_full_units(two_pool_cfg(None), status, &["u1"]);
    assert_eq!(st.units["u1"].pool.as_deref(), Some("a"));
}

#[test]
fn a_multi_pool_kind_falls_through_to_the_next_pool_when_the_first_is_full() {
    let status = "slot a bridge slot-1 busy\nslot b bridge slot-1 free\n";
    let st = run_full_units(two_pool_cfg(None), status, &["u1", "u2"]);
    assert_eq!(st.units["u1"].pool.as_deref(), Some("b"));
    // Pool b's only slot is consumed and a has none: u2 waits.
    assert_eq!(st.units["u2"].status, Status::Pending);
    assert_eq!(st.units["u2"].task, None);
}

#[test]
fn a_kinds_max_counts_inflight_units_across_all_its_pools() {
    let status =
        "slot a bridge slot-1 free\nslot b bridge slot-1 free\nslot b bridge slot-2 free\n";
    let st = run_full_units(two_pool_cfg(Some(2)), status, &["u1", "u2", "u3"]);
    // u1 goes to b (most free); u2 ties at one slot each and takes a (first
    // listed); the cap of 2 holds u3 back although a slot remains.
    assert_eq!(st.units["u1"].pool.as_deref(), Some("b"));
    assert_eq!(st.units["u2"].pool.as_deref(), Some("a"));
    assert_eq!(st.units["u3"].status, Status::Pending);
}

fn hand_on(pool: &str) -> RoleRouting {
    RoleRouting {
        roles: vec![RoutedRole {
            name: "hand".into(),
            kind: Some("hand".into()),
            status: "active".into(),
            pool: Some(pool.into()),
            read_only: None,
            model: None,
            models: Vec::new(),
        }],
        pools: BTreeMap::from([("a".into(), routed_pool(2)), ("b".into(), routed_pool(2))]),
    }
}

fn routed_pool(slots: u32) -> RoutedPool {
    RoutedPool {
        slots,
        provider: "local".into(),
        fallback: None,
        caps: BTreeMap::new(),
        models: Vec::new(),
        has_bands: false,
    }
}

fn run_full_units_routed(status: &str, uids: &[&str], routing: RoleRouting) -> State {
    let (_dir, ctx, host) = setup_with(two_pool_cfg(None));
    let ctx = ctx.with_routing(routing);
    host.reply("pool status", 0, status);
    wire_sends(&host);
    let mut st = State::default();
    ordered(&mut st, uids);
    for uid in uids {
        st.units.get_mut(*uid).unwrap().tools = String::from("full");
    }
    dispatch(&ctx, &mut st);
    st
}

#[test]
fn a_hand_role_pool_is_used_even_when_another_pool_has_more_slots() {
    let status =
        "slot a bridge slot-1 free\nslot b bridge slot-1 free\nslot b bridge slot-2 free\n";
    let st = run_full_units_routed(status, &["u1"], hand_on("a"));
    assert_eq!(st.units["u1"].pool.as_deref(), Some("a"));
}

#[test]
fn a_hand_role_pool_dispatches_without_tools_or_a_default_kind() {
    let config = PumpConfig {
        branch: String::from("next"),
        ..PumpConfig::default()
    };
    let (_dir, ctx, host) = setup_with(config);
    let mut routing = hand_on("a");
    routing.roles[0].model = Some(String::from("m/a"));
    let ctx = ctx.with_routing(routing);
    host.reply(
        "pool status",
        0,
        "slot a bridge slot-1 free\nslot b bridge slot-1 free\n",
    );
    wire_sends(&host);
    let mut st = State::default();
    ordered(&mut st, &["u1"]);
    st.units.get_mut("u1").unwrap().tools.clear();

    dispatch(&ctx, &mut st);

    assert_eq!(st.units["u1"].status, Status::Inflight);
    assert_eq!(st.units["u1"].pool.as_deref(), Some("a"));
    assert!(
        !host
            .logs()
            .iter()
            .any(|line| line.contains("EXCEPTION unknown-kind u1"))
    );
}

#[test]
fn a_hand_role_uses_its_configured_fallback_when_the_primary_pool_is_full() {
    let status = "slot a bridge slot-1 busy\nslot b bridge slot-1 free\n";
    let mut routing = hand_on("a");
    routing.pools.get_mut("a").unwrap().fallback = Some(String::from("b"));
    let st = run_full_units_routed(status, &["u1"], routing);
    assert_eq!(st.units["u1"].status, Status::Inflight);
    assert_eq!(st.units["u1"].pool.as_deref(), Some("b"));
}

#[test]
fn a_quarantine_repair_uses_the_separate_merge_repair_budget() {
    let mut cfg = dispatch_cfg();
    cfg.merge.proof_timeout = 900;
    cfg.merge.repair_budget = Some(1800);
    let (_dir, ctx, host) = setup_with(cfg);
    wire(&host);
    let mut st = State::default();
    ordered(&mut st, &["pumpfix-q1"]);
    st.units.get_mut("pumpfix-q1").unwrap().repairs = Some(String::from("q1"));
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["pumpfix-q1"].status, Status::Inflight);

    assert!(
        host.commands().iter().any(|command| {
            command.contains("pool dispatch") && command.contains("--budget 1800")
        })
    );
}

#[test]
fn a_full_role_pool_waits_and_does_not_spill_to_another_pool() {
    let status = "slot a bridge slot-1 busy\nslot b bridge slot-1 free\n";
    let st = run_full_units_routed(status, &["u1", "u2"], hand_on("a"));
    assert_eq!(st.units["u1"].status, Status::Pending);
    assert_eq!(st.units["u1"].task, None);
    assert_eq!(st.units["u1"].pool, None);
    assert_eq!(st.units["u2"].status, Status::Pending);
}

#[test]
fn apply_start_on_dispatch_failure_closes_the_task_and_zeroes_the_pool() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    ordered(&mut st, &["u1"]);
    let mut free = BTreeMap::from([(String::from("fly4"), 2)]);
    let started = apply_start(
        &ctx,
        &mut st,
        "u1",
        &Plan {
            pool: String::from("fly4"),
            model: Some(String::from("m/a")),
            attempt: 1,
            branch_suffix: 1,
            branch: String::from("vivi/unit/u1-a1"),
            from_ref: String::from("next"),
        },
        "m/a",
        UnitStart::DispatchFailed {
            task: String::from("1a2b3c4d"),
            text: String::from("error: no free slot on fly4"),
        },
        &mut free,
    );
    assert!(!started);
    assert_eq!(free.get("fly4"), Some(&0));
    assert_eq!(st.units["u1"].status, Status::Pending);
    assert_eq!(st.units["u1"].task, None);
    let done = commands_with(&host, "task done");
    assert_eq!(done.len(), 1);
    assert!(
        done[0].contains("task done 1a2b3c4d --for operator --note 'dispatch failed; superseded'"),
        "{:?}",
        done
    );
}

#[test]
fn dispatch_records_the_slot_on_the_unit() {
    let (_dir, ctx, host) = setup();
    wire(&host);
    let mut st = State::default();
    ordered(&mut st, &["u1"]);
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["u1"].slot.as_deref(), Some("slot-1"));
}

#[test]
fn parse_dispatched_slot_reads_the_real_porcelain_line() {
    let line = "dispatched 1a2b3c4d fly4 slot-2 job001 m/a next unit/u1-a1 1800 write 0 -";
    assert_eq!(parse_dispatched_slot(line).as_deref(), Some("slot-2"));
}

#[test]
fn parse_dispatched_slot_treats_a_dash_as_absent() {
    assert_eq!(
        parse_dispatched_slot("dispatched 1a2b3c4d fly4 - job001"),
        None
    );
}

#[test]
fn parse_dispatched_slot_needs_a_slot_token() {
    assert_eq!(parse_dispatched_slot("dispatched 1a2b3c4d fly4"), None);
    assert_eq!(parse_dispatched_slot("nothing to see here"), None);
}

/// A config whose one plain kind pool (`fly4`) is also the gate pool, with
/// `per_tick` room for both a plain and a fix unit.
fn gate_pool_cfg() -> PumpConfig {
    let mut cfg = cfg_with(3);
    cfg.gate.pools = vec![String::from("fly4")];
    cfg
}

/// Wire one free gate-pool slot plus the sends a start needs.
fn wire_gate_pool(host: &Arc<FakeHost>) {
    host.reply("pool status", 0, "slot fly4 bridge slot-1 free\n");
    wire_sends(host);
}

#[test]
fn a_forced_gate_reserves_the_last_gate_pool_slot() {
    let (dir, ctx, host) = setup_with(gate_pool_cfg());
    wire_gate_pool(&host);
    fs::write(dir.path().join("home/FORCE-GATE"), "after fixes\n").unwrap();
    let mut st = State::default();
    ordered(&mut st, &["u1", "pumpfix-1"]);
    // The fix unit must wait, so only the reservation can hold the plain one.
    st.units.get_mut("pumpfix-1").unwrap().retry_not_before = Some(T0 + 60);
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Pending);
    assert_eq!(st.units["pumpfix-1"].status, Status::Pending);
    assert!(commands_with(&host, "task send").is_empty());
    assert!(commands_with(&host, "pool dispatch").is_empty());

    // The flag clears but the fix unit still waits: the plain unit may not
    // take the last gate-pool slot that the fix unit needs.
    fs::remove_file(dir.path().join("home/FORCE-GATE")).unwrap();
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Pending);
    assert_eq!(st.units["pumpfix-1"].status, Status::Pending);
    assert!(commands_with(&host, "task send").is_empty());

    // The fix unit becomes ready and takes the reserved slot; the plain unit
    // still cannot take the last slot while the fix unit is pending.
    host.advance(120);
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["pumpfix-1"].status, Status::Inflight);
    assert_eq!(st.units["u1"].status, Status::Pending);
    let sent = commands_with(&host, "task send --from operator --to hand");
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].contains("[pumpfix-1 a1]"));
}

#[test]
fn an_assigned_gate_role_pool_is_reserved_without_explicit_gate_candidates() {
    let mut cfg = cfg_with(3);
    cfg.gate.pools.clear();
    let (dir, base, host) = setup_with(cfg);
    let mut routing = hand_on("a");
    routing.roles.push(RoutedRole {
        name: String::from("gate"),
        kind: Some(String::from("gate")),
        status: String::from("active"),
        pool: Some(String::from("a")),
        read_only: Some(true),
        model: None,
        models: Vec::new(),
    });
    let ctx = base.with_routing(routing);
    host.reply("pool status", 0, "slot a bridge slot-1 free\n");
    fs::write(dir.path().join("home/FORCE-GATE"), "test reservation\n").unwrap();
    let mut st = State::default();
    ordered(&mut st, &["u1"]);
    st.units.get_mut("u1").unwrap().tools = String::from("full");

    dispatch(&ctx, &mut st);

    assert_eq!(st.units["u1"].status, Status::Pending);
    assert_eq!(st.units["u1"].task, None);
    assert!(commands_with(&host, "pool dispatch").is_empty());
}

#[test]
fn a_waiting_fix_unit_reserves_the_gate_pool_slot() {
    let (_dir, ctx, host) = setup_with(gate_pool_cfg());
    wire_gate_pool(&host);
    let mut st = State::default();
    ordered(&mut st, &["u1", "pumpfix-1"]);
    dispatch(&ctx, &mut st);
    // No forced gate: the fix unit takes the reserved slot and the plain unit
    // cannot have it.
    assert_eq!(st.units["pumpfix-1"].status, Status::Inflight);
    assert_eq!(st.units["u1"].status, Status::Pending);
    assert_eq!(commands_with(&host, "task send").len(), 1);

    // A fix unit that must wait still holds the slot: the plain unit cannot
    // take the last gate-pool slot.
    let mut blocked = State::default();
    ordered(&mut blocked, &["u2", "pumpfix-2"]);
    blocked.units.get_mut("pumpfix-2").unwrap().retry_not_before = Some(T0 + 3600);
    dispatch(&ctx, &mut blocked);
    assert_eq!(blocked.units["u2"].status, Status::Pending);
    assert_eq!(blocked.units["u2"].task, None);

    // With no fix unit pending the plain unit dispatches.
    let mut plain_only = State::default();
    ordered(&mut plain_only, &["u3"]);
    dispatch(&ctx, &mut plain_only);
    assert_eq!(plain_only.units["u3"].status, Status::Inflight);
}
