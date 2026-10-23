//! `vivi pump rungs show|set` through the real binary.

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

fn init() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    assert!(
        vivi(&["mailspace", "init", "--project", root])
            .status
            .success()
    );
    project
}

#[test]
fn rungs_set_then_show_round_trips_and_rejects_unknown_rung() {
    let project = init();
    let root = project.path().to_str().unwrap();
    let config = project.path().join(".vivi").join("mailspace.toml");

    let shown = vivi(&["pump", "rungs", "show", "--project", root]);
    assert!(shown.status.success(), "{shown:?}");
    let text = stdout(&shown);
    for name in ["task", "need", "main", "release"] {
        assert!(text.contains(&format!("{name}: (unset)")), "{text}");
    }
    assert!(text.contains("timeout: 1800"), "{text}");
    assert!(text.contains("pools: (any)"), "{text}");

    let set = vivi(&[
        "pump",
        "rungs",
        "set",
        "task",
        "cargo test --quiet",
        "--project",
        root,
    ]);
    assert!(set.status.success(), "{set:?}");
    let text = stdout(&vivi(&["pump", "rungs", "show", "--project", root]));
    assert!(text.contains("task: cargo test --quiet"), "{text}");
    assert!(text.contains("need: (unset)"), "{text}");

    let before = fs::read_to_string(&config).unwrap();
    let unknown = vivi(&["pump", "rungs", "set", "bogus", "true", "--project", root]);
    assert!(!unknown.status.success());
    let blank = vivi(&["pump", "rungs", "set", "need", "  ", "--project", root]);
    assert!(!blank.status.success());
    assert_eq!(fs::read_to_string(&config).unwrap(), before);
}

#[test]
fn rungs_target_override_wins_and_unknown_target_is_refused() {
    let project = init();
    let root = project.path().to_str().unwrap();
    let config = project.path().join(".vivi").join("mailspace.toml");

    let missing = vivi(&[
        "pump",
        "rungs",
        "set",
        "task",
        "true",
        "--target",
        "web",
        "--project",
        root,
    ]);
    assert!(!missing.status.success());

    let mut raw = fs::read_to_string(&config).unwrap();
    raw.push_str("\n[pump.targets.web]\npath = \"web\"\n");
    fs::write(&config, raw).unwrap();

    let set = vivi(&[
        "pump",
        "rungs",
        "set",
        "need",
        "make all",
        "--project",
        root,
    ]);
    assert!(set.status.success(), "{set:?}");
    let set = vivi(&[
        "pump",
        "rungs",
        "set",
        "task",
        "npm test",
        "--target",
        "web",
        "--project",
        root,
    ]);
    assert!(set.status.success(), "{set:?}");
    let text = stdout(&vivi(&[
        "pump",
        "rungs",
        "show",
        "--target",
        "web",
        "--project",
        root,
    ]));
    assert!(text.contains("task: npm test"), "{text}");
    assert!(text.contains("need: make all"), "{text}");
}
