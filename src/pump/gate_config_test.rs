//! Tests for [`super::gate_config`].

use std::fs;
use std::path::Path;

use tempfile::{TempDir, tempdir};

use crate::pump::gate_config::{command, set_command};

fn project_with(text: &str) -> TempDir {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    fs::write(config(dir.path()), text).unwrap();
    dir
}

fn config(root: &Path) -> std::path::PathBuf {
    root.join(".vivi").join("mailspace.toml")
}

fn text(dir: &TempDir) -> String {
    fs::read_to_string(config(dir.path())).unwrap()
}

#[test]
fn set_creates_pump_gate_when_missing() {
    let dir = project_with("name = \"x\"\n");
    set_command(dir.path(), "cargo test").unwrap();
    let doc: toml::Value = toml::from_str(&text(&dir)).unwrap();
    assert_eq!(doc["pump"]["gate"]["command"].as_str(), Some("cargo test"));
    assert_eq!(doc["name"].as_str(), Some("x"));
    assert_eq!(command(dir.path()).unwrap().as_deref(), Some("cargo test"));
}

#[test]
fn set_preserves_every_other_byte() {
    let before = "# top comment\nname = \"x\"   # trailing\n\n[pump]\n\
                  branch = \"next\"  # keep\n\n[pump.gate]\n# gate note\n\
                  command = \"old\"   # was\nevery_merges = 3\n\n\
                  [pools.a]\nprovider = \"local\"\n";
    let dir = project_with(before);
    set_command(dir.path(), "new cmd").unwrap();
    let after = text(&dir);
    assert_eq!(
        after,
        before.replace("command = \"old\"   # was", "command = \"new cmd\"   # was")
    );
}

#[test]
fn set_adds_gate_table_under_existing_pump_without_touching_it() {
    let before = "[pump]\n# note\nbranch = \"next\"\n\n[pools.a]\nprovider = \"local\"\n";
    let dir = project_with(before);
    set_command(dir.path(), "true").unwrap();
    let after = text(&dir);
    assert!(
        after.starts_with("[pump]\n# note\nbranch = \"next\"\n"),
        "{after}"
    );
    assert!(
        after.contains("[pools.a]\nprovider = \"local\"\n"),
        "{after}"
    );
    assert_eq!(command(dir.path()).unwrap().as_deref(), Some("true"));
}

#[test]
fn set_overwrites_and_is_idempotent() {
    let dir = project_with("[pump.gate]\ncommand = \"old\"\n");
    set_command(dir.path(), "one").unwrap();
    set_command(dir.path(), "two").unwrap();
    let once = text(&dir);
    set_command(dir.path(), "two").unwrap();
    assert_eq!(text(&dir), once);
    assert_eq!(command(dir.path()).unwrap().as_deref(), Some("two"));
}

#[test]
fn empty_and_blank_commands_are_rejected_without_writing() {
    let before = "name = \"x\"\n";
    let dir = project_with(before);
    for bad in ["", "   ", "\t\n"] {
        assert!(set_command(dir.path(), bad).is_err(), "{bad:?}");
    }
    assert_eq!(text(&dir), before);
}

#[test]
fn getter_reports_none_when_unset() {
    let dir = project_with("[pump]\nbranch = \"next\"\n");
    assert_eq!(command(dir.path()).unwrap(), None);
}

#[test]
fn missing_config_is_an_error_and_is_not_created() {
    let dir = tempdir().unwrap();
    assert!(set_command(dir.path(), "true").is_err());
    assert!(command(dir.path()).is_err());
    assert!(!config(dir.path()).exists());
}

#[test]
fn non_table_pump_is_an_error() {
    let before = "pump = 3\n";
    let dir = project_with(before);
    assert!(set_command(dir.path(), "true").is_err());
    assert_eq!(text(&dir), before);
}

#[test]
fn a_blank_command_reads_as_unset() {
    let dir = tempfile::tempdir().unwrap();
    let vivi = dir.path().join(".vivi");
    std::fs::create_dir_all(&vivi).unwrap();
    std::fs::write(vivi.join("mailspace.toml"), "[pump.gate]\ncommand = \"\"\n").unwrap();
    assert_eq!(super::command(dir.path()).unwrap(), None);
}
