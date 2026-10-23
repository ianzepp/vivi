//! Need-branch names, base-once-per-run and tip, over the fake host.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{DELETING, FRESH, LIVE, PRUNED, RENAMING, branch, prior, record_base, tip};
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};

const MAIN_A: &str = "1111111111111111111111111111111111111111";
const MAIN_B: &str = "2222222222222222222222222222222222222222";
const NEED_TIP: &str = "3333333333333333333333333333333333333333";
const NEED: &str = "abcd1234";

struct Fixture {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    store: PumpStore,
}

fn fixture() -> Fixture {
    let dir = tempdir().expect("tempdir");
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    Fixture {
        _dir: dir,
        ctx,
        host,
        store,
    }
}

fn main_tip(host: &FakeHost, sha: &str) {
    host.reply(
        "ls-remote origin main",
        0,
        &format!("{sha}\trefs/heads/main\n"),
    );
}

fn row(status: &str, run: u32) -> NeedBranchRow {
    NeedBranchRow {
        need: NEED.into(),
        branch: branch(NEED),
        base: None,
        status: status.into(),
        run,
        target: "default".into(),
        priors: vec![prior(NEED, 1)],
    }
}

#[test]
fn names_are_one_segment_after_their_prefix() {
    assert_eq!(branch(NEED), "vivi/need/abcd1234");
    let old = prior(NEED, 3);
    assert_eq!(old, "vivi/need-prior/abcd1234-r3");
    assert!(!old.strip_prefix("vivi/need-prior/").unwrap().contains('/'));
    assert_eq!(
        [FRESH, LIVE, RENAMING, DELETING, PRUNED],
        ["fresh", "live", "renaming", "deleting", "pruned"]
    );
}

#[test]
fn the_base_is_recorded_once_per_run() {
    let f = fixture();
    main_tip(&f.host, MAIN_A);
    assert_eq!(
        record_base(&f.ctx, &f.store, NEED, 1, "default").unwrap(),
        MAIN_A
    );
    main_tip(&f.host, MAIN_B);
    assert_eq!(
        record_base(&f.ctx, &f.store, NEED, 1, "default").unwrap(),
        MAIN_A
    );
    let stored = f.store.need_branch(NEED).unwrap().unwrap();
    assert_eq!(stored.base.as_deref(), Some(MAIN_A));
    assert_eq!(stored.status, LIVE);
    assert_eq!(stored.branch, "vivi/need/abcd1234");
    assert_eq!(stored.target, "default");
}

#[test]
fn a_fresh_row_records_a_new_base_and_keeps_its_priors() {
    let f = fixture();
    f.store.upsert_need_branch(&row(FRESH, 2)).unwrap();
    main_tip(&f.host, MAIN_B);
    assert_eq!(
        record_base(&f.ctx, &f.store, NEED, 2, "default").unwrap(),
        MAIN_B
    );
    let stored = f.store.need_branch(NEED).unwrap().unwrap();
    assert_eq!(stored.status, LIVE);
    assert_eq!(stored.run, 2);
    assert_eq!(stored.priors, vec![prior(NEED, 1)]);
}

#[test]
fn record_base_errors_while_the_old_branch_is_moving() {
    for status in [RENAMING, DELETING] {
        let f = fixture();
        f.store.upsert_need_branch(&row(status, 1)).unwrap();
        main_tip(&f.host, MAIN_A);
        let err = record_base(&f.ctx, &f.store, NEED, 2, "default").unwrap_err();
        assert!(err.to_string().contains("still being moved"), "{err}");
        let stored = f.store.need_branch(NEED).unwrap().unwrap();
        assert_eq!(stored.status, status);
        assert_eq!(stored.base, None);
    }
}

#[test]
fn tip_is_the_base_until_the_branch_is_pushed() {
    let f = fixture();
    assert_eq!(tip(&f.ctx, &f.store, NEED).unwrap(), None);
    main_tip(&f.host, MAIN_A);
    record_base(&f.ctx, &f.store, NEED, 1, "default").unwrap();
    f.host
        .reply("ls-remote origin refs/heads/vivi/need/", 0, "");
    assert_eq!(
        tip(&f.ctx, &f.store, NEED).unwrap().as_deref(),
        Some(MAIN_A)
    );
    f.host.reply(
        "ls-remote origin refs/heads/vivi/need/",
        0,
        &format!("{NEED_TIP}\trefs/heads/vivi/need/{NEED}\n"),
    );
    assert_eq!(
        tip(&f.ctx, &f.store, NEED).unwrap().as_deref(),
        Some(NEED_TIP)
    );
}

#[test]
fn tip_is_none_unless_the_row_is_live() {
    let f = fixture();
    for status in [FRESH, RENAMING, DELETING, PRUNED] {
        f.store.upsert_need_branch(&row(status, 1)).unwrap();
        assert_eq!(tip(&f.ctx, &f.store, NEED).unwrap(), None, "{status}");
    }
}

#[test]
fn an_ls_remote_failure_propagates() {
    let f = fixture();
    f.host.reply("ls-remote", 128, "fatal: unable to access");
    let err = record_base(&f.ctx, &f.store, NEED, 1, "default").unwrap_err();
    assert!(err.to_string().contains("ls-remote exit 128"), "{err}");
    assert!(f.store.need_branch(NEED).unwrap().is_none());
}
