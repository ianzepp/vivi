//! Tests for [`crate::pump::boardpass`]: the merge-back rule, the interval and
//! pending-sync handling, and one full pass
//! through the real worker thread driven by [`FakeHost`] replies.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};

use crate::pump::boardpass::{BoardPass, merge_back};
use crate::pump::config::{BoardConfig, PumpConfig};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::State;

/// A context with an operator sender.
fn setup() -> (TempDir, Ctx, Arc<FakeHost>) {
    let cfg = PumpConfig {
        board: BoardConfig {
            from: vec![String::from("operator@bd371d96.local")],
            ..BoardConfig::default()
        },
        ..PumpConfig::default()
    };
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg);
    (dir, ctx, host)
}

#[test]
fn merge_back_copies_only_the_interval_stamp() {
    let mut live = State::default();
    live.board.at = 100;
    live.board.sync_pending = true;
    live.board.replied.push(String::from("live:planned"));

    let mut done = State::default();
    done.board.at = 500;
    done.board.replied.push(String::from("worker:planned"));

    merge_back(&mut live, &done);

    assert_eq!(live.board.at, 500);
    assert!(live.board.sync_pending, "the live push flag stays");
    assert_eq!(live.board.replied, vec![String::from("live:planned")]);
    assert!(live.jobs.is_empty());
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
    assert_eq!(host.commands().len(), 1, "{:?}", host.commands());
    assert!(host.commands()[0].contains("sync pull"));
    assert!(st.jobs.is_empty(), "the board queues no job");
    assert!(host.spawned().is_empty());
}

#[test]
fn a_board_pass_uses_the_config_sent_on_the_request() {
    let (_dir, mut ctx, host) = setup();
    let mut pass = BoardPass::start(&ctx);
    let mut cfg = (*ctx.cfg).clone();
    cfg.board.pull = false;
    ctx.cfg = Arc::new(cfg);
    let mut st = State::default();
    st.board.at = 0;
    pass.request(&ctx, &mut st);

    let deadline = Instant::now() + Duration::from_secs(10);
    while st.board.at == 0 && Instant::now() < deadline {
        pass.apply(&ctx, &mut st);
        std::thread::sleep(Duration::from_millis(5));
    }

    assert_eq!(st.board.at, ctx.now(), "the pass ran");
    assert!(
        host.commands().is_empty(),
        "the startup config pulled; commands: {:?}",
        host.commands()
    );
}
