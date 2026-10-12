//! Embedded skills: list, show, install.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::tempdir;

#[test]
fn help_names_the_skills_commands() {
    let top = stdout(&vivi(&["--help"]));
    assert!(top.contains("skills"), "{top}");

    let help = stdout(&vivi(&["skills", "--help"]));
    assert!(help.contains("list"), "{help}");
    assert!(help.contains("show"), "{help}");
    assert!(help.contains("install"), "{help}");
    assert!(help.contains("embedded"), "{help}");

    let show = stdout(&vivi(&["skills", "show", "--help"]));
    assert!(show.contains("--file"), "{show}");

    let install = stdout(&vivi(&["skills", "install", "--help"]));
    assert!(install.contains("--force"), "{install}");
}

#[test]
fn list_prints_names_descriptions_and_the_binary_version() {
    let version = stdout(&vivi(&["--version"]));
    let crate_version = version
        .split_whitespace()
        .last()
        .expect("vivi --version ends with the crate version")
        .trim()
        .to_string();

    let listed = stdout(&vivi(&["skills", "list"]));
    for name in ["vivi", "tugboat", "speedboat"] {
        let line = listed
            .lines()
            .find(|line| line.starts_with(name))
            .unwrap_or_else(|| panic!("missing {name} in:\n{listed}"));
        assert!(line.contains(&crate_version), "{line}");
    }
    assert!(listed.contains("Use the Vivi CLI"), "{listed}");

    let json: Value = serde_json::from_str(&stdout(&vivi(&["skills", "list", "--json"]))).unwrap();
    let names: Vec<_> = json
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["vivi", "tugboat", "speedboat"]);
    assert_eq!(json[0]["version"], crate_version);
    assert!(
        json[0]["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file == "SKILL.md")
    );
}

#[test]
fn show_prints_skill_md_and_lists_supporting_files() {
    let out = stdout(&vivi(&["skills", "show", "vivi"]));
    assert!(out.contains("skill: vivi"), "{out}");
    assert!(out.contains("files:"), "{out}");
    assert!(out.contains("  SKILL.md"), "{out}");
    assert!(out.contains("  references/work-graph-flow.md"), "{out}");
    assert!(out.contains("# Vivi"), "{out}");

    let flow = stdout(&vivi(&[
        "skills",
        "show",
        "vivi",
        "--file",
        "references/work-graph-flow.md",
    ]));
    assert!(!flow.contains("skill: vivi"), "{flow}");
    assert!(
        flow.contains("Work-graph") || flow.contains("work graph"),
        "{flow}"
    );

    let missing = vivi(&["skills", "show", "ghost"]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("unknown skill"),
        "{}",
        stderr(&missing)
    );
}

#[test]
fn install_writes_skills_and_refuses_without_force() {
    let dir = tempdir().unwrap();
    let dest = dir.path().join("agent-skills");
    let dest_s = dest.to_str().unwrap();

    let first = vivi(&["skills", "install", dest_s]);
    assert_success(&first);
    let receipt = stdout(&first);
    assert!(receipt.contains("installed 3 skills"), "{receipt}");
    assert!(receipt.contains(dest_s), "{receipt}");

    assert!(dest.join("vivi/SKILL.md").is_file());
    assert!(dest.join("tugboat/references/charters/hand.md").is_file());
    assert!(dest.join("speedboat/SKILL.md").is_file());
    assert_eq!(
        fs::read_to_string(dest.join("vivi/SKILL.md")).unwrap(),
        on_disk("skills/vivi/SKILL.md")
    );

    fs::write(dest.join("vivi/SKILL.md"), "stale\n").unwrap();
    let blocked = vivi(&["skills", "install", dest_s]);
    assert!(!blocked.status.success());
    assert!(
        stderr(&blocked).contains("already exists"),
        "{}",
        stderr(&blocked)
    );

    let forced = vivi(&["skills", "install", dest_s, "--force"]);
    assert_success(&forced);
    assert_eq!(
        fs::read_to_string(dest.join("vivi/SKILL.md")).unwrap(),
        on_disk("skills/vivi/SKILL.md")
    );
}

fn on_disk(rel: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap()
}

fn vivi(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
