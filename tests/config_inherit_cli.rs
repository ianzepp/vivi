//! `vivi config show | validate` over an inherited base config.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::tempdir;

fn vivi(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

fn write_mailspace(dir: &Path, text: &str) {
    fs::create_dir_all(dir.join(".vivi")).unwrap();
    fs::write(dir.join(".vivi/mailspace.toml"), text).unwrap();
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn show_and_validate_name_the_file_each_value_came_from() {
    let tmp = tempdir().unwrap();
    write_mailspace(
        tmp.path(),
        "base = true\n[pools.studio]\nprovider = \"local\"\nslots = 4\n",
    );
    let project = tmp.path().join("work/proj");
    write_mailspace(
        &project,
        "name = \"proj\"\nid = \"12345678-1234-4234-8234-123456789abc\"\n\
         unset = [\"pools.gone\"]\n[pools.studio]\nslots = 9\n",
    );
    let root = project.to_str().unwrap();
    for command in ["show", "validate"] {
        let output = vivi(&["config", command, "--project", root]);
        let out = text(&output);
        assert!(out.contains("[base]") && out.contains("[project]"), "{out}");
        assert!(
            out.contains(
                &tmp.path()
                    .join(".vivi/mailspace.toml")
                    .display()
                    .to_string()
            ),
            "{out}"
        );
        assert!(
            out.contains("pools.studio: file 1 (provider); file 2 (slots)"),
            "{out}"
        );
    }
    let out = text(&vivi(&["config", "validate", "--project", root]));
    assert!(
        out.contains("unset \"pools.gone\"") && out.contains("matched nothing"),
        "{out}"
    );
    let json = vivi(&["config", "show", "--json", "--project", root]);
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["origins"]["files"].as_array().unwrap().len(), 2);
}

#[test]
fn show_without_a_base_prints_no_origins() {
    let tmp = tempdir().unwrap();
    write_mailspace(
        tmp.path(),
        "name = \"proj\"\nid = \"12345678-1234-4234-8234-123456789abc\"\n",
    );
    let root = tmp.path().to_str().unwrap();
    let out = text(&vivi(&["config", "show", "--project", root]));
    assert!(!out.contains("origins:"), "{out}");
    let json = vivi(&["config", "show", "--json", "--project", root]);
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert!(value.get("origins").is_none());
}
