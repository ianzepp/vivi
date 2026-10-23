use std::fs;
use std::path::Path;

use super::{assess, is_ready, render_missing};
use crate::mailspace::{Mailspace, RoleUpdate};
use crate::routing::{Issue, Severity};

/// A project as `vivi mailspace init` leaves it: the shipped roles and the
/// starter `[pump]` table, nothing else.
fn fresh(root: &Path) -> Mailspace {
    let mut space = Mailspace::init(Some(root)).unwrap();
    space.ensure_operator_role().unwrap();
    space.ensure_merge_role().unwrap();
    space.ensure_pump_planner_role().unwrap();
    space.ensure_starter_pump().unwrap();
    space
}

fn reopen(root: &Path) -> Mailspace {
    Mailspace::open_root(root.to_path_buf()).unwrap()
}

fn config_path(root: &Path) -> std::path::PathBuf {
    root.join(".vivi").join("mailspace.toml")
}

fn edit(root: &Path, change: impl FnOnce(String) -> String) {
    let path = config_path(root);
    fs::write(&path, change(fs::read_to_string(&path).unwrap())).unwrap();
}

fn items(issues: &[Issue]) -> Vec<String> {
    issues
        .iter()
        .filter(|issue| issue.severity == Severity::NotReady)
        .filter_map(|issue| issue.item.clone())
        .collect()
}

fn errors(issues: &[Issue]) -> Vec<&str> {
    issues
        .iter()
        .filter(|issue| issue.severity == Severity::Error)
        .map(|issue| issue.message.as_str())
        .collect()
}

#[test]
fn a_fresh_project_is_not_ready_and_has_no_errors() {
    let tmp = tempfile::tempdir().unwrap();
    fresh(tmp.path());
    let issues = assess(&reopen(tmp.path()));
    assert!(errors(&issues).is_empty(), "{:?}", errors(&issues));
    assert!(!is_ready(&issues));
    let names = items(&issues);
    for expected in [
        "pump.branch",
        "pump.committer_name",
        "pump.committer_email",
        "pump.merge.shared_checks",
        "hand pool",
        "merge pool",
        "pump-planner pool",
        "auditor role",
        "role hand",
        "role mind",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} in {names:?}"
        );
    }
}

#[test]
fn each_missing_item_is_listed_once() {
    let tmp = tempfile::tempdir().unwrap();
    fresh(tmp.path());
    let names = items(&assess(&reopen(tmp.path())));
    let mut unique = names.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(names.len(), unique.len(), "{names:?}");
}

#[test]
fn the_missing_report_counts_and_names_every_item() {
    let tmp = tempfile::tempdir().unwrap();
    fresh(tmp.path());
    let issues = assess(&reopen(tmp.path()));
    let report = render_missing(&issues);
    let count = items(&issues).len();
    assert!(
        report.starts_with(&format!("not ready: {count} item(s) missing")),
        "{report}"
    );
    assert!(report.contains("- pump.committer_name:"), "{report}");
    assert_eq!(render_missing(&[]), "");
}

#[test]
fn a_wrong_value_is_an_error_even_while_items_are_missing() {
    let tmp = tempfile::tempdir().unwrap();
    fresh(tmp.path());
    edit(tmp.path(), |text| {
        text.replace("[pump]\n", "[pump]\ntick = 0\n")
    });
    let issues = assess(&reopen(tmp.path()));
    assert!(
        errors(&issues)
            .iter()
            .any(|e| e.contains("tick must be >= 1")),
        "{issues:?}"
    );
    assert!(!items(&issues).is_empty());
    assert!(crate::routing::require_ok(&issues).is_err());
}

#[test]
fn a_pool_name_that_points_at_nothing_stays_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    fresh(tmp.path());
    edit(tmp.path(), |text| {
        format!("{text}\n[pump.rungs]\npools = [\"nowhere\"]\n")
    });
    let issues = assess(&reopen(tmp.path()));
    assert!(
        errors(&issues)
            .iter()
            .any(|e| e.contains("unknown pool `nowhere`")),
        "{issues:?}"
    );
}

#[test]
fn a_role_that_names_a_missing_pool_stays_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = fresh(tmp.path());
    space
        .set_role(
            "merge",
            RoleUpdate {
                pool: Some(Some(String::from("nowhere"))),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    let issues = assess(&reopen(tmp.path()));
    assert!(
        errors(&issues)
            .iter()
            .any(|e| e.contains("unknown pool `nowhere`")),
        "{issues:?}"
    );
}

const POOL: &str = "
[pools.p]
provider = \"local\"
slots = 1

[pools.p.harness]
name = \"h\"
run = \"h --model {model} @{brief}\"
models = [\"m\"]

[pools.p.bands]
P2 = [{ model = \"m\", reasoning = \"low\" }]
";

fn give_role_a_pool(space: &mut Mailspace, name: &str, kind: &str) {
    let update = RoleUpdate {
        kind: Some(Some(kind.to_owned())),
        pool: Some(Some(String::from("p"))),
        model: Some(Some(String::from("P2"))),
        ..RoleUpdate::default()
    };
    if space.role_view(name).is_ok() {
        space.set_role(name, update).unwrap();
    } else {
        space.add_role(name, update).unwrap();
    }
}

#[test]
fn filling_in_the_missing_items_makes_the_project_ready_with_no_other_step() {
    let tmp = tempfile::tempdir().unwrap();
    fresh(tmp.path());
    edit(tmp.path(), |text| text + POOL);
    let mut space = reopen(tmp.path());
    for (name, kind) in [
        ("hand", "hand"),
        ("merge", "merge"),
        ("pump-planner", "planner"),
        ("auditor", "auditor"),
        ("mind", "mind"),
    ] {
        give_role_a_pool(&mut space, name, kind);
    }
    // Roles and the pool are in; only the pump values are left.
    let issues = assess(&reopen(tmp.path()));
    assert!(errors(&issues).is_empty(), "{:?}", errors(&issues));
    assert_eq!(
        items(&issues),
        [
            "pump.committer_name",
            "pump.committer_email",
            "pump.branch",
            "pump.merge.shared_checks"
        ],
        "{issues:?}"
    );
    edit(tmp.path(), |text| {
        text.replace("branch = \"\"", "branch = \"next\"")
            .replace("committer_name = \"\"", "committer_name = \"T\"")
            .replace(
                "committer_email = \"\"",
                "committer_email = \"t@example.com\"",
            )
            .replace("shared_checks = []", "shared_checks = [\"true\"]")
    });
    let issues = assess(&reopen(tmp.path()));
    assert!(is_ready(&issues), "{issues:?}");
    assert!(crate::routing::not_ready_summary(&issues).is_none());
}

#[test]
fn a_project_without_a_pump_table_has_nothing_missing() {
    let tmp = tempfile::tempdir().unwrap();
    Mailspace::init(Some(tmp.path())).unwrap();
    let issues = assess(&reopen(tmp.path()));
    assert!(is_ready(&issues), "{issues:?}");
}

#[test]
fn a_misspelled_pump_role_is_an_error_not_a_missing_role() {
    let tmp = tempfile::tempdir().unwrap();
    fresh(tmp.path());
    edit(tmp.path(), |text| {
        text.replace("[pump]\n", "[pump]\nto = \"hnad\"\n")
    });
    let issues = assess(&reopen(tmp.path()));
    assert!(
        errors(&issues)
            .iter()
            .any(|e| e.contains("unknown local role 'hnad'")),
        "{issues:?}"
    );
    assert!(!items(&issues).iter().any(|name| name == "role hnad"));
}

#[test]
fn a_writable_cadence_role_is_one_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = fresh(tmp.path());
    space
        .add_role(
            "head-writer",
            RoleUpdate {
                kind: Some(Some(String::from("head"))),
                cadence: Some(Some(String::from("4h"))),
                read_only: Some(Some(false)),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    let issues = assess(&reopen(tmp.path()));
    let warnings: Vec<&Issue> = issues
        .iter()
        .filter(|issue| issue.severity == Severity::Warning && issue.message.contains("cadence"))
        .collect();
    assert_eq!(warnings.len(), 1, "{issues:?}");
    assert!(warnings[0].message.contains("head-writer"), "{warnings:?}");
    assert!(errors(&issues).is_empty(), "{:?}", errors(&issues));
    assert!(
        !items(&issues)
            .iter()
            .any(|name| name.contains("head-writer"))
    );
}
