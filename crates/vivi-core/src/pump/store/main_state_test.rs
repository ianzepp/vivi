//! Tests for the `main_state` table methods in [`super`].

use tempfile::{TempDir, tempdir};

use crate::pump::store::{DB_FILENAME, PumpStore};

fn open(dir: &TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open")
}

#[test]
fn missing_key_reads_none() {
    let dir = tempdir().expect("tempdir");
    assert_eq!(open(&dir).main_state("k").expect("read"), None);
}

#[test]
fn set_then_read() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.set_main_state("k", "v1").expect("set");
    assert_eq!(store.main_state("k").expect("read"), Some("v1".into()));
}

#[test]
fn set_twice_keeps_last_value() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.set_main_state("k", "v1").expect("set");
    store.set_main_state("k", "v2").expect("set");
    assert_eq!(store.main_state("k").expect("read"), Some("v2".into()));
}

#[test]
fn clear_removes_the_key() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.set_main_state("k", "v").expect("set");
    store.clear_main_state("k").expect("clear");
    assert_eq!(store.main_state("k").expect("read"), None);
}

#[test]
fn clear_of_missing_key_is_ok() {
    let dir = tempdir().expect("tempdir");
    open(&dir).clear_main_state("nope").expect("clear");
}

#[test]
fn values_survive_reopening() {
    let dir = tempdir().expect("tempdir");
    open(&dir).set_main_state("k", "v").expect("set");
    assert_eq!(open(&dir).main_state("k").expect("read"), Some("v".into()));
}
