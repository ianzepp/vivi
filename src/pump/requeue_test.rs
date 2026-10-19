//! Tests for [`crate::pump::requeue`]: operator requests applied to state,
//! with the attempt counter kept clear of branches that already exist.

use std::sync::Arc;

use tempfile::tempdir;

use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::requeue::apply_pending;
use crate::pump::state::{Attempt, State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// The fake clock's start.
const T0: u64 = 1_000_000;

/// A context with default config, home at `dir/home`, and a paused clock.
fn setup() -> (tempfile::TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.set_now(T0);
    (dir, ctx, host)
}

/// Queue one requeue request in the pump store.
fn request(ctx: &Ctx, uid: &str, attempts: Option<i64>, fresh: bool, refund: bool, reason: &str) {
    let mut store = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    store
        .add_requeue_request(uid, attempts, fresh, refund, reason, T0)
        .unwrap();
}

/// Put one unit into `st`.
fn put(st: &mut State, uid: &str, unit: Unit) {
    st.units.insert(uid.to_owned(), unit);
}

/// True when `line` was logged exactly.
fn logged(host: &FakeHost, line: &str) -> bool {
    host.logs().iter().any(|entry| entry == line)
}

#[test]
fn requeue_applies_a_stuck_unit() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "ls-remote",
        0,
        "aaa\trefs/heads/unit/u1-a1\nbbb\trefs/heads/unit/u1-a2\n",
    );
    let mut st = State::default();
    put(
        &mut st,
        "u1",
        Unit {
            status: Status::Stuck,
            attempts: 1,
            task: Some(String::from("task-1")),
            held_on: Some(String::from("u0")),
            since: 1,
            ..Unit::default()
        },
    );
    request(&ctx, "u1", None, false, false, "operator asked");

    apply_pending(&ctx, &mut st);

    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Pending);
    assert_eq!(unit.attempts, 2);
    assert_eq!(unit.task, None);
    assert_eq!(unit.held_on, None);
    assert_eq!(unit.since, T0);
    assert!(logged(&host, "requeue u1 by operator: operator asked"));

    // The request is consumed: a second tick changes nothing.
    let before = host.logs().len();
    apply_pending(&ctx, &mut st);
    assert_eq!(host.logs().len(), before);
    assert_eq!(st.units["u1"].attempts, 2);
}

#[test]
fn requeue_never_collides_with_an_existing_branch() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "ls-remote",
        0,
        concat!(
            "aaa\trefs/heads/unit/u1-a1\n",
            "bbb\trefs/heads/unit/u1-a2\n",
            "ccc\trefs/heads/unit/u1-a3\n",
        ),
    );
    let mut st = State::default();
    put(
        &mut st,
        "u1",
        Unit {
            status: Status::Stuck,
            attempts: 3,
            ..Unit::default()
        },
    );
    request(&ctx, "u1", None, true, false, "start over");

    apply_pending(&ctx, &mut st);

    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Pending);
    assert_eq!(unit.attempts, 3, "fresh still starts past every branch");
    assert_eq!(unit.attempts + 1, 4, "the next attempt branch is a4");
    assert!(logged(
        &host,
        "requeue u1 attempts raised to 3: branch unit/u1-a3 already exists"
    ));
}

#[test]
fn requeue_releases_a_quarantined_unit_solo() {
    let (_dir, ctx, host) = setup();
    host.reply("ls-remote", 0, "");
    let mut st = State::default();
    put(
        &mut st,
        "u1",
        Unit {
            status: Status::Quarantined,
            quarantine: Some(String::from("q1")),
            attempts: 2,
            ..Unit::default()
        },
    );
    request(&ctx, "u1", None, false, false, "retry alone");

    apply_pending(&ctx, &mut st);

    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Delivered);
    assert!(unit.solo);
    assert_eq!(unit.quarantine, None);
    assert_eq!(unit.attempts, 2);
    assert!(logged(&host, "requeue u1 by operator: retry alone"));
}

#[test]
fn requeue_refuses_a_merged_unit() {
    let (_dir, ctx, host) = setup();
    host.reply("ls-remote", 0, "");
    let mut st = State::default();
    put(
        &mut st,
        "u1",
        Unit {
            status: Status::Merged,
            attempts: 2,
            commit: Some(String::from("abc123")),
            ..Unit::default()
        },
    );
    request(&ctx, "u1", None, true, false, "too late");

    apply_pending(&ctx, &mut st);

    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Merged);
    assert_eq!(unit.attempts, 2);
    assert_eq!(unit.commit.as_deref(), Some("abc123"));
    assert!(logged(&host, "requeue u1 refused: status is merged"));

    // Refused or not, the request is consumed.
    request(&ctx, "u1", None, false, false, "again");
    let before = host.logs().len();
    apply_pending(&ctx, &mut st);
    assert_eq!(host.logs().len(), before + 1);
    let store = PumpStore::open_read_only(&ctx.home.join(DB_FILENAME))
        .unwrap()
        .unwrap();
    assert!(store.pending_requeue_requests().unwrap().is_empty());
}

#[test]
fn requeue_refunds_transient_history() {
    let (_dir, ctx, host) = setup();
    host.reply("ls-remote", 0, "");
    let mut st = State::default();
    put(
        &mut st,
        "u1",
        Unit {
            status: Status::Stuck,
            attempts: 3,
            transient_retries: 2,
            history: vec![
                Attempt {
                    attempt: 1,
                    why: String::from("error/host-down"),
                    ..Attempt::default()
                },
                Attempt {
                    attempt: 2,
                    why: String::from("fetch-failed"),
                    ..Attempt::default()
                },
                Attempt {
                    attempt: 3,
                    why: String::from("proof-failed"),
                    detail: String::from("2 tests failed (exit 1)"),
                    ..Attempt::default()
                },
            ],
            ..Unit::default()
        },
    );
    request(&ctx, "u1", None, false, true, "refund the host failures");

    apply_pending(&ctx, &mut st);

    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Pending);
    assert_eq!(unit.attempts, 1);
    assert_eq!(unit.transient_retries, 0);
    assert!(logged(
        &host,
        "requeue u1 by operator: refund the host failures"
    ));
}
