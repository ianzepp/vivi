//! `vivi flow list|show` through the real binary.

use std::process::{Command, Output};

fn vivi(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn list_shows_the_three_shipped_kinds() {
    let out = vivi(&["flow", "list"]);
    assert!(out.status.success(), "{out:?}");
    let lines: Vec<String> = stdout(&out).lines().map(str::to_string).collect();
    assert_eq!(lines, ["build", "research", "release"]);

    let json = vivi(&["flow", "list", "--json"]);
    let value: serde_json::Value = serde_json::from_str(&stdout(&json)).unwrap();
    assert_eq!(value, serde_json::json!(["build", "research", "release"]));
}

#[test]
fn show_prints_the_shipped_source_and_rejects_unknown_kinds() {
    let out = vivi(&["flow", "show", "build"]);
    assert!(out.status.success(), "{out:?}");
    assert!(stdout(&out).contains("cq -->|lock, failed| locked"));

    let json = vivi(&["flow", "show", "build", "--json"]);
    let value: serde_json::Value = serde_json::from_str(&stdout(&json)).unwrap();
    assert_eq!(value["kind"], "build");
    assert!(
        value["mermaid"]
            .as_str()
            .unwrap()
            .contains("cq -->|lock, failed| locked")
    );
    assert!(value.get("runs").is_none());

    assert!(!vivi(&["flow", "show", "bogus"]).status.success());
}

#[test]
fn show_with_need_and_no_pump_db_says_so() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    assert!(
        vivi(&["mailspace", "init", "--project", root])
            .status
            .success()
    );

    let out = vivi(&[
        "flow",
        "show",
        "build",
        "--need",
        "abc12345",
        "--project",
        root,
    ]);
    assert!(out.status.success(), "{out:?}");
    let text = stdout(&out);
    assert!(text.contains("cq -->|lock, failed| locked"), "{text}");
    assert!(text.trim_end().ends_with("no pump.db here"), "{text}");

    let json = vivi(&[
        "flow",
        "show",
        "build",
        "--need",
        "abc12345",
        "--project",
        root,
        "--json",
    ]);
    let value: serde_json::Value = serde_json::from_str(&stdout(&json)).unwrap();
    assert!(value["runs"].is_null());
}
