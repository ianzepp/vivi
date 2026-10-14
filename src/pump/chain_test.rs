//! Tests for [`super::chain`] and the continuation chain across the pump:
//! plan ingest, readiness, dispatch `--from`, the merge guard, and `after`
//! satisfaction, driven through [`FakeHost`] replies.

use std::fs;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

use crate::pump::config::{DispatchConfig, Kind, Lane, MergeConfig, PumpConfig};
use crate::pump::dispatch::dispatch;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::merge::Merger;
use crate::pump::plan::ingest;
use crate::pump::ready::{is_ready, satisfied};
use crate::pump::run::tick;
use crate::pump::state::{State, Status, Unit};

/// The fake clock's start.
const T0: u64 = 1_000_000;

/// A config with integration branch `next`, one lane, one pool `p1`.
fn config() -> PumpConfig {
    PumpConfig {
        branch: String::from("next"),
        board: crate::pump::config::BoardConfig {
            from: Vec::new(),
            mark: String::new(),
            pull: false,
            ..Default::default()
        },
        dispatch: DispatchConfig {
            lanes: vec![Lane {
                model: String::from("m1"),
                cap: 5,
            }],
            kinds: std::collections::BTreeMap::from([(
                String::from("plain"),
                Kind {
                    pool: String::from("p1"),
                    ..Kind::default()
                },
            )]),
            default_kind: String::from("plain"),
            ..DispatchConfig::default()
        },
        merge: MergeConfig {
            shared_checks: Vec::new(),
            ..MergeConfig::default()
        },
        ..PumpConfig::default()
    }
}

/// A context over a fresh home.
fn setup() -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), config());
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    (dir, ctx, host)
}

/// A unit of need `n` writing `scope`, in `status`, on `branch`.
fn unit(status: Status, scope: &str, branch: Option<&str>) -> Unit {
    Unit {
        need: String::from("n"),
        title: String::from("a unit"),
        brief: String::from("do it"),
        write_scope: vec![scope.to_owned()],
        tools: String::from("plain"),
        status,
        branch: branch.map(str::to_owned),
        since: T0,
        ..Unit::default()
    }
}

/// Insert `unit` under `uid` and append it to the order.
fn put(st: &mut State, uid: &str, unit: Unit) {
    st.order.push(uid.to_owned());
    st.units.insert(uid.to_owned(), unit);
}

/// A unit that continues `from`.
fn follow(status: Status, scope: &str, from: &str) -> Unit {
    Unit {
        continues: Some(from.to_owned()),
        ..unit(status, scope, None)
    }
}

/// The state after unit `u-1` was blocked and replaced: `u-1` is `replaced`
/// by the chain `u-1.1 -> u-1.2` and `d` waits on `u-1`.
fn chained_state() -> State {
    let mut st = State::default();
    let mut blocked = unit(Status::Replaced, "src/a", Some("unit/u-1-a1"));
    blocked.replaced_by = vec![String::from("u-1.1"), String::from("u-1.2")];
    put(&mut st, "u-1", blocked);
    put(&mut st, "u-1.1", follow(Status::Pending, "src/a", "u-1"));
    put(&mut st, "u-1.2", follow(Status::Pending, "src/a", "u-1.1"));
    let mut dependent = unit(Status::Pending, "src/d", None);
    dependent.after = vec![String::from("u-1")];
    put(&mut st, "d", dependent);
    st
}

/// A continuation plan for `u-1` with the given unit ids.
fn plan(ids: &[&str]) -> Value {
    let mut after: Option<String> = None;
    let mut units = Vec::new();
    for id in ids {
        units.push(json!({
            "id": id,
            "title": format!("step {id}"),
            "brief": "continue",
            "write_scope": ["src/a"],
            "proof": ["./proof"],
            "after": after.iter().collect::<Vec<_>>(),
        }));
        after = Some((*id).to_owned());
    }
    json!({"need": "n", "replaces": "u-1", "verdict": "units", "reason": "r", "units": units})
}

/// Ingest `plan` as `plans/u-1.split.json`.
fn ingest_plan(ctx: &Ctx, st: &mut State, plan: &Value) {
    fs::write(
        ctx.home.join("plans").join("u-1.split.json"),
        plan.to_string(),
    )
    .unwrap();
    ingest(ctx, st);
}

/// A splitting unit `u-1` whose worker pushed `blocked` (or nothing).
fn splitting(blocked: Option<&str>) -> State {
    let mut st = State::default();
    let mut u = unit(Status::Splitting, "src/a", Some("unit/u-1-a1"));
    u.blocked_branch = blocked.map(str::to_owned);
    put(&mut st, "u-1", u);
    st
}

#[test]
fn a_continuation_plan_chains_its_units_from_the_blocked_branch() {
    let (_dir, ctx, _host) = setup();
    let mut st = splitting(Some("unit/u-1-a1"));
    ingest_plan(&ctx, &mut st, &plan(&["u-1.1", "u-1.2", "u-1.3"]));
    assert_eq!(st.units["u-1"].status, Status::Replaced);
    assert_eq!(st.units["u-1"].replaced_by, ["u-1.1", "u-1.2", "u-1.3"]);
    assert_eq!(st.units["u-1.1"].continues.as_deref(), Some("u-1"));
    assert_eq!(st.units["u-1.2"].continues.as_deref(), Some("u-1.1"));
    assert_eq!(st.units["u-1.3"].continues.as_deref(), Some("u-1.2"));
    // The chain carries the order; `after` between chain units would never be
    // satisfied (a unit that is handed on is satisfied only by the last one).
    assert!(st.units["u-1.2"].after.is_empty());
    assert!(st.units["u-1.3"].after.is_empty());
}

#[test]
fn a_blocked_unit_that_pushed_nothing_splits_from_the_integration_branch() {
    let (_dir, ctx, _host) = setup();
    let mut st = splitting(None);
    ingest_plan(&ctx, &mut st, &plan(&["u-1.1", "u-1.2"]));
    assert_eq!(st.units["u-1"].status, Status::Replaced);
    assert_eq!(st.units["u-1.1"].continues, None);
    assert_eq!(st.units["u-1.2"].continues, None);
    assert_eq!(st.units["u-1.2"].after, ["u-1.1"]);
}

#[test]
fn a_chain_unit_waits_for_its_predecessor_to_deliver() {
    let (_dir, ctx, _host) = setup();
    let mut st = chained_state();
    assert!(is_ready(&ctx, &st, "u-1.1")); // the blocked branch is pushed
    assert!(!is_ready(&ctx, &st, "u-1.2")); // u-1.1 has not delivered
    let first = st.units.get_mut("u-1.1").unwrap();
    first.status = Status::Inflight;
    assert!(!is_ready(&ctx, &st, "u-1.2"));
    let first = st.units.get_mut("u-1.1").unwrap();
    first.status = Status::Delivered;
    first.branch = Some(String::from("unit/u-1.1-a1"));
    assert!(is_ready(&ctx, &st, "u-1.2")); // overlapping scope does not block a successor
}

/// Scripted pool replies for dispatch: free slots, a task handle, success.
fn wire(host: &FakeHost) {
    host.reply(
        "pool status",
        0,
        "slot p1 slot-1 id free\nslot p1 slot-2 id free\n",
    );
    host.reply("task send", 0, "created hand 1a2b3c4d\n");
    host.reply("pool dispatch", 0, "dispatched 1a2b3c4d p1 slot-1 m1\n");
}

/// The recorded `pool dispatch` command lines.
fn dispatches(host: &FakeHost) -> Vec<String> {
    host.commands()
        .into_iter()
        .filter(|c| c.contains("pool dispatch"))
        .collect()
}

#[test]
fn chain_units_dispatch_from_their_predecessors_branch() {
    let (_dir, ctx, host) = setup();
    wire(&host);
    let mut st = chained_state();
    dispatch(&ctx, &mut st);
    let sent = dispatches(&host);
    assert_eq!(sent.len(), 1, "{sent:?}"); // u-1.2 waits; d waits on u-1
    assert!(
        sent[0].contains("--from unit/u-1-a1 --branch unit/u-1.1-a1"),
        "{}",
        sent[0]
    );
    assert_eq!(st.units["u-1.1"].status, Status::Inflight);
    assert_eq!(st.units["u-1.2"].status, Status::Pending);
    // Once u-1.1 delivers, u-1.2 starts from u-1.1's branch.
    st.units.get_mut("u-1.1").unwrap().status = Status::Delivered;
    dispatch(&ctx, &mut st);
    let sent = dispatches(&host);
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(
        sent[1].contains("--from unit/u-1.1-a1 --branch unit/u-1.2-a1"),
        "{}",
        sent[1]
    );
}

#[test]
fn an_ordinary_unit_still_dispatches_from_the_integration_branch() {
    let (_dir, ctx, host) = setup();
    wire(&host);
    let mut st = State::default();
    put(&mut st, "x", unit(Status::Pending, "src/x", None));
    dispatch(&ctx, &mut st);
    let sent = dispatches(&host);
    assert!(
        sent[0].contains("--from next --branch unit/x-a1"),
        "{sent:?}"
    );
}

#[test]
fn a_delivered_middle_unit_is_handed_on_and_only_the_last_is_merged() {
    let (_dir, ctx, _host) = setup();
    let (merger, _out, jobs) = Merger::detached();
    let mut st = chained_state();
    for (uid, branch) in [("u-1.1", "unit/u-1.1-a1"), ("u-1.2", "unit/u-1.2-a1")] {
        let u = st.units.get_mut(uid).unwrap();
        u.status = Status::Delivered;
        u.branch = Some(branch.to_owned());
    }
    st.units.get_mut("u-1.2").unwrap().proof = vec![String::from("./last-proof")];
    st.units
        .get_mut("u-1.1")
        .unwrap()
        .write_scope
        .push(String::from("src/extra"));
    merger.feed(&ctx, &mut st);
    // The middle unit is settled but not merged; it never reaches the merger.
    assert_eq!(st.units["u-1.1"].status, Status::Replaced);
    assert_eq!(st.units["u-1.1"].replaced_by, ["u-1.2"]);
    assert_eq!(st.units["u-1.2"].status, Status::Merging);
    let sent = jobs.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(sent.uid, "u-1.2");
    assert_eq!(sent.branch, "unit/u-1.2-a1");
    assert_eq!(sent.proof, ["./last-proof"]);
    // The merged branch holds the whole chain, so its scope covers it all.
    assert_eq!(sent.write_scope, ["src/a", "src/extra"]);
    assert!(jobs.try_recv().is_err());
}

#[test]
fn after_on_the_blocked_original_waits_for_the_last_unit_to_merge() {
    let (_dir, ctx, _host) = setup();
    let (merger, _out, _jobs) = Merger::detached();
    let mut st = chained_state();
    for (uid, branch) in [("u-1.1", "unit/u-1.1-a1"), ("u-1.2", "unit/u-1.2-a1")] {
        let u = st.units.get_mut(uid).unwrap();
        u.status = Status::Delivered;
        u.branch = Some(branch.to_owned());
    }
    assert!(!satisfied(&st, "u-1", 0));
    assert!(!is_ready(&ctx, &st, "d"));
    merger.feed(&ctx, &mut st); // u-1.1 handed on, u-1.2 merging
    assert!(!satisfied(&st, "u-1", 0));
    assert!(!is_ready(&ctx, &st, "d"));
    st.units.get_mut("u-1.2").unwrap().status = Status::Merged;
    assert!(satisfied(&st, "u-1", 0));
    assert!(is_ready(&ctx, &st, "d"));
}

#[test]
fn a_stuck_middle_unit_merges_nothing_and_satisfies_nothing() {
    let (_dir, ctx, _host) = setup();
    let (merger, _out, jobs) = Merger::detached();
    let mut st = chained_state();
    let first = st.units.get_mut("u-1.1").unwrap();
    first.status = Status::Delivered;
    first.branch = Some(String::from("unit/u-1.1-a1"));
    st.units.get_mut("u-1.2").unwrap().status = Status::Stuck;
    merger.feed(&ctx, &mut st);
    assert_eq!(st.units["u-1.1"].status, Status::Replaced);
    assert!(jobs.try_recv().is_err());
    assert!(!satisfied(&st, "u-1", 0));
    assert!(!is_ready(&ctx, &st, "d"));
}

#[test]
fn a_blocked_chain_unit_continues_again_and_what_followed_it_moves_to_the_end() {
    let (_dir, ctx, _host) = setup();
    let mut st = chained_state();
    // u-1.1 delivered and was handed on; u-1.2 ran and reported blocked.
    let first = st.units.get_mut("u-1.1").unwrap();
    first.status = Status::Replaced;
    first.replaced_by = vec![String::from("u-1.2")];
    first.branch = Some(String::from("unit/u-1.1-a1"));
    put(&mut st, "u-1.3", follow(Status::Pending, "src/a", "u-1.2"));
    let blocked = st.units.get_mut("u-1.2").unwrap();
    blocked.status = Status::Splitting;
    blocked.branch = Some(String::from("unit/u-1.2-a1"));
    blocked.blocked_branch = Some(String::from("unit/u-1.2-a1"));
    let mut again = plan(&["u-1.2.1", "u-1.2.2"]);
    again["replaces"] = json!("u-1.2");
    fs::write(
        ctx.home.join("plans").join("u-1.2.split.json"),
        again.to_string(),
    )
    .unwrap();
    ingest(&ctx, &mut st);
    assert_eq!(st.units["u-1.2"].status, Status::Replaced);
    assert_eq!(st.units["u-1.2.1"].continues.as_deref(), Some("u-1.2"));
    assert_eq!(st.units["u-1.2.2"].continues.as_deref(), Some("u-1.2.1"));
    // The unit that used to follow u-1.2 now follows the end of the new chain.
    assert_eq!(st.units["u-1.3"].continues.as_deref(), Some("u-1.2.2"));
    assert!(is_ready(&ctx, &st, "u-1.2.1"));
    assert!(!is_ready(&ctx, &st, "u-1.3"));
}

#[test]
fn a_timeout_split_inside_a_chain_keeps_the_chain_base() {
    let (_dir, ctx, _host) = setup();
    let mut st = chained_state();
    let first = st.units.get_mut("u-1.1").unwrap();
    first.status = Status::Replaced;
    first.replaced_by = vec![String::from("u-1.2")];
    first.branch = Some(String::from("unit/u-1.1-a1"));
    // u-1.2 timed out: no blocked branch, its partial work is discarded.
    st.units.get_mut("u-1.2").unwrap().status = Status::Splitting;
    let mut split = plan(&["u-1.2.1"]);
    split["replaces"] = json!("u-1.2");
    fs::write(
        ctx.home.join("plans").join("u-1.2.split.json"),
        split.to_string(),
    )
    .unwrap();
    ingest(&ctx, &mut st);
    assert_eq!(st.units["u-1.2.1"].continues.as_deref(), Some("u-1.1"));
}

/// Tick, pausing briefly for the merge thread, until `done` holds.
fn drive_until(ctx: &Ctx, st: &mut State, merger: &Merger, done: impl Fn(&State) -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        tick(ctx, st, merger);
        if done(st) {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_chain_merges_once_through_the_whole_tick_and_releases_the_dependent() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), config());
    wire(&host);
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d p1 slot-1 finished \"pushed branch sha\"\n",
    );
    host.reply(
        "refs/heads/unit/",
        0,
        "1111111111111111111111111111111111111111\n",
    );
    let mut st = chained_state();
    let merger = Merger::start(&ctx);
    let landed = drive_until(&ctx, &mut st, &merger, |st| {
        st.units["d"].status == Status::Merged
    });
    assert!(landed, "units: {:?}", st.counts());
    assert_eq!(st.units["u-1.1"].status, Status::Replaced);
    assert_eq!(st.units["u-1.2"].status, Status::Merged);
    assert_eq!(st.units["u-1"].status, Status::Replaced);
    let merge_cmds: Vec<String> = host
        .commands()
        .into_iter()
        .filter(|c| c.contains("merge --no-ff"))
        .collect();
    // One merge for the chain's last unit, one for the dependent; the
    // middle unit's branch is never merged.
    assert_eq!(merge_cmds.len(), 2, "{merge_cmds:?}");
    assert!(
        merge_cmds.iter().all(|c| !c.contains("unit/u-1.1-a1")),
        "{merge_cmds:?}"
    );
    assert!(
        merge_cmds.iter().any(|c| c.contains("unit/u-1.2-a1")),
        "{merge_cmds:?}"
    );
}
