//! Tests for `pump::config::validate`.

use std::fs;
use std::path::Path;

use tempfile::tempdir;

use super::{issues_json, validate};
use crate::mailspace::Mailspace;
use crate::routing::Severity;

fn init() -> tempfile::TempDir {
    let tmp = tempdir().unwrap();
    Mailspace::init_with_defaults(Some(tmp.path())).unwrap();
    tmp
}

fn append(root: &Path, text: &str) {
    let path = root.join(".vivi/mailspace.toml");
    let mut raw = fs::read_to_string(&path).unwrap();
    raw.push_str(text);
    fs::write(path, raw).unwrap();
}

#[test]
fn validate_of_a_fresh_project_is_not_ready_and_not_an_error() {
    let tmp = init();
    let report = validate(Some(tmp.path())).unwrap();
    assert!(
        report.issues.iter().any(|issue| {
            issue.severity == Severity::NotReady
                && issue.item.as_deref() == Some("pump.committer_name")
        }),
        "{:?}",
        report.issues
    );
    assert!(
        report
            .issues
            .iter()
            .all(|issue| issue.severity != Severity::Error),
        "{:?}",
        report.issues
    );
}

#[test]
fn validate_names_a_wrong_tick_and_keeps_not_ready() {
    let tmp = init();
    append(tmp.path(), "tick = 0\n");
    let report = validate(Some(tmp.path())).unwrap();
    assert!(
        report.issues.iter().any(|issue| {
            issue.severity == Severity::Error && issue.message.contains("tick must be >= 1")
        }),
        "{:?}",
        report.issues
    );
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.severity == Severity::NotReady)
    );
}

/// The five pump keys the catalogue schedules for removal while the pump
/// still reads them, as `(table, key)`.
const PLANNED_PUMP_KEYS: [(&str, &str); 5] = [
    ("pump", "from"),
    ("pump", "reply_from"),
    ("pump.board", "from"),
    ("pump.board", "reply_to"),
    ("pump.dispatch", "from"),
];

#[test]
fn removed_pump_keys_warn_individually() {
    let tmp = init();
    append(
        tmp.path(),
        r#"
from = "dispatcher"
reply_from = "pump"
[pump.board]
from = ["operator@x"]
reply_to = "operator@x"
[pump.dispatch]
from = "origin/main"
"#,
    );
    let report = validate(Some(tmp.path())).unwrap();
    for (table, key) in PLANNED_PUMP_KEYS {
        let issue = report
            .issues
            .iter()
            .find(|issue| {
                issue.severity == Severity::Warning
                    && issue.item.as_deref() == Some(&format!("{table}.{key}"))
            })
            .unwrap_or_else(|| panic!("no warning for [{table}] {key}: {:?}", report.issues));
        assert!(
            issue.message.contains(&format!("[{table}] {key}: ")),
            "{issue:?}"
        );
    }
}

#[test]
fn empty_removed_values_still_warn() {
    let tmp = init();
    append(
        tmp.path(),
        r#"
from = ""
reply_from = ""
[pump.board]
from = []
reply_to = ""
[pump.dispatch]
from = ""
"#,
    );
    let report = validate(Some(tmp.path())).unwrap();
    for (table, key) in PLANNED_PUMP_KEYS {
        assert!(
            report.issues.iter().any(|issue| {
                issue.severity == Severity::Warning
                    && issue.item.as_deref() == Some(&format!("{table}.{key}"))
            }),
            "no warning for [{table}] {key}: {:?}",
            report.issues
        );
    }
}

#[test]
fn validate_names_a_retired_key_and_the_migrate_fix() {
    let tmp = init();
    append(tmp.path(), "branch = \"old\"\n");
    let report = validate(Some(tmp.path())).unwrap();
    let message = report
        .issues
        .iter()
        .find(|issue| issue.severity == Severity::Error && issue.message.contains("[pump] branch:"))
        .map(|issue| issue.message.as_str())
        .unwrap_or_else(|| panic!("{:?}", report.issues));
    assert!(message.contains("vivi config migrate --write"), "{message}");
    assert!(message.contains("delete the key"), "{message}");
}

#[test]
fn issues_json_marks_not_ready_and_omits_an_absent_item() {
    let tmp = init();
    append(tmp.path(), "tick = 0\n");
    let report = validate(Some(tmp.path())).unwrap();
    let rows = issues_json(&report.issues);
    let missing = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["item"] == "pump.committer_name")
        .unwrap_or_else(|| panic!("{rows}"));
    assert_eq!(missing["severity"], "not_ready");
    let tick = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["message"]
                .as_str()
                .is_some_and(|text| text.contains("tick must be >= 1"))
        })
        .unwrap_or_else(|| panic!("{rows}"));
    assert_eq!(tick["severity"], "error");
    assert!(tick.get("item").is_none(), "{tick}");
}

#[test]
fn validate_of_a_project_without_a_pump_has_no_error() {
    let tmp = tempdir().unwrap();
    Mailspace::init(Some(tmp.path())).unwrap();
    let report = validate(Some(tmp.path())).unwrap();
    assert!(
        report
            .issues
            .iter()
            .all(|issue| issue.severity != Severity::Error),
        "{:?}",
        report.issues
    );
}
