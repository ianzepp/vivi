//! Tests for the `need_branches` table methods in [`super`].

use tempfile::{TempDir, tempdir};

use super::NeedBranchRow;
use crate::pump::store::{DB_FILENAME, PumpStore};

fn row(need: &str) -> NeedBranchRow {
    NeedBranchRow {
        need: need.into(),
        branch: format!("vivi/need/{need}"),
        base: Some("abc123".into()),
        status: "live".into(),
        run: 2,
        target: "vivi".into(),
        priors: vec![format!("vivi/need-prior/{need}-r1")],
    }
}

fn open(dir: &TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open")
}

#[test]
fn upsert_then_read_round_trips_every_field() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.upsert_need_branch(&row("n1")).expect("upsert");
    assert_eq!(store.need_branch("n1").expect("read"), Some(row("n1")));

    let mut none_base = row("n2");
    none_base.base = None;
    none_base.priors = vec![];
    store.upsert_need_branch(&none_base).expect("upsert");
    assert_eq!(store.need_branch("n2").expect("read"), Some(none_base));
}

#[test]
fn second_upsert_replaces() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.upsert_need_branch(&row("n1")).expect("first");
    let mut second = row("n1");
    second.base = Some("def456".into());
    second.run = 3;
    second.target = "other".into();
    second.priors.clear();
    store.upsert_need_branch(&second).expect("second");
    assert_eq!(store.need_branch("n1").expect("read"), Some(second));
}

#[test]
fn set_status_changes_only_the_status() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.upsert_need_branch(&row("n1")).expect("upsert");
    store
        .set_need_branch_status("n1", "renaming")
        .expect("status");
    let mut want = row("n1");
    want.status = "renaming".into();
    assert_eq!(store.need_branch("n1").expect("read"), Some(want));
}

#[test]
fn unknown_need_reads_none() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    assert_eq!(store.need_branch("ghost").expect("read"), None);
    store
        .set_need_branch_status("ghost", "live")
        .expect("no-op");
    assert_eq!(store.need_branch("ghost").expect("read"), None);
}
