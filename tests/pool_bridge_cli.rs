//! `vivi pool dispatch` and the MCP bridge: a dispatch on a pool with
//! `[pools.<name>.bridge] enabled = true` mints exactly one project API key
//! through the dispatching mailspace's own member, names only its key id,
//! and a refused dispatch touches neither the server nor the slot. Child
//! processes against the loopback fake of the project server.

#[path = "support/project_server.rs"]
mod project_server;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use project_server::ProjectServer;

fn vivi(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "status: {}\n{}",
        output.status,
        text(output)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

struct Rig {
    _dir: tempfile::TempDir,
    project: PathBuf,
}

/// A mailspace with member `alice` registered on `server`, a `[sync]` table
/// naming `sync_server` (a closed port when the bridge must find the server
/// down), and a one-slot local worktree pool `p` carrying `bridge` as its
/// `[pools.p.bridge]` table (empty for no bridge at all).
fn rig(server: &ProjectServer, sync_server: &str, bridge: &str) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let (project, repo) = (base.join("project"), base.join("repo"));
    std::fs::create_dir(&project).unwrap();
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "T"]);
    git(&repo, &["config", "user.email", "t@localhost"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    let root = project.to_str().unwrap();
    vivi(&["mailspace", "init", "--project", root]);
    for identity in ["mind", "hand"] {
        vivi(&["mailspace", "identity", "add", identity, "--project", root]);
    }
    let pem = ok(&vivi(&["sync", "key", "--project", root]));
    server.register("alice", pem.trim());
    let table = format!(
        "\n[sync]\nserver = \"{sync_server}\"\nproject = \"{}\"\nname = \"alice\"\n\
         \n[pools.p]\nprovider = \"local\"\nslots = 1\n\
         \n[pools.p.local]\nroot = \"{}\"\nworktree_of = \"{}\"\n\
         {bridge}\
         \n[pools.p.harness]\nname = \"standin\"\nrun = \"echo {{model}} >> a.txt\"\n\
         models = [\"standin\"]\n\
         \n[pools.p.committer]\nname = \"bot\"\nemail = \"bot@example.com\"\n",
        server.project(),
        base.join("lanes").display(),
        repo.display(),
    );
    let config = project.join(".vivi/mailspace.toml");
    let mut current = std::fs::read_to_string(&config).unwrap();
    current.push_str(&table);
    std::fs::write(&config, current).unwrap();
    ok(&vivi(&[
        "pool",
        "create",
        "--pool",
        "p",
        "--porcelain",
        "--project",
        root,
    ]));
    Rig { _dir: dir, project }
}

fn handle(rig: &Rig, subject: &str) -> String {
    let sent = vivi(&[
        "task",
        "send",
        "--project",
        rig.project.to_str().unwrap(),
        "--from",
        "mind",
        "--to",
        "hand",
        "--subject",
        subject,
        "--body",
        "work",
    ]);
    String::from_utf8_lossy(&sent.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("created hand "))
        .and_then(|r| r.split_whitespace().next())
        .map(str::to_string)
        .expect("a task handle")
}

fn dispatch(rig: &Rig, branch: &str) -> Output {
    let task = handle(rig, branch);
    vivi(&[
        "pool",
        "dispatch",
        "--pool",
        "p",
        &task,
        "--model",
        "standin",
        "--from",
        "main",
        "--branch",
        branch,
        "--project",
        rig.project.to_str().unwrap(),
    ])
}

/// The `job <id>` token of the human dispatch line.
fn job_of(said: &str) -> String {
    let line = said
        .lines()
        .find(|l| l.starts_with("dispatched"))
        .expect("a dispatch line");
    let mut fields = line.split_whitespace();
    while let Some(field) = fields.next() {
        if field == "job" {
            return fields.next().expect("a job id after `job`").to_string();
        }
    }
    panic!("no `job <id>` on the dispatch line: {line}");
}

/// Let the one job finish and be collected, so no runner outlives the test.
fn settle(rig: &Rig) {
    let waited = ok(&vivi(&[
        "pool",
        "wait",
        "--timeout",
        "120",
        "--porcelain",
        "--project",
        rig.project.to_str().unwrap(),
    ]));
    assert!(waited.contains("collected"), "{waited}");
}

/// A loopback port nothing listens on.
fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// Whether anything was delivered to a slot of the pool: a `job` directory
/// under the pool's home.
fn anything_delivered(rig: &Rig) -> bool {
    let lanes = rig.project.parent().unwrap().join("lanes");
    walk_dirs(&lanes)
        .iter()
        .any(|d| d.file_name().is_some_and(|n| n == "job"))
}

/// Every directory under `dir`, however deep (`dir` itself excluded).
fn walk_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.push(path.clone());
            found.extend(walk_dirs(&path));
        }
    }
    found
}

#[test]
fn a_bridge_dispatch_mints_exactly_one_key_and_names_only_its_id() {
    let server = ProjectServer::start();
    let rig = rig(&server, &server.url(), "[pools.p.bridge]\nenabled = true\n");
    let out = dispatch(&rig, "feat/bridge");
    let said = ok(&out);

    let job = job_of(&said);
    let keys = server.keys();
    assert_eq!(keys.len(), 1, "exactly one mint: {said}");
    assert_eq!(keys[0].label, format!("pool-p-{job}"), "{said}");
    assert_eq!(keys[0].created_by, "alice", "the dispatching member");
    assert!(!keys[0].revoked);
    assert!(keys[0].secret.starts_with("vivi_"), "{:?}", keys[0].secret);

    assert!(
        said.contains(&format!("bridge key {}", keys[0].key_id)),
        "{said}"
    );
    assert!(said.contains(&keys[0].label), "{said}");
    assert!(
        !said.contains(&keys[0].secret),
        "the raw key is never printed: {said}"
    );

    settle(&rig);
    assert_eq!(server.keys().len(), 1, "no second mint anywhere");
}

#[test]
fn a_bridge_dispatch_with_the_server_down_is_refused_before_the_claim() {
    let server = ProjectServer::start();
    let down = format!("http://127.0.0.1:{}", closed_port());
    let rig = rig(&server, &down, "[pools.p.bridge]\nenabled = true\n");
    let out = dispatch(&rig, "feat/bridge");

    assert!(!out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(
        !said.contains("bridge key"),
        "nothing minted, nothing named: {said}"
    );
    assert_eq!(server.requests_seen(), 0, "the fake was never contacted");
    assert!(server.keys().is_empty(), "no key exists");

    let status = ok(&vivi(&[
        "pool",
        "status",
        "--pool",
        "p",
        "--json",
        "--project",
        rig.project.to_str().unwrap(),
    ]));
    let slots: serde_json::Value = serde_json::from_str(&status).unwrap();
    let slot = &slots.as_array().expect("a slot list")[0];
    assert_eq!(slot["state"], "free", "{status}");
    assert!(slot["task"].is_null(), "{status}");
    assert!(!anything_delivered(&rig), "nothing was ever delivered");
}

#[test]
fn a_pool_without_a_bridge_table_mints_nothing_and_contacts_nothing() {
    let server = ProjectServer::start();
    let rig = rig(&server, &server.url(), "");
    let out = dispatch(&rig, "feat/plain");
    let said = ok(&out);

    assert!(!said.contains("bridge key"), "{said}");
    assert_eq!(server.requests_seen(), 0, "no request reached the fake");
    assert!(server.keys().is_empty(), "nothing was minted");

    settle(&rig);
    assert_eq!(server.requests_seen(), 0);
    assert!(server.keys().is_empty());
}
