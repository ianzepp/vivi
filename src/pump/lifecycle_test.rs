//! Whole-lifecycle tests for the pump state machine.
//!
//! The per-module tests cover each step against its own seams; these drive
//! whole units through [`crate::pump::run::tick`] with a real merge thread
//! ([`Merger::start`]) behind a [`FakeHost`], so dispatch, collect, merge
//! and gate transitions happen through the same command lines a running
//! pump uses. The fake host answers unscripted git commands with exit 0 and
//! empty text, so the merge thread's `rev-parse` yields an empty tip — these
//! tests assert state, not commit identity.

use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::tempdir;

use crate::pump::boardpass::BoardPass;
use crate::pump::config::{BoardConfig, DispatchConfig, GateConfig, Lane, MergeConfig, PumpConfig};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::merge::Merger;
use crate::pump::run::tick as run_tick;
use crate::pump::state::{NeedRecord, State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// The fake clock's start.
const T0: u64 = 1_000_000;
/// Seconds a test waits for the asynchronous merge thread.
const DEADLINE_SECS: u64 = 5;
/// A 40-hex integration tip a gate can start from.
const TIP: &str = "0123456789abcdef0123456789abcdef01234567";
/// The task handle the scripted gate mints.
const GATE_TASK: &str = "99887766";
/// A red gate name carries its crate as the first field for hold filtering.
const RED_NAME: &str = "vivi crate::a::t1";

fn tick(ctx: &Ctx, st: &mut State, merger: &Merger) {
    let (mut pass, requests, results) = BoardPass::detached();
    merger.apply(ctx, st);
    run_tick(ctx, st, &mut pass).unwrap();
    crate::pump::jobs::dispatch_pending(ctx, st).unwrap();
    merger.apply(ctx, st);
    merger.feed(ctx, st);
    pass.serve(ctx, st, &requests, &results);
    crate::pump::run::write_status(ctx, st, &[]);
}
/// A second plan file, ingested mid-test to mint a fresh unit.
const SECOND_PLAN: &str = concat!(
    "{\"need\": \"ny\", \"verdict\": \"units\", \"reason\": \"a second need\", ",
    "\"units\": [{\"id\": \"y-b1\", \"title\": \"second unit\", ",
    "\"brief\": \"do b\", \"write_scope\": [\"vivi/src/b.rs\"], ",
    "\"proof\": [], \"tools\": \"plain\"}]}"
);

/// The brief's config: integration branch `next`, one `m1` lane of five,
/// three attempts, pool `p1` for units and gates, no shared checks, and a
/// board with no senders, no mark and no pull, so ticks stay on the units.
fn config() -> PumpConfig {
    PumpConfig {
        branch: String::from("next"),
        board: BoardConfig {
            from: Vec::new(),
            mark: String::new(),
            pull: false,
            ..BoardConfig::default()
        },
        dispatch: DispatchConfig {
            lanes: vec![Lane {
                model: String::from("m1"),
                cap: 5,
            }],
            max_attempts: 3,
            kinds: std::collections::BTreeMap::from([(
                String::from("plain"),
                crate::pump::config::Kind {
                    pools: vec![String::from("p1")],
                    ..Default::default()
                },
            )]),
            default_kind: String::from("plain"),
            ..DispatchConfig::default()
        },
        merge: MergeConfig {
            shared_checks: Vec::new(),
            ..MergeConfig::default()
        },
        gate: GateConfig {
            pools: vec![String::from("p1")],
            ..GateConfig::default()
        },
        ..PumpConfig::default()
    }
}

/// A pending unit of `need` that may write only `scope`.
fn unit(need: &str, scope: &str) -> Unit {
    Unit {
        need: need.to_owned(),
        title: String::from("a unit"),
        brief: String::from("do the work"),
        write_scope: vec![scope.to_owned()],
        proof: Vec::new(),
        tools: String::from("plain"),
        since: T0,
        ..Unit::default()
    }
}

/// Add `uid` to the state's order and units.
fn put(st: &mut State, uid: &str, unit: Unit) {
    st.order.push(uid.to_owned());
    st.units.insert(uid.to_owned(), unit);
}

/// Record `uids` as the planned units of need `handle`.
fn record_need(st: &mut State, handle: &str, uids: &[&str]) {
    st.needs.insert(
        handle.to_owned(),
        NeedRecord {
            verdict: String::from("units"),
            reason: String::new(),
            units: uids.iter().map(|uid| (*uid).to_owned()).collect(),
            ..NeedRecord::default()
        },
    );
}

/// The scripted pool world: one free `p1` slot, a minted task handle, a
/// successful dispatch, and a finished reconcile line for that handle.
fn world(host: &FakeHost) {
    host.reply("merge-base --is-ancestor origin/unit/", 1, "");
    host.reply("pool status", 0, "slot p1 slot-1 id free x y z\n");
    host.reply("task send", 0, "created hand 1a2b3c4d\n");
    host.reply(
        "pool dispatch",
        0,
        "dispatched 1a2b3c4d p1 slot-1 job m1 x y next unit/x-a1 600 write 0 -\n",
    );
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d p1 slot-1 finished \"pushed branch sha\"\n",
    );
    host.reply("task show", 0, "agent exit 0 after 1s\n");
}

/// How many recorded command lines contain `fragment`.
fn count(host: &FakeHost, fragment: &str) -> usize {
    host.commands()
        .iter()
        .filter(|cmd| cmd.contains(fragment))
        .count()
}

/// Whether `uid` sits in `status` right now.
fn sits(st: &State, uid: &str, status: Status) -> bool {
    st.units.get(uid).is_some_and(|u| u.status == status)
}

/// Tick, pausing briefly for the merge thread, until `done` holds or the
/// deadline passes; the return value is whether `done` held.
fn drive_until(ctx: &Ctx, st: &mut State, merger: &Merger, done: impl Fn(&State) -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(DEADLINE_SECS);
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
fn a_unit_travels_pending_inflight_delivered_merged_and_closes_its_need() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), config());
    world(&host);
    // The worker's branch reaches the remote once the task finishes.
    host.reply(
        "refs/heads/unit/",
        0,
        "1111111111111111111111111111111111111111\n",
    );
    let mut st = State::default();
    put(&mut st, "x-a1", unit("n1", "src/a.rs"));
    record_need(&mut st, "n1", &["x-a1"]);
    let merger = Merger::start(&ctx);

    tick(&ctx, &mut st, &merger); // the dispatch pass
    let x = st.units.get("x-a1").unwrap();
    assert_eq!(x.status, Status::Inflight);
    assert_eq!(x.task.as_deref(), Some("1a2b3c4d"));
    assert_eq!(x.branch.as_deref(), Some("unit/x-a1-a1"));
    assert_eq!((x.attempts, x.pool.as_deref()), (1, Some("p1")));
    assert_eq!(count(&host, "task send"), 1);

    tick(&ctx, &mut st, &merger); // collect: finished task, branch on remote
    assert!(host.logs().iter().any(|l| l.contains("delivered x-a1")));

    assert!(
        drive_until(&ctx, &mut st, &merger, |st| sits(
            st,
            "x-a1",
            Status::Merged
        )),
        "x-a1 never merged; units: {:?}",
        st.counts()
    );
    assert_eq!(st.merges, 1);
    assert_eq!(st.units.get("x-a1").unwrap().outside, Vec::<String>::new());
    assert!(st.closed_needs.iter().any(|n| n == "n1"));
    let ready = fs::read_to_string(ctx.home.join("needs-ready.txt")).unwrap();
    assert!(ready.contains("n1") && ready.contains("x-a1"), "{ready}");
    assert_eq!(count(&host, "git push origin HEAD:refs/heads/next"), 1);
    let status = fs::read_to_string(ctx.home.join("status.txt")).unwrap();
    assert!(status.contains("units merged=1"), "{status}");
}

#[test]
fn a_failing_unit_is_retried_on_a_new_branch_and_is_stuck_after_max_attempts() {
    let dir = tempdir().unwrap();
    let mut cfg = config();
    cfg.dispatch.last_resort = String::from("m1"); // the final attempt's lane
    let (ctx, host) = test_ctx(dir.path(), cfg);
    world(&host);
    // No remote branch ever appears and the agent always fails.
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d p1 slot-1 failed \"agent gave up\"\n",
    );
    let mut st = State::default();
    put(&mut st, "x-a1", unit("n1", "src/a.rs"));
    record_need(&mut st, "n1", &["x-a1"]);
    let merger = Merger::start(&ctx);

    for attempt in 1_u32..=3 {
        tick(&ctx, &mut st, &merger); // collect fails, dispatch retries
        assert_eq!(count(&host, "task send"), attempt as usize);
        let x = st.units.get("x-a1").unwrap();
        assert_eq!(x.status, Status::Inflight);
        assert_eq!(x.attempts, attempt);
        assert_eq!(x.branch, Some(format!("unit/x-a1-a{attempt}")));
    }
    tick(&ctx, &mut st, &merger); // the third failure exhausts max_attempts
    let x = st.units.get("x-a1").unwrap();
    assert_eq!(x.status, Status::Stuck);
    assert_eq!(x.attempts, 3);
    assert_eq!(x.history.len(), 3);
    assert!(x.history.iter().all(|h| h.why == "failed/agent gave up"));
    assert_eq!(x.task, None);
    for attempt in 1..=3 {
        let branch = format!("--branch unit/x-a1-a{attempt}");
        assert!(
            host.commands().iter().any(|c| c.contains(&branch)),
            "no dispatch on {branch}"
        );
    }
    assert_eq!(st.merges, 0);
    assert!(st.closed_needs.is_empty());
    assert!(!ctx.home.join("needs-ready.txt").exists());
    let exc = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exc.contains("stuck x-a1"), "{exc}");
}

#[test]
fn a_red_gate_starts_an_auditor_and_holds_non_fix_merges_until_a_clean_gate() {
    let dir = tempdir().unwrap();
    let mut cfg = config();
    cfg.gate.command = String::from("./gate-run");
    cfg.gate.model = String::from("m1");
    let (ctx, host) = test_ctx(dir.path(), cfg);
    world(&host);
    host.reply("refs/heads/unit/", 0, &format!("{TIP}\n"));
    let mut st = State::default();
    put(&mut st, "x-a1", unit("n1", "src/a.rs"));
    record_need(&mut st, "n1", &["x-a1"]);
    let merger = Merger::start(&ctx);

    tick(&ctx, &mut st, &merger); // dispatch x-a1
    assert_eq!(st.units.get("x-a1").unwrap().status, Status::Inflight);
    // Now the integration tip exists, the gate task mints, and the pool
    // reports both tasks finished.
    host.reply("refs/heads/next", 0, &format!("{TIP}\n"));
    host.reply("pump-job:gate", 0, &format!("created hand {GATE_TASK}\n"));
    host.reply("[pump audit]", 0, "created hand aabbccdd\n");
    host.reply(
        "pool reconcile",
        0,
        &format!(
            "collected {GATE_TASK} p1 slot-1 finished \"gate reply\"\n\
             collected 1a2b3c4d p1 slot-1 finished \"pushed branch sha\"\n"
        ),
    );
    // The first gate reply carries an empty marker block: the baseline gate.
    host.reply_once(
        "task show 99887766",
        0,
        "agent exit 0 after 1s\nFAILING-BEGIN\n\nFAILING-END\n",
    );
    // Every later gate is green until the red reply below replaces this rule:
    // the batch rule re-gates right after the merge, and that reply must be
    // deterministic.
    host.reply(
        "task show 99887766",
        0,
        "agent exit 0 after 1s\nFAILING-BEGIN\n\nFAILING-END\n",
    );

    tick(&ctx, &mut st, &merger); // deliver x-a1, feed it, start the gate
    assert!(
        drive_until(&ctx, &mut st, &merger, |st| sits(
            st,
            "x-a1",
            Status::Merged
        )),
        "x-a1 never merged; units: {:?}",
        st.counts()
    );
    assert_eq!(st.merges, 1);
    assert_eq!(st.baseline, Some(Vec::new())); // the baseline gate finished
    // The batch rule: the merger runs before the gates, so the merge that
    // just landed is already one past the gate's start and a re-gate is due.
    assert!(
        drive_until(&ctx, &mut st, &merger, |st| st.gates.len() == 2
            && st.last_gate.task.is_none()),
        "the post-merge re-gate never finished; finished gates: {}",
        st.gates.len()
    );
    assert_eq!(st.gates.len(), 2); // baseline plus one post-merge re-gate
    assert_eq!(st.last_gate.task, None);
    assert_eq!(st.red.len(), 0); // the green re-gate owns nothing

    // A red gate: force one, answer with a name the baseline never had.
    host.reply(
        "task show 99887766",
        0,
        "agent exit 0 after 1s\nFAILING-BEGIN\nvivi crate::a::t1\nFAILING-END\n",
    );
    fs::write(ctx.home.join("FORCE-GATE"), b"").unwrap();
    tick(&ctx, &mut st, &merger); // the forced gate starts
    assert!(st.last_gate.job_id.is_some());
    let gate_job = st
        .jobs
        .get(st.last_gate.job_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(gate_job.task.as_deref(), Some(GATE_TASK));
    assert!(!ctx.home.join("FORCE-GATE").exists());
    tick(&ctx, &mut st, &merger); // the forced red gate finishes
    assert_eq!(st.gates.len(), 3);
    let owner = "pumpfix-g01234567";
    assert_eq!(st.red.get(RED_NAME).map(String::as_str), Some(owner));
    let audits: Vec<String> = host
        .commands()
        .into_iter()
        .filter(|cmd| cmd.contains("task send") && cmd.contains("[pump audit]"))
        .collect();
    assert_eq!(audits.len(), 1, "{audits:?}");
    assert!(audits[0].contains("[pump audit] 1 failures at 0123456789"));
    let dispatches: Vec<String> = host
        .commands()
        .into_iter()
        .filter(|cmd| cmd.contains("pool dispatch") && cmd.contains("aabbccdd"))
        .collect();
    assert_eq!(
        dispatches.len(),
        1,
        "the auditor job is dispatched: {dispatches:?}"
    );
    assert!(host.spawned().is_empty(), "no planner process starts");
    let hold = ctx.home.join("HOLD-MERGE");
    assert!(hold.is_file(), "a red gate holds the merge queue");
    set_mtime(&hold, 999_999);
    let exc = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exc.contains("gate-new-failures"), "{exc}");

    // A fresh unit can deliver, but the red gate holds it from merging.
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    fs::write(ctx.home.join("plans").join("ny.json"), SECOND_PLAN).unwrap();
    tick(&ctx, &mut st, &merger); // ingest mints y-b1, dispatch runs
    assert_eq!(st.units.get("y-b1").unwrap().status, Status::Inflight);
    assert!(drive_until(&ctx, &mut st, &merger, |st| sits(
        st,
        "y-b1",
        Status::Delivered
    )));
    tick(&ctx, &mut st, &merger);
    tick(&ctx, &mut st, &merger);
    assert_eq!(st.merges, 1);
    assert!(!sits(&st, "y-b1", Status::Merged));

    // A clean forced gate after the fix releases the hold and pending work.
    host.reply(
        "task show 99887766",
        0,
        "agent exit 0 after 1s\nFAILING-BEGIN\n\nFAILING-END\n",
    );
    fs::write(ctx.home.join("FORCE-GATE"), b"").unwrap();
    tick(&ctx, &mut st, &merger);
    assert!(
        drive_until(&ctx, &mut st, &merger, |st| st.gates.len() == 4
            && st.last_gate.task.is_none()),
        "the clean gate never finished; gates: {}",
        st.gates.len()
    );
    assert!(
        drive_until(&ctx, &mut st, &merger, |st| sits(
            st,
            "y-b1",
            Status::Merged
        )),
        "y-b1 never merged after the clean gate; units: {:?}",
        st.counts()
    );
    assert_eq!(st.merges, 2);
    assert!(!hold.exists());
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("hold lifted: gate at 0123456789 is clean")),
        "{:?}",
        host.logs()
    );
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains(&format!("red fixed: {RED_NAME} (owner pumpfix-g01234567)")))
    );
    assert!(st.red.is_empty());
    assert!(host.spawned().is_empty());
    assert_eq!(
        host.commands()
            .iter()
            .filter(|cmd| cmd.contains("task send") && cmd.contains("[pump audit]"))
            .count(),
        1
    );
    let exc = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert_eq!(exc.matches("gate-new-failures").count(), 1, "{exc}");
    assert!(st.closed_needs.iter().any(|n| n == "ny"));
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

#[test]
fn paused_control_stops_dispatch_but_a_delivered_unit_still_merges() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), config());
    world(&host);
    let mut st = State::default();
    put(&mut st, "x-a1", unit("n1", "src/a.rs"));
    let mut y = unit("n2", "src/b.rs");
    y.status = Status::Delivered;
    y.branch = Some(String::from("unit/y-b1-a1"));
    put(&mut st, "y-b1", y);
    record_need(&mut st, "n1", &["x-a1"]);
    record_need(&mut st, "n2", &["y-b1"]);
    let mut control = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    control.initialize_control("paused").unwrap();
    let merger = Merger::start(&ctx);

    tick(&ctx, &mut st, &merger);
    // The pause return sits after the first feed: y-b1 is already queued
    // for the merge thread, while nothing dispatches and no pool is even
    // asked for slots.
    assert_eq!(st.units.get("y-b1").unwrap().status, Status::Merging);
    assert_eq!(st.units.get("x-a1").unwrap().status, Status::Pending);
    assert_eq!(count(&host, "task send"), 0);
    assert_eq!(count(&host, "pool status"), 0);
    assert!(dir.path().join("home/status.txt").is_file());

    assert!(drive_until(&ctx, &mut st, &merger, |st| sits(
        st,
        "y-b1",
        Status::Merged
    )));
    assert_eq!(st.merges, 1);
    assert_eq!(st.units.get("x-a1").unwrap().status, Status::Pending);
    assert!(st.closed_needs.iter().any(|n| n == "n2"));
    assert_eq!(count(&host, "task send"), 0); // still paused, still silent
    assert_eq!(count(&host, "git push origin HEAD:refs/heads/next"), 1);
}

#[test]
fn a_dependent_waits_for_merge_but_overlap_does_not_hold_a_delivered_unit() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), config());
    world(&host);
    // Distinct task handles per send: the prerequisite, the overlapping unit,
    // then the dependent whose prerequisite must merge first.
    host.reply_once("task send", 0, "created hand aaaaaaaa\n");
    host.reply_once("task send", 0, "created hand bbbbbbbb\n");
    host.reply("task send", 0, "created hand cccccccc\n");
    host.reply(
        "refs/heads/unit/",
        0,
        "3333333333333333333333333333333333333333\n",
    );
    host.reply(
        "pool reconcile",
        0,
        concat!(
            "collected aaaaaaaa p1 slot-1 finished \"pushed a\"\n",
            "collected bbbbbbbb p1 slot-1 finished \"pushed b\"\n",
            "collected cccccccc p1 slot-1 finished \"pushed c\"\n",
        ),
    );
    let mut st = State::default();
    put(&mut st, "x-a1", unit("n1", "src/a.rs"));
    let mut y = unit("n2", "src/b.rs");
    y.after = vec![String::from("x-a1")];
    put(&mut st, "y-b1", y);
    put(&mut st, "z-c1", unit("n3", "src/a.rs")); // overlaps x-a1's scope
    record_need(&mut st, "n1", &["x-a1"]);
    record_need(&mut st, "n2", &["y-b1"]);
    record_need(&mut st, "n3", &["z-c1"]);
    let merger = Merger::start(&ctx);

    tick(&ctx, &mut st, &merger); // x starts; z waits while x is inflight
    assert_eq!(st.units.get("x-a1").unwrap().status, Status::Inflight);
    assert_eq!(st.units.get("y-b1").unwrap().status, Status::Pending);
    assert_eq!(st.units.get("z-c1").unwrap().status, Status::Pending);
    assert_eq!(count(&host, "task send"), 1);

    tick(&ctx, &mut st, &merger); // x-a1 delivered and queued for merge
    assert!(host.logs().iter().any(|l| l.contains("delivered x-a1")));
    // Delivered is not satisfied, so y still waits for the merge. z can start
    // now because no agent is editing x's scope and integration is serialized.
    assert_eq!(st.units.get("y-b1").unwrap().status, Status::Pending);
    assert_eq!(st.units.get("z-c1").unwrap().status, Status::Inflight);
    assert_eq!(count(&host, "task send"), 2);

    let all_merged = |st: &State| {
        ["x-a1", "y-b1", "z-c1"]
            .iter()
            .all(|uid| sits(st, uid, Status::Merged))
    };
    assert!(drive_until(&ctx, &mut st, &merger, all_merged));
    assert_eq!(st.merges, 3);
    assert_eq!(st.closed_needs.len(), 3);
    // The dependent starts after the prerequisite merges; the overlapping
    // unit starts as soon as the prerequisite worker delivers its branch.
    let branches: Vec<String> = host
        .commands()
        .iter()
        .filter(|c| c.contains("--branch unit/"))
        .cloned()
        .collect();
    assert_eq!(branches.len(), 3, "{branches:?}");
    assert!(branches[0].contains("unit/x-a1-a1"), "{branches:?}");
    assert!(branches[1].contains("unit/z-c1-a1"), "{branches:?}");
    assert!(branches[2].contains("unit/y-b1-a1"), "{branches:?}");
}
