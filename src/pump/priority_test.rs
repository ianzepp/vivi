//! Tests for [`super::priority`].

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::priority::{clear, import_legacy, levels, list, set};

fn setup() -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    (dir, ctx, host)
}

#[test]
fn no_database_means_no_levels() {
    let (_dir, ctx, _host) = setup();
    assert!(levels(&ctx).is_empty());
}

#[test]
fn set_list_and_clear_round_trip_highest_first() {
    let (_dir, ctx, _host) = setup();
    set(&ctx.home, "b", 2).unwrap();
    set(&ctx.home, "a", 2).unwrap();
    set(&ctx.home, "c", 9).unwrap();
    assert_eq!(
        list(&ctx.home).unwrap(),
        [("c".into(), 9), ("a".into(), 2), ("b".into(), 2)]
    );
    assert_eq!(levels(&ctx)["c"], 9);
    set(&ctx.home, "c", 3).unwrap();
    assert_eq!(levels(&ctx)["c"], 3);
    clear(&ctx.home, "c").unwrap();
    assert!(!levels(&ctx).contains_key("c"));
}

#[test]
fn level_zero_removes_and_negative_is_refused() {
    let (_dir, ctx, _host) = setup();
    set(&ctx.home, "a", 4).unwrap();
    set(&ctx.home, "a", 0).unwrap();
    assert!(list(&ctx.home).unwrap().is_empty());
    assert!(set(&ctx.home, "a", -1).is_err());
}

#[test]
fn a_running_reader_sees_a_later_write() {
    let (_dir, ctx, _host) = setup();
    set(&ctx.home, "a", 1).unwrap();
    assert_eq!(levels(&ctx).len(), 1);
    set(&ctx.home, "b", 5).unwrap();
    assert_eq!(levels(&ctx).len(), 2);
}

#[test]
fn the_legacy_file_imports_once_at_level_one_and_is_renamed() {
    let (_dir, ctx, host) = setup();
    set(&ctx.home, "kept", 7).unwrap();
    std::fs::write(
        ctx.home.join("priority.txt"),
        "# hot\n\n  one  \nkept\ntwo\n",
    )
    .unwrap();
    import_legacy(&ctx);
    assert_eq!(
        list(&ctx.home).unwrap(),
        [("kept".into(), 7), ("one".into(), 1), ("two".into(), 1)]
    );
    assert!(!ctx.home.join("priority.txt").exists());
    assert!(ctx.home.join("priority.txt.imported").exists());
    assert!(host.logs().iter().any(|line| line.contains("imported 2")));

    // A second startup finds no file and changes nothing.
    clear(&ctx.home, "one").unwrap();
    import_legacy(&ctx);
    assert!(!levels(&ctx).contains_key("one"));
}
