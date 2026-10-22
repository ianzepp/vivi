//! Tests for [`crate::pump::boardpass`]: the merge-back rules on hand-built
//! snapshots, the interval and pending-sync handling, and one full pass
//! through the real worker thread driven by [`FakeHost`] replies.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};

use crate::pump::boardpass::{BoardPass, merge_back};
use crate::pump::config::{BoardConfig, PumpConfig};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, PoolJob};
use crate::pump::state::{NeedRecord, State};

/// A context with an operator sender.
fn setup() -> (TempDir, Ctx, Arc<FakeHost>) {
    let cfg = PumpConfig {
        board: BoardConfig {
            from: vec![String::from("operator@bd371d96.local")],
            ..BoardConfig::default()
        },
        branch: String::from("next"),
        ..PumpConfig::default()
    };
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg);
    (dir, ctx, host)
}

#[test]
fn merge_back_keeps_new_outbox_jobs_and_preserves_live_dispatch_progress() {
    let pending = |id: &str| {
        PoolJob::from(JobSpec::new(
            id,
            "planner:board",
            "planner",
            format!("plan {id}"),
            "plan this need",
        ))
    };
    let mut snap = State::default();
    snap.jobs
        .insert(String::from("planner-live"), pending("planner-live"));
    snap.jobs
        .insert(String::from("planner-updated"), pending("planner-updated"));

    let mut live = snap.clone();
    let dispatched = live.jobs.get_mut("planner-live").unwrap();
    dispatched.status = JobStatus::Dispatched;
    dispatched.pool = Some(String::from("test-planner-pool"));
    dispatched.task = Some(String::from("task-1"));
    dispatched.dispatched_at = Some(101);

    let mut done = snap.clone();
    let worker_update = done.jobs.get_mut("planner-updated").unwrap();
    worker_update.status = JobStatus::Finished;
    worker_update.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::from("agent exit 0"),
        receipt: String::new(),
        at: 102,
    });
    let new_job = pending("planner-new");
    done.jobs.insert(new_job.id.clone(), new_job.clone());
    // A stale worker snapshot must not replace newer dispatch state.
    done.jobs.get_mut("planner-live").unwrap().status = JobStatus::Lost;

    merge_back(&mut live, &snap, &done);

    assert_eq!(live.jobs.get("planner-new"), Some(&new_job));
    assert_eq!(live.jobs["planner-updated"].status, JobStatus::Finished);
    assert_eq!(live.jobs["planner-live"].status, JobStatus::Dispatched);
    assert_eq!(live.jobs["planner-live"].task.as_deref(), Some("task-1"));
    assert_eq!(
        live.jobs["planner-live"].pool.as_deref(),
        Some("test-planner-pool")
    );
}

/// One JSON row for a `need list` reply.
fn row(handle: &str, date: &str, subject: &str, marks: &[&str]) -> String {
    let marks = marks
        .iter()
        .map(|mark| format!("\"{mark}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{{\"handle\": \"{handle}\", \"date\": \"{date}\", \"subject\": \"{subject}\", \"marks\": [{marks}]}}"
    )
}

/// A JSON array reply wrapping `rows`.
fn list(rows: &[String]) -> String {
    format!("[{}]", rows.join(", "))
}

/// An operator thread header line and body, as `need show` prints them.
fn thread(stamp: &str, hex: &str, from: &str, body: &str) -> String {
    format!("## {stamp} - {hex} [open]\nFrom: {from}\n\n{body}\n")
}

/// A need record with a verdict, a reason and no units.
fn record(verdict: &str) -> NeedRecord {
    NeedRecord {
        verdict: String::from(verdict),
        reason: String::from("one unit"),
        ..NeedRecord::default()
    }
}

#[test]
fn merge_back_applies_worker_removals_additions_and_replies() {
    let mut st = State::default();
    st.needs.insert(String::from("a"), record("split"));
    st.needs.insert(String::from("b"), record("single"));
    // Added by the tick thread during the pass: the worker never saw it.
    st.needs.insert(String::from("added"), record("parker"));
    st.plans_seen = vec![String::from("a.json"), String::from("c.json")];
    st.board.at = 100;
    st.board.planning.insert(String::from("p1"), 10);
    st.board.replied.push(String::from("n1:planned"));
    st.board.revision_planning.insert(String::from("r1"), 5);

    let mut snap = State::default();
    snap.needs.insert(String::from("a"), record("split"));
    snap.needs.insert(String::from("b"), record("single"));
    snap.plans_seen = vec![String::from("a.json")];
    snap.board.at = 100;
    snap.board.planning.insert(String::from("p1"), 10);
    snap.board.replied.push(String::from("n1:planned"));
    snap.board.revision_planning.insert(String::from("r1"), 5);

    let mut done = State::default();
    done.needs.insert(String::from("b"), record("single"));
    done.plans_seen = Vec::new();
    done.board.at = 500;
    // `p1` expired during the pass, `p2` and `n9` launched by it.
    done.board.planning.insert(String::from("p2"), 20);
    done.board.planning.insert(String::from("n9"), 30);
    done.board.replied.push(String::from("n1:planned"));
    done.board.replied.push(String::from("n2:planned"));
    done.board.revision_planning.insert(String::from("r2"), 7);

    merge_back(&mut st, &snap, &done);

    assert!(!st.needs.contains_key("a"), "worker dropped `a`");
    assert!(st.needs.contains_key("b"));
    assert!(st.needs.contains_key("added"), "main's need must survive");
    assert_eq!(st.plans_seen, vec![String::from("c.json")]);
    assert_eq!(st.board.planning.len(), 2);
    assert_eq!(st.board.planning.get("p2"), Some(&20));
    assert_eq!(st.board.planning.get("n9"), Some(&30));
    assert_eq!(
        st.board.replied,
        vec![String::from("n1:planned"), String::from("n2:planned")]
    );
    assert_eq!(st.board.revision_planning.len(), 1);
    assert_eq!(st.board.revision_planning.get("r2"), Some(&7));
    assert_eq!(st.board.at, 500);
}

#[test]
fn merge_back_copies_amendment_fields_but_not_main_side_changes() {
    let mut planned = record("split");
    planned.thread_seen_at = Some(100);
    planned.thread_sha256 = Some(String::from("aaaa"));
    planned.units = vec![String::from("u1")];
    planned.revision = 1;

    let mut st = State::default();
    st.needs.insert(String::from("n1"), planned.clone());
    // The tick thread replanned during the pass.
    let live = st.needs.get_mut("n1").unwrap();
    live.units.push(String::from("u2"));
    live.revision = 2;

    let snap = st.clone();

    let mut done = State::default();
    let mut amended = planned;
    amended.thread_seen_at = Some(200);
    amended.thread_sha256 = Some(String::from("bbbb"));
    amended.amendment_pending = true;
    // A worker-side change to a field main owns must not land.
    amended.units.push(String::from("u9"));
    amended.verdict = String::from("hold");
    done.needs.insert(String::from("n1"), amended);

    merge_back(&mut st, &snap, &done);

    let merged = st.needs.get("n1").unwrap();
    assert_eq!(merged.revision, 2);
    assert_eq!(
        merged.units,
        vec![String::from("u1"), String::from("u2")],
        "the worker's unit change must not land"
    );
    assert_eq!(merged.verdict, "split");
    assert_eq!(merged.thread_seen_at, Some(200));
    assert_eq!(merged.thread_sha256.as_deref(), Some("bbbb"));
    assert!(merged.amendment_pending);
}

#[test]
fn merge_back_skips_launching_a_need_planned_during_the_pass() {
    let mut st = State::default();
    // Entered the live state after the snapshot was taken.
    st.needs.insert(String::from("late"), record("split"));
    let snap = State::default();

    let mut done = State::default();
    done.board.planning.insert(String::from("late"), 30);

    merge_back(&mut st, &snap, &done);

    assert!(st.board.planning.is_empty());
}

#[test]
fn request_respects_the_interval_and_consumes_sync_pending() {
    let (_dir, ctx, _host) = setup();
    let (mut pass, requests, _out) = BoardPass::detached();
    let mut st = State::default();
    st.board.at = ctx.now();
    st.board.sync_pending = true;

    pass.request(&ctx, &mut st);
    assert!(
        requests.try_recv().is_err(),
        "a fresh board.at means no request"
    );
    assert!(
        st.board.sync_pending,
        "nothing was shipped, so nothing is taken"
    );

    st.board.at = ctx.now().saturating_sub(ctx.cfg.board.every + 1);
    pass.request(&ctx, &mut st);
    let (sent_ctx, sent) = requests.try_recv().unwrap();
    assert_eq!(sent.board.at, 0);
    assert!(sent.board.sync_pending, "the clone carries the push");
    assert_eq!(sent_ctx.cfg.board.from, ctx.cfg.board.from);
    assert!(!st.board.sync_pending, "the live flag is consumed");

    // A pass is in flight: a second request is dropped.
    pass.request(&ctx, &mut st);
    assert!(requests.try_recv().is_err());
}

#[test]
fn a_full_pass_round_trips_through_the_worker_thread() {
    let (_dir, ctx, host) = setup();
    host.reply("sync pull", 0, "pulled 1 message");
    host.reply(
        "pump list --status open --json",
        0,
        "[{\"handle\": \"p1\", \"need\": \"n1\", \"status\": \"open\"}]",
    );
    host.reply("--held", 0, "[]");
    host.reply(
        "--json --to mind",
        0,
        &list(&[row("n1", "2026-10-15T00:00", "Plan me", &[])]),
    );
    host.reply(
        "need show n1",
        0,
        &thread(
            "2026-10-15T00:00:00Z",
            "f00d01",
            "operator@bd371d96.local",
            "the need",
        ),
    );

    let mut pass = BoardPass::start(&ctx);
    let mut st = State::default();
    st.board.at = 0;
    pass.request(&ctx, &mut st);

    let deadline = Instant::now() + Duration::from_secs(10);
    while st.board.at == 0 && Instant::now() < deadline {
        pass.apply(&ctx, &mut st);
        std::thread::sleep(Duration::from_millis(5));
    }

    assert_eq!(st.board.at, ctx.now());
    assert!(st.board.planning.contains_key("n1"));
    let job = st
        .jobs
        .values()
        .find(|job| job.kind == "planner:board")
        .expect("worker planner outbox entry was merged into the live state");
    assert_eq!(job.role, "pump-planner");
    assert_eq!(job.pool_candidates, vec![String::from("test-planner-pool")]);
    assert_eq!(job.status, JobStatus::Pending);
    assert_eq!(job.payload["planner_output"]["out"], "board/n1.out");
    assert!(job.body.contains("PUMP-NEED-INPUT-BEGIN"));
    assert!(job.body.contains("PUMP-PLAN-BEGIN"));
    assert!(host.spawned().is_empty(), "pool owns planner execution");
}

#[test]
fn a_board_pass_uses_the_config_sent_on_the_request() {
    let (_dir, mut ctx, host) = setup();
    let mut pass = BoardPass::start(&ctx);
    let mut cfg = (*ctx.cfg).clone();
    cfg.reply_from = String::from("reloaded");
    ctx.cfg = Arc::new(cfg);
    host.reply("sync pull", 0, "pulled 1 message");
    host.reply(
        "pump list --status open --json",
        0,
        "[{\"handle\": \"p1\", \"need\": \"n1\", \"status\": \"open\"}]",
    );
    host.reply("--held", 0, "[]");
    host.reply(
        "--json --to reloaded",
        0,
        &list(&[row("n1", "2026-10-15T00:00", "Plan me", &[])]),
    );
    host.reply(
        "need show n1",
        0,
        &thread(
            "2026-10-15T00:00:00Z",
            "f00d01",
            "operator@bd371d96.local",
            "the need",
        ),
    );
    let mut st = State::default();
    st.board.at = 0;
    pass.request(&ctx, &mut st);

    let deadline = Instant::now() + Duration::from_secs(10);
    while st.board.at == 0 && Instant::now() < deadline {
        pass.apply(&ctx, &mut st);
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(
        st.board.planning.contains_key("n1"),
        "the reloaded reply_from found the need; commands: {:?}",
        host.commands()
    );
    assert!(
        host.commands()
            .iter()
            .any(|cmd| cmd.contains("need list --status open --json --to reloaded")),
        "reloaded reply_from unused; commands: {:?}",
        host.commands()
    );
    assert!(
        !host
            .commands()
            .iter()
            .any(|cmd| cmd.contains("--json --to mind")),
        "startup reply_from leaked; commands: {:?}",
        host.commands()
    );
}
