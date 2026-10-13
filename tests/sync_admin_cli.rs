//! The sync admin commands at the CLI, for what needs no server: `sync key`
//! against a fresh mailspace, and the checks that refuse a bad `sync vouch`
//! before any request is made.

use std::path::Path;
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

/// A mailspace with a `[sync]` table pointing at a port nothing listens on.
fn synced() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    let dir = project.path().join(".vivi");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("mailspace.toml"),
        "name = \"cli\"\n\n[[identities]]\nname = \"mind\"\n\n[sync]\n\
         server = \"http://127.0.0.1:9\"\nproject = \"abcde-fghjk\"\nname = \"alice\"\n",
    )
    .unwrap();
    project
}

/// A mailspace whose `[sync]` table names only the server.
fn half_synced() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    let dir = project.path().join(".vivi");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("mailspace.toml"),
        "name = \"cli\"\n\n[[identities]]\nname = \"mind\"\n\n[sync]\n\
         server = \"http://127.0.0.1:9\"\n",
    )
    .unwrap();
    project
}

#[test]
fn sync_key_prints_the_public_key_without_a_sync_table() {
    let project = tempfile::tempdir().unwrap();
    ok(&run(project.path(), &["mailspace", "init"]));

    let out = ok(&run(project.path(), &["sync", "key"]));

    assert!(out.starts_with("-----BEGIN PUBLIC KEY-----\n"), "{out}");
    assert!(out.contains("-----END PUBLIC KEY-----"), "{out}");
}

#[test]
fn a_bad_pem_is_refused_before_any_request() {
    let project = synced();

    let out = run(
        project.path(),
        &["sync", "vouch", "bob", "--key", "not a pem"],
    );

    assert!(!out.status.success());
    let error = stderr(&out);
    assert!(error.contains("public key PEM"), "{error}");
    assert!(
        !error.contains("unreachable"),
        "the request must not be made: {error}"
    );
}

#[test]
fn vouch_needs_a_key_argument() {
    let project = synced();

    let out = run(project.path(), &["sync", "vouch", "bob"]);

    assert!(!out.status.success(), "one of --key-file/--key is required");
}

#[test]
fn vouch_needs_the_full_sync_table() {
    let project = tempfile::tempdir().unwrap();
    let dir = project.path().join(".vivi");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("mailspace.toml"),
        "name = \"cli\"\n\n[[identities]]\nname = \"mind\"\n",
    )
    .unwrap();

    let out = run(
        project.path(),
        &["sync", "vouch", "bob", "--key", "not a pem"],
    );

    assert!(!out.status.success());
    let error = stderr(&out);
    assert!(error.contains("[sync]"), "{error}");
}

#[test]
fn create_needs_a_server_and_a_creator_name() {
    let project = tempfile::tempdir().unwrap();
    let dir = project.path().join(".vivi");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("mailspace.toml"),
        "name = \"cli\"\n\n[[identities]]\nname = \"mind\"\n",
    )
    .unwrap();

    let out = run(project.path(), &["sync", "create", "--name", "alice"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("[sync] server"), "{}", stderr(&out));

    let half = half_synced();
    let out = run(half.path(), &["sync", "create"]);
    assert!(!out.status.success());
    let error = stderr(&out);
    assert!(error.contains("--name"), "{error}");
    assert!(error.contains("[sync] name"), "{error}");
}
