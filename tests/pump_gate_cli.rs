//! `vivi pump gate show|set` through the real binary.

use std::fs;
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
fn gate_set_then_show_round_trips_and_rejects_blank() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    assert!(
        vivi(&["mailspace", "init", "--project", root])
            .status
            .success()
    );
    let config = project.path().join(".vivi").join("mailspace.toml");

    let shown = vivi(&["pump", "gate", "show", "--project", root]);
    assert!(shown.status.success());
    assert!(stdout(&shown).contains("no gate command set"));

    let set = vivi(&[
        "pump",
        "gate",
        "set",
        "cargo test --quiet",
        "--project",
        root,
    ]);
    assert!(set.status.success(), "{set:?}");
    let shown = vivi(&["pump", "gate", "show", "--project", root]);
    assert_eq!(stdout(&shown), "cargo test --quiet\n");

    let before = fs::read_to_string(&config).unwrap();
    let blank = vivi(&["pump", "gate", "set", "  ", "--project", root]);
    assert!(!blank.status.success());
    assert_eq!(fs::read_to_string(&config).unwrap(), before);
}
