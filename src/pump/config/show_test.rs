//! Tests for `pump::config::show`.

use tempfile::tempdir;

use super::{count_free, show};
use crate::mailspace::Mailspace;

fn project() -> tempfile::TempDir {
    let tmp = tempdir().unwrap();
    Mailspace::init_with_defaults(Some(tmp.path())).unwrap();
    tmp
}

#[test]
fn show_lists_starter_roles_without_live_slots() {
    let tmp = project();
    let report = show(Some(tmp.path()), false).unwrap();
    let names: Vec<&str> = report
        .overview
        .roles
        .iter()
        .map(|row| row.role.as_str())
        .collect();
    assert!(names.contains(&"operator"), "{names:?}");
    assert!(names.contains(&"merge"), "{names:?}");
    assert!(report.overview.roles.iter().all(|row| row.free.is_none()));
    assert!(!report.origins.inherited());
    assert_eq!(report.overview.flow.main, "main");
}

#[test]
fn show_resolved_still_returns_when_pool_status_is_not_vivi() {
    let tmp = project();
    let report = show(Some(tmp.path()), true).unwrap();
    assert!(!report.overview.roles.is_empty());
}

#[test]
fn count_free_counts_only_free_slots() {
    let text = "\
slot glm 1 hand free extra
slot glm 2 hand busy
slot host 1 merge free
not a slot line free
slot only-three
";
    let free = count_free(text);
    assert_eq!(free.get("glm").copied(), Some(1));
    assert_eq!(free.get("host").copied(), Some(1));
    assert!(!free.contains_key("only-three"));
}
