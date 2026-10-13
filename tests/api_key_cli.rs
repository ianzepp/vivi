//! `vivi sync api-key mint|list|revoke` at the CLI: child processes against
//! the fake project server. The raw key is printed exactly once, is found in
//! no file under the mailspace's `.vivi/`, and label/id validation refuses
//! before any request reaches the fake.

#[path = "support/project_server.rs"]
mod project_server;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use project_server::ProjectServer;

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

/// The value after `prefix` on the first line that carries it.
fn value_after<'a>(text: &'a str, prefix: &str) -> String {
    text.lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or("")
        .to_string()
}

/// A mailspace whose `[sync]` table points at `server`, named alice.
fn mailspace(server: &str) -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    let dir = project.path().join(".vivi");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("mailspace.toml"),
        format!(
            "name = \"cli\"\n\n[[identities]]\nname = \"mind\"\n\n[sync]\n\
             server = \"{server}\"\nproject = \"abcde-fghjk\"\nname = \"alice\"\n"
        ),
    )
    .unwrap();
    project
}

/// A fake whose alice is this mailspace's identity: `vivi sync key` mints
/// the mailspace's key (local, no server), the parent registers the printed
/// PEM with the fake.
fn registered() -> (tempfile::TempDir, ProjectServer) {
    let server = ProjectServer::start();
    let project = mailspace(&server.url());
    let pem = ok(&run(project.path(), &["sync", "key"]));
    server.register("alice", pem.trim());
    (project, server)
}

/// Mint one key on `project`'s mailspace, returning (key_id, raw key).
fn mint_ok(project: &Path, label: &str) -> (String, String) {
    let out = run(project, &["sync", "api-key", "mint", "--label", label]);
    let printed = ok(&out);
    let key_id = value_after(&printed, "key_id ");
    let raw = value_after(&printed, "key ");
    assert!(key_id.starts_with("key_"), "{printed}");
    assert!(raw.starts_with("vivi_"), "{printed}");
    (key_id, raw)
}

/// Every file under `dir`, however deep.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}

#[test]
fn mint_prints_the_key_once_and_no_file_under_vivi_holds_it() {
    let (project, server) = registered();

    let out = run(
        project.path(),
        &["sync", "api-key", "mint", "--label", "ci"],
    );
    let printed = ok(&out);

    let lines: Vec<&str> = printed.lines().collect();
    assert_eq!(lines.len(), 3, "exactly the three lines: {printed}");
    let key_id = lines[0].strip_prefix("key_id ").unwrap();
    assert_eq!(lines[1], "label ci");
    let raw = lines[2].strip_prefix("key ").unwrap();
    assert!(raw.starts_with("vivi_"), "{printed}");
    assert_eq!(
        lines.iter().filter(|line| line.starts_with("key ")).count(),
        1,
        "exactly one `key ` line: the raw key, once"
    );

    let said = stderr(&out);
    assert!(
        said.contains("the key is shown once; Vivi does not store it"),
        "{said}"
    );
    assert!(
        said.contains(&format!("vivi sync api-key revoke {key_id}")),
        "{said}"
    );

    let keys = server.keys();
    assert_eq!(keys.len(), 1, "the fake has it");
    assert_eq!(keys[0].key_id, key_id);
    assert_eq!(keys[0].label, "ci");
    assert_eq!(keys[0].created_by, "alice");
    assert!(!keys[0].revoked);
    assert_eq!(keys[0].secret, raw);

    for path in walk(&project.path().join(".vivi")) {
        let contents = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !contents.contains(raw),
            "{} holds the raw key",
            path.display()
        );
    }
}

#[test]
fn list_shows_the_live_key_and_revoke_ends_it() {
    let (project, server) = registered();
    let (key_id, raw) = mint_ok(project.path(), "ci");

    let listed = ok(&run(project.path(), &["sync", "api-key", "list"]));

    assert!(listed.contains(&key_id), "{listed}");
    assert!(listed.contains("ci"), "{listed}");
    assert!(listed.contains("alice"), "{listed}");
    assert!(!listed.contains(&raw), "never the raw key: {listed}");

    let revoked = ok(&run(
        project.path(),
        &["sync", "api-key", "revoke", &key_id],
    ));
    assert_eq!(revoked, format!("revoked {key_id}\n"));
    assert!(server.keys()[0].revoked, "the fake shows it revoked");

    let after = ok(&run(project.path(), &["sync", "api-key", "list"]));
    assert_eq!(
        after, "no api keys\n",
        "staging keeps revoked keys server-side; the list shows live keys only"
    );
}

#[test]
fn revoking_an_unknown_id_fails_with_a_message_and_no_stdout() {
    let (project, _server) = registered();
    mint_ok(project.path(), "ci");

    let out = run(
        project.path(),
        &["sync", "api-key", "revoke", "key_never_minted"],
    );

    assert!(!out.status.success());
    assert_eq!(stdout(&out), "", "nothing on stdout");
    let said = stderr(&out);
    assert!(said.contains("key_never_minted"), "{said}");
    assert!(said.contains("not minted by this member"), "{said}");
}

#[test]
fn a_bad_label_is_refused_before_any_request_reaches_the_fake() {
    let server = ProjectServer::start();
    let project = mailspace(&server.url());
    let long = "x".repeat(65);

    for label in ["", long.as_str()] {
        let out = run(
            project.path(),
            &["sync", "api-key", "mint", "--label", label],
        );
        assert!(!out.status.success());
        assert_eq!(stdout(&out), "", "nothing on stdout");
        let said = stderr(&out);
        assert!(said.contains("label"), "{label:?}: {said}");
        assert!(said.contains("1 to 64"), "{label:?}: {said}");
    }
    assert_eq!(
        server.requests_seen(),
        0,
        "validation refuses before any request"
    );
}
