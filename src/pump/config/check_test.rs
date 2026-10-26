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
    append_to(&root.join(".vivi/mailspace.toml"), text);
}

fn append_to(path: &Path, text: &str) {
    let mut raw = fs::read_to_string(path).unwrap();
    raw.push_str(text);
    fs::write(path, raw).unwrap();
}

/// A project under a base config: `(guard, project root, base config path)`.
/// The base lives at the guard root with `base_toml`; the project is
/// initialized under it, so the merge takes the base's pump table and the
/// project file itself stays thin.
fn init_inherited(base_toml: &str) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempdir().unwrap();
    let base_file = tmp.path().join(".vivi/mailspace.toml");
    fs::create_dir_all(base_file.parent().unwrap()).unwrap();
    fs::write(&base_file, base_toml).unwrap();
    let project = tmp.path().join("proj");
    Mailspace::init_with_defaults(Some(&project)).unwrap();
    (tmp, project, base_file)
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
fn inherited_retired_keys_name_their_owning_file() {
    let (tmp, project, base_file) =
        init_inherited("base = true\n\n[pump]\nbranch = \"old\"\nfrom = \"dispatcher\"\n");
    let report = validate(Some(&project)).unwrap();
    let base = base_file.display().to_string();
    let branch = report
        .issues
        .iter()
        .find(|issue| issue.severity == Severity::Error && issue.message.contains("[pump] branch"))
        .unwrap_or_else(|| panic!("{:?}", report.issues));
    assert!(branch.message.contains(&base), "{:?}", branch);
    assert!(
        branch.message.contains("delete the key there"),
        "{:?}",
        branch
    );
    let from = report
        .issues
        .iter()
        .find(|issue| issue.severity == Severity::Warning && issue.message.contains("[pump] from"))
        .unwrap_or_else(|| panic!("{:?}", report.issues));
    assert!(from.message.contains(&base), "{:?}", from);
    assert_eq!(from.item.as_deref(), Some("pump.from"));
    assert!(
        from.message.contains("vivi config migrate` leaves it"),
        "{:?}",
        from
    );
}

#[test]
fn overridden_and_empty_inherited_keys_report_each_source() {
    let (tmp, project, base_file) = init_inherited("base = true\n\n[pump]\nfrom = \"\"\n");
    append_to(
        &project.join(".vivi/mailspace.toml"),
        "[pump]\nfrom = \"project\"\n",
    );
    let report = validate(Some(&project)).unwrap();
    let base = base_file.display().to_string();
    let occurrences: Vec<&str> = report
        .issues
        .iter()
        .filter(|issue| issue.message.contains("[pump] from"))
        .map(|issue| issue.message.as_str())
        .collect();
    assert_eq!(occurrences.len(), 2, "{:?}", occurrences);
    assert!(
        occurrences.iter().any(|text| text.contains(&base)),
        "{occurrences:?}"
    );
    assert!(
        occurrences.iter().any(|text| !text.contains(&base)),
        "{occurrences:?}"
    );
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
