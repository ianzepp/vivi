//! `vivi sync keep-local` at the CLI: local only, and an unknown handle is a
//! non-zero error naming it.

use std::path::Path;
use std::process::{Command, Output};

/// Run the binary with `--project` last, as the other CLI tests do.
fn vivi(project: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .arg("--project")
        .arg(project)
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// A mailspace with `[sync]` configured, as keep-local requires; the server
/// is never contacted.
fn synced() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let vivi_dir = dir.path().join(".vivi");
    std::fs::create_dir_all(&vivi_dir).unwrap();
    std::fs::write(
        vivi_dir.join("mailspace.toml"),
        "name = \"alice\"\n\n[[identities]]\nname = \"mind\"\n\n\
         [sync]\nserver = \"http://127.0.0.1:9\"\n\
         project = \"abcde-fghjk-mnpqr-stvwz-23456\"\nname = \"alice\"\n",
    )
    .unwrap();
    dir
}

#[test]
fn an_unknown_handle_fails_naming_it() {
    let project = synced();

    let output = vivi(project.path(), &["sync", "keep-local", "deadbeef"]);

    assert!(!output.status.success(), "{}", text(&output));
    let printed = text(&output);
    assert!(printed.contains("deadbeef"), "{printed}");
    assert!(printed.contains("not found"), "{printed}");
}
