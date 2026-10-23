//! Tests for the flow link tables in [`super`].

use tempfile::{TempDir, tempdir};

use super::SliceLink;
use crate::pump::store::{DB_FILENAME, PumpStore};

fn open(dir: &TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open")
}

fn link(task: &str, fanout: Option<&str>, deps: &[&str]) -> SliceLink {
    SliceLink {
        task: task.into(),
        need: "n1".into(),
        run: 1,
        fanout: fanout.map(String::from),
        deps: deps.iter().map(|d| (*d).to_string()).collect(),
    }
}

#[test]
fn open_fanout_round_trips_and_replaces() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    assert_eq!(store.open_fanout("n1", 1).expect("read"), None);
    store.set_open_fanout("n1", 1, "t-a").expect("set");
    assert_eq!(
        store.open_fanout("n1", 1).expect("read"),
        Some("t-a".into())
    );
    store.set_open_fanout("n1", 1, "t-b").expect("replace");
    assert_eq!(
        store.open_fanout("n1", 1).expect("read"),
        Some("t-b".into())
    );
    assert_eq!(store.open_fanout("n1", 2).expect("read"), None);
}

#[test]
fn slice_link_round_trips_replaces_and_lists_in_insertion_order() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    assert_eq!(store.slice_link("s1").expect("read"), None);
    store.link_slice(&link("s1", Some("f"), &[])).expect("link");
    store.link_slice(&link("s2", None, &["s1"])).expect("link");
    assert_eq!(
        store.slice_link("s2").expect("read"),
        Some(link("s2", None, &["s1"]))
    );
    store
        .link_slice(&link("s1", Some("g"), &["x"]))
        .expect("replace");
    assert_eq!(
        store.slice_link("s1").expect("read"),
        Some(link("s1", Some("g"), &["x"]))
    );
    let mut other = link("s3", Some("f"), &[]);
    other.run = 2;
    store.link_slice(&other).expect("link");
    let tasks: Vec<String> = store
        .slice_links_of_run("n1", 1)
        .expect("list")
        .into_iter()
        .map(|l| l.task)
        .collect();
    assert_eq!(tasks, vec!["s1", "s2"]);
}

#[test]
fn set_slice_deps_updates_and_refuses_unknown_task() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    store.link_slice(&link("s1", Some("f"), &[])).expect("link");
    store
        .set_slice_deps("s1", &["a".into(), "b".into()])
        .expect("deps");
    assert_eq!(
        store.slice_link("s1").expect("read").expect("row").deps,
        vec!["a", "b"]
    );
    assert!(store.set_slice_deps("nope", &[]).is_err());
}

#[test]
fn restart_entry_round_trips_and_replaces() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    assert_eq!(store.restart_entry("n1", 1).expect("read"), None);
    store.set_restart_entry("n1", 1, "rework1").expect("set");
    assert_eq!(
        store.restart_entry("n1", 1).expect("read"),
        Some("rework1".into())
    );
    store.set_restart_entry("n1", 1, "plan").expect("replace");
    assert_eq!(
        store.restart_entry("n1", 1).expect("read"),
        Some("plan".into())
    );
}

#[test]
fn reopening_the_file_keeps_the_rows() {
    let dir = tempdir().expect("tempdir");
    {
        let store = open(&dir);
        store.set_open_fanout("n1", 1, "f").expect("set");
        store
            .link_slice(&link("s1", Some("f"), &["s0"]))
            .expect("link");
        store.set_restart_entry("n1", 1, "plan").expect("set");
    }
    let store = open(&dir);
    assert_eq!(store.open_fanout("n1", 1).expect("read"), Some("f".into()));
    assert_eq!(
        store.slice_link("s1").expect("read"),
        Some(link("s1", Some("f"), &["s0"]))
    );
    assert_eq!(
        store.restart_entry("n1", 1).expect("read"),
        Some("plan".into())
    );
}

#[test]
fn fanout_start_keeps_the_first_tip_and_tells_none_from_unset() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    assert_eq!(store.fanout_start("t-a").expect("read"), None);
    store.set_fanout_start("t-a", None).expect("set none");
    assert_eq!(store.fanout_start("t-a").expect("read"), Some(None));
    store.set_fanout_start("t-a", Some("abc")).expect("ignored");
    assert_eq!(store.fanout_start("t-a").expect("read"), Some(None));
    store.set_fanout_start("t-b", Some("abc")).expect("set");
    store.set_fanout_start("t-b", Some("def")).expect("ignored");
    assert_eq!(
        store.fanout_start("t-b").expect("read"),
        Some(Some("abc".into()))
    );
}
