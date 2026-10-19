//! `mailspace init` and `sync join` ensure an `operator` role of kind
//! `operator`, so mail from `operator@<prefix>.local` speaks for a role that
//! may close and reopen work it did not author.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run(project: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .arg("--project")
        .arg(project)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
    stdout(output)
}

fn config_path(project: &Path) -> PathBuf {
    project.join(".vivi").join("mailspace.toml")
}

fn config(project: &Path) -> String {
    std::fs::read_to_string(config_path(project)).unwrap()
}

/// A project holding a hand-written `.vivi/mailspace.toml`.
fn with_config(body: &str) -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    let dir = project.path().join(".vivi");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mailspace.toml"), body).unwrap();
    project
}

#[test]
fn mailspace_init_ensures_an_operator_role() {
    let project = tempfile::tempdir().unwrap();
    ok(&run(project.path(), &["mailspace", "init"]));

    let toml = config(project.path());
    assert!(
        toml.contains("name = \"operator\""),
        "no operator identity in:\n{toml}"
    );
    assert!(
        toml.contains("kind = \"operator\""),
        "operator identity has no operator kind in:\n{toml}"
    );

    // A second init is idempotent: the role is not duplicated.
    ok(&run(project.path(), &["mailspace", "init"]));
    let again = config(project.path());
    assert_eq!(
        again.matches("name = \"operator\"").count(),
        1,
        "operator role duplicated in:\n{again}"
    );
}

#[test]
fn mailspace_init_sets_the_kind_of_a_kindless_operator_role() {
    let project = with_config("name = \"cli\"\n\n[[identities]]\nname = \"operator\"\n");
    assert!(!config(project.path()).contains("kind = \"operator\""));

    ok(&run(project.path(), &["mailspace", "init"]));

    let toml = config(project.path());
    assert!(
        toml.contains("kind = \"operator\""),
        "kind not set on the existing operator role in:\n{toml}"
    );
    assert_eq!(
        toml.matches("name = \"operator\"").count(),
        1,
        "operator role duplicated in:\n{toml}"
    );
}

#[test]
fn sync_join_ensures_the_operator_role_before_the_server() {
    // The `synced()` shape: one mind identity and a `[sync]` table pointing at
    // a loopback port nothing listens on.
    let project = with_config(
        "name = \"cli\"\n\n[[identities]]\nname = \"mind\"\n\n[sync]\n\
         server = \"http://127.0.0.1:9\"\nproject = \"abcde-fghjk\"\nname = \"alice\"\n",
    );

    let output = run(project.path(), &["sync", "join"]);
    assert!(
        !output.status.success(),
        "join should fail against a dead server\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );

    let toml = config(project.path());
    assert!(
        toml.contains("name = \"operator\""),
        "no operator identity after a failed join in:\n{toml}"
    );
    assert!(
        toml.contains("kind = \"operator\""),
        "operator identity has no operator kind after a failed join in:\n{toml}"
    );
}
