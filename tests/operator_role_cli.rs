//! `mailspace init` and `sync join` provision the built-in operator and merge
//! roles before a project relies on them.

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

fn role(project: &Path, name: &str) -> serde_json::Value {
    let name_and_flag = ["role", "show", name, "--json"];
    serde_json::from_str(&ok(&run(project, &name_and_flag))).unwrap()
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
fn mailspace_init_provisions_the_merge_role_and_default_charter_idempotently() {
    let project = tempfile::tempdir().unwrap();
    ok(&run(project.path(), &["mailspace", "init"]));

    let merge = role(project.path(), "merge");
    assert_eq!(merge["name"], "merge");
    assert_eq!(merge["kind"], "merge");
    assert_eq!(merge["status"], "active");
    assert!(
        merge["pool"].is_null(),
        "pool is left to role routing: {merge}"
    );
    assert!(
        merge["model"].is_null(),
        "model is left to role routing: {merge}"
    );
    assert_eq!(merge["has_charter"], true);
    let charter = merge["charter"]
        .as_str()
        .unwrap()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for term in [
        "`pump-job-kind` is `sync`",
        "`merged` or `conflict`",
        "observed target SHA",
        "expected merged ancestry",
    ] {
        assert!(charter.contains(term), "charter lacks {term:?}:\n{charter}");
    }

    ok(&run(project.path(), &["mailspace", "init"]));
    let again = role(project.path(), "merge");
    assert_eq!(again["charter"], merge["charter"]);
    let toml = config(project.path());
    assert_eq!(toml.matches("name = \"merge\"").count(), 1, "{toml}");
}

#[test]
fn mailspace_init_preserves_merge_role_customizations_and_charter() {
    let project = with_config(
        "name = \"cli\"\n\n[[identities]]\nname = \"merge\"\nkind = \"review\"\n\
         status = \"parked\"\nprovider = \"custom-provider\"\nmodel = \"custom-model\"\n\
         thinking = \"high\"\npool = \"custom-pool\"\n",
    );
    let charter_dir = project.path().join(".vivi/charters");
    std::fs::create_dir_all(&charter_dir).unwrap();
    std::fs::write(charter_dir.join("merge.md"), "operator charter\n").unwrap();

    ok(&run(project.path(), &["mailspace", "init"]));

    let merge = role(project.path(), "merge");
    assert_eq!(merge["kind"], "review");
    assert_eq!(merge["status"], "parked");
    assert_eq!(merge["provider"], "custom-provider");
    assert_eq!(merge["model"], "custom-model");
    assert_eq!(merge["thinking"], "high");
    assert_eq!(merge["pool"], "custom-pool");
    assert_eq!(merge["charter"], "operator charter\n");
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
    let merge = role(project.path(), "merge");
    assert_eq!(merge["kind"], "merge");
    assert_eq!(merge["has_charter"], true);
}
