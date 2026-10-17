//! Tests for [`crate::pump::dispatch`]: the handle parser, the slot counter,
//! and the dispatch state machine driven through [`FakeHost`] replies.

use std::fs;
use std::sync::Arc;

use tempfile::tempdir;

use std::collections::BTreeMap;

use crate::pump::config::{DispatchConfig, Kind, Lane, PumpConfig};
use crate::pump::dispatch::*;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::{Attempt, State, Status, Unit};

/// The fake clock's start; units are created "now".
const T0: u64 = 1_000_000;

/// A kind dispatching to `pool`, capped at `max` units in flight.
fn kind(pool: &str, max: Option<u32>) -> Kind {
    Kind {
        pool: pool.to_owned(),
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
    assert_eq!(unit.branch.as_deref(), Some("unit/u1-a1"));
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
        disp[0].contains("--model m/a --from next --branch unit/u1-a1 --budget 600 --porcelain")
    );
    let brief = fs::read_to_string(dir.path().join("home/briefs/u1.txt")).unwrap();
    assert!(brief.contains("UNIT u1: Do u1"));
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

/// Write the configured priority file.
fn write_priority(ctx: &Ctx, text: &str) {
    let path = ctx.cfg.priority_path(&ctx.project);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn priority_units_dispatch_before_fix_units_and_the_rest() {
    let (_dir, ctx, host) = setup_with(cfg_with(1));
    wire(&host);
    write_priority(&ctx, "# hot\n\nhot-need\n");
    let mut st = State::default();
    ordered(&mut st, &["u1", "pumpfix-1", "u2"]);
    st.units.get_mut("u2").unwrap().need = String::from("hot-need");
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["u2"].status, Status::Inflight);
    assert_eq!(st.units["pumpfix-1"].status, Status::Pending);
    assert_eq!(st.units["u1"].status, Status::Pending);
    let sent = commands_with(&host, "task send --from operator --to hand");
    assert!(sent[0].contains("[u2 a1]"));
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
    }];
    dispatch(&ctx, &mut st);
    let unit = &st.units["u1"];
    assert_eq!(unit.attempts, 2);
    assert_eq!(unit.branch.as_deref(), Some("unit/u1-a2"));
    let brief = fs::read_to_string(dir.path().join("home/briefs/u1.txt")).unwrap();
    assert!(brief.contains("A PREVIOUS ATTEMPT AT THIS UNIT FAILED (proof-failed)"));
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
    st.units.get_mut("n-1").unwrap().need = String::from("n");

    dispatch(&ctx, &mut st);
    assert_eq!(st.units["n-1"].status, Status::Pending);
    assert_eq!(st.units["n-1"].held_on.as_deref(), Some("dep"));
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
    dispatch(&ctx, &mut st);
    assert_eq!(st.units["n-1"].status, Status::Inflight);
    assert!(st.units["n-1"].held_on.is_none());
    assert_eq!(commands_with(&host, "task send").len(), 1);
}
