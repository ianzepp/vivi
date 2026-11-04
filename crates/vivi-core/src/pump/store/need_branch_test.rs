//! Tests for the `need_branches` table methods in [`super`].

use tempfile::{TempDir, tempdir};

use super::NeedBranchRow;
use crate::pump::flow::index::RunRow;
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

fn raw_insert(dir: &TempDir, need: &str, record: Option<&str>) {
    let conn = rusqlite::Connection::open(dir.path().join(DB_FILENAME)).expect("raw open");
    conn.execute(
        "INSERT INTO need_branches (need, branch, base, status, record_json)
         VALUES (?1, 'b', NULL, 'live', ?2)",
        rusqlite::params![need, record],
    )
    .expect("raw insert");
}

#[test]
fn null_record_is_an_error_naming_the_need() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    raw_insert(&dir, "n9", None);
    let err = store
        .need_branch("n9")
        .expect_err("null record")
        .to_string();
    assert!(err.contains("need branch n9"), "{err}");
    assert!(
        err.contains("expected a record with run and target"),
        "{err}"
    );
}

#[test]
fn partial_record_is_an_error_naming_the_need() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    raw_insert(&dir, "n8", Some("{}"));
    let err = store
        .need_branch("n8")
        .expect_err("empty record")
        .to_string();
    assert!(err.contains("need branch n8"), "{err}");
    raw_insert(&dir, "n7", Some(r#"{"run":1}"#));
    let err = store.need_branch("n7").expect_err("no target").to_string();
    assert!(
        err.contains("need branch n7") && err.contains("target"),
        "{err}"
    );
}

#[test]
fn for_run_copies_need_and_run() {
    let run = RunRow {
        need: "n5".into(),
        run: 4,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    };
    let got = NeedBranchRow::for_run(&run, "b".into(), None, "fresh", "vivi", vec!["p".into()]);
    assert_eq!(got.need, "n5");
    assert_eq!(got.run, 4);
    assert_eq!(got.status, "fresh");
    assert_eq!(got.target, "vivi");
    assert_eq!(got.priors, vec!["p".to_string()]);
}
