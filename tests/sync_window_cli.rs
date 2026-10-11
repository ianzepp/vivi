//! The pull-window commands at the CLI, for what needs no server: the
//! history line in `sync status`, `sync prune`, and the checks that refuse a
//! bad `--since`, handle, or `--depth` before any request is made.

use std::path::Path;
use std::process::{Command, Output};

const PROJECT_ID: &str = "abcde-fghjk-mnpqr-stvwz-23456";

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
        format!(
            "name = \"cli\"\n\n[[identities]]\nname = \"mind\"\n\n[sync]\n\
             server = \"http://127.0.0.1:9\"\nproject = \"{PROJECT_ID}\"\nname = \"alice\"\n"
        ),
    )
    .unwrap();
    project
}

#[test]
fn status_says_where_history_starts() {
    let project = synced();
    let fresh = ok(&run(project.path(), &["sync", "status"]));
    assert!(
        fresh.contains("sync history from: nothing pulled yet"),
        "{fresh}"
    );

    let sync_dir = project.path().join(".vivi/sync");
    std::fs::create_dir_all(&sync_dir).unwrap();
    std::fs::write(
        sync_dir.join("state.json"),
        format!("{{\"project\": \"{PROJECT_ID}\", \"since\": 7}}"),
    )
    .unwrap();
    let old = ok(&run(project.path(), &["sync", "status"]));
    assert!(old.contains("sync cursor: 7"), "{old}");
    assert!(
        old.contains("sync history from: unknown (pulled before windows)"),
        "{old}"
    );

    std::fs::write(
        sync_dir.join("state.json"),
        format!(
            "{{\"project\": \"{PROJECT_ID}\", \"since\": 9, \
             \"floor\": {{\"sequence\": 4, \"time\": \"2026-10-01T00:00:00Z\"}}}}"
        ),
    )
    .unwrap();
    let windowed = ok(&run(project.path(), &["sync", "status"]));
    assert!(
        windowed.contains("sync history from: 2026-10-01T00:00:00Z (sequence 4)"),
        "{windowed}"
    );
}

#[test]
fn prune_runs_without_a_server_and_dry_run_says_so() {
    let project = tempfile::tempdir().unwrap();
    ok(&run(project.path(), &["mailspace", "init"]));

    let dry = ok(&run(
        project.path(),
        &["sync", "prune", "--older-than", "30d", "--dry-run"],
    ));
    assert!(
        dry.contains("would evict 0 letter(s) dated before"),
        "{dry}"
    );
    assert!(dry.contains("dry run: nothing changed"), "{dry}");

    let real = ok(&run(
        project.path(),
        &["sync", "prune", "--older-than", "30d"],
    ));
    assert!(real.contains("evicted 0 letter(s) dated before"), "{real}");

    let bad = run(project.path(), &["sync", "prune", "--older-than", "soon"]);
    assert!(!bad.status.success());
    assert!(stderr(&bad).contains("invalid"), "{}", stderr(&bad));
    let missing = run(project.path(), &["sync", "prune"]);
    assert!(!missing.status.success(), "--older-than is required");
}

#[test]
fn bad_input_is_refused_before_any_request() {
    let project = synced();

    let since = run(project.path(), &["sync", "pull", "--since", "soon"]);
    assert!(!since.status.success());
    assert!(
        stderr(&since).contains("invalid --since"),
        "{}",
        stderr(&since)
    );

    let handle = run(project.path(), &["sync", "fetch", "xyz"]);
    assert!(!handle.status.success());
    assert!(
        stderr(&handle).contains("not a handle"),
        "{}",
        stderr(&handle)
    );

    let depth = run(project.path(), &["sync", "pull", "--refs", "--depth", "4"]);
    assert!(!depth.status.success());
    assert!(
        stderr(&depth).contains("--depth must be 1 to 3"),
        "{}",
        stderr(&depth)
    );

    let orphan = run(project.path(), &["sync", "pull", "--depth", "2"]);
    assert!(!orphan.status.success(), "--depth needs --refs");
}
