//! `vivi pool dispatch` and the MCP bridge: a dispatch on a pool with
//! `[pools.<name>.bridge] enabled = true` mints exactly one project API key
//! through the dispatching mailspace's own member, names only its key id,
//! and a refused dispatch touches neither the server nor the slot. A
//! collected job's key is revoked at once and only that key; `pool destroy
//! --all` sweeps every live key labelled for the pool and only those; a
//! revoke that cannot be done warns and never fails the collect or the
//! destroy. Child processes against the loopback fake of the project server.

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

/// The standin harness: finishes at once, so `settle` is quick.
const ECHO_HARNESS: &str = "echo {model} >> a.txt";
/// A harness that keeps the one slot busy while a second dispatch runs.
const SLEEPING_HARNESS: &str = "sleep 5; echo {model} >> a.txt";

/// The charter of the role fixture, as in the role dispatch tests.
const CHARTER: &str = "Audit faithfully.\nReport every finding.\n";

/// A mailspace with member `alice` registered on `server`, a `[sync]` table
/// naming `sync_server` (a closed port when the bridge must find the server
/// down), and a one-slot local worktree pool `p` carrying `bridge` as its
/// `[pools.p.bridge]` table (empty for no bridge at all) and `harness` as
/// its standin command.
fn rig(server: &ProjectServer, sync_server: &str, bridge: &str, harness: &str) -> Rig {
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
    let origin = base.join("origin.git");
    git(
        &repo,
        &["clone", "-q", "--bare", ".", origin.to_str().unwrap()],
    );
    git(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&repo, &["fetch", "-q", "origin"]);
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
         \n[pools.p.harness]\nname = \"standin\"\nrun = \"{harness}\"\n\
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

/// Give the rig a role `helper` of kind hand carrying a charter, so a task
/// addressed to it dispatches with the charter as its standing prompt.
fn with_chartered_role(rig: &Rig) {
    let root = rig.project.to_str().unwrap();
    ok(&vivi(&[
        "role",
        "add",
        "helper",
        "--kind",
        "hand",
        "--project",
        root,
    ]));
    let charter = rig.project.parent().unwrap().join("charter.md");
    std::fs::write(&charter, CHARTER).unwrap();
    ok(&vivi(&[
        "role",
        "charter",
        "set",
        "helper",
        "--body-file",
        charter.to_str().unwrap(),
        "--project",
        root,
    ]));
}

/// Send a task addressed to `to` and dispatch it on pool `p` with the plain
/// standin model, like [`dispatch`] but to a role.
fn dispatch_to(rig: &Rig, to: &str, branch: &str) -> Output {
    let sent = vivi(&[
        "task",
        "send",
        "--project",
        rig.project.to_str().unwrap(),
        "--from",
        "mind",
        "--to",
        to,
        "--subject",
        branch,
        "--body",
        "work",
    ]);
    let task = String::from_utf8_lossy(&sent.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("created "))
        .and_then(|r| r.split_whitespace().nth(1))
        .map(str::to_string)
        .expect("a task handle");
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

/// The one `<name>` file under the slots' `job` directories (e.g. rules.md).
fn job_file(rig: &Rig, name: &str) -> PathBuf {
    let lanes = rig.project.parent().unwrap().join("lanes");
    let mut found = Vec::new();
    for dir in walk_dirs(&lanes) {
        if dir.file_name().is_some_and(|n| n == "job") {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                if entry.file_name().to_str() == Some(name) {
                    found.push(entry.path());
                }
            }
        }
    }
    assert_eq!(found.len(), 1, "one job/{name} under the slots: {found:?}");
    found.pop().unwrap()
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

/// `pool wait` in human mode, so the `revoked bridge key` line reaches
/// standard error (porcelain mode drops it).
fn wait_human(rig: &Rig) -> Output {
    vivi(&[
        "pool",
        "wait",
        "--timeout",
        "120",
        "--project",
        rig.project.to_str().unwrap(),
    ])
}

/// `pool destroy --pool p --all --yes`.
fn destroy_all(rig: &Rig) -> Output {
    vivi(&[
        "pool",
        "destroy",
        "--pool",
        "p",
        "--all",
        "--yes",
        "--project",
        rig.project.to_str().unwrap(),
    ])
}

/// Hand-mint a key with `label` through the CLI, as the mailspace's own
/// member; returns its key id.
fn mint(project: &Path, label: &str) -> String {
    let said = ok(&vivi(&[
        "sync",
        "api-key",
        "mint",
        "--label",
        label,
        "--project",
        project.to_str().unwrap(),
    ]));
    said.lines()
        .find_map(|l| l.strip_prefix("key_id "))
        .map(str::to_string)
        .expect("the mint prints its key id")
}

/// The id the fake gave the key minted with `label` (it must exist).
fn key_id_by_label(server: &ProjectServer, label: &str) -> String {
    server
        .keys()
        .iter()
        .find(|key| key.label == label)
        .unwrap_or_else(|| panic!("no key labelled {label}"))
        .key_id
        .clone()
}

/// Whether the fake holds the key minted with `label` as revoked (it must
/// exist).
fn revoked_by_label(server: &ProjectServer, label: &str) -> bool {
    server
        .keys()
        .iter()
        .find(|key| key.label == label)
        .unwrap_or_else(|| panic!("no key labelled {label}"))
        .revoked
}

/// Point the mailspace's `[sync]` at `url` — after the dispatch, so a collect
/// or a destroy finds the server gone.
fn repoint_sync(rig: &Rig, url: &str) {
    let config = rig.project.join(".vivi/mailspace.toml");
    let current = std::fs::read_to_string(&config).unwrap();
    let changed: Vec<String> = current
        .lines()
        .map(|l| {
            if l.starts_with("server = ") {
                format!("server = \"{url}\"")
            } else {
                l.to_string()
            }
        })
        .collect();
    std::fs::write(&config, format!("{}\n", changed.join("\n"))).unwrap();
}

/// A second mailspace on the same fake, admitted as `name`, so a key can be
/// minted that the rig's own member is not allowed to revoke.
fn member_mailspace(server: &ProjectServer, name: &str) -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    let dir = project.path().join(".vivi");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("mailspace.toml"),
        format!(
            "name = \"neighbour\"\n\n[[identities]]\nname = \"mind\"\n\n[sync]\n\
             server = \"{}\"\nproject = \"{}\"\nname = \"{name}\"\n",
            server.url(),
            server.project(),
        ),
    )
    .unwrap();
    let pem = ok(&vivi(&[
        "sync",
        "key",
        "--project",
        project.path().to_str().unwrap(),
    ]));
    server.register(name, pem.trim());
    project
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
    let rig = rig(
        &server,
        &server.url(),
        "[pools.p.bridge]\nenabled = true\n",
        ECHO_HARNESS,
    );
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
    let rig = rig(
        &server,
        &down,
        "[pools.p.bridge]\nenabled = true\n",
        ECHO_HARNESS,
    );
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
    let rig = rig(&server, &server.url(), "", ECHO_HARNESS);
    let out = dispatch(&rig, "feat/plain");
    let said = ok(&out);

    assert!(!said.contains("bridge key"), "{said}");
    assert_eq!(server.requests_seen(), 0, "no request reached the fake");
    assert!(server.keys().is_empty(), "nothing was minted");

    settle(&rig);
    assert_eq!(server.requests_seen(), 0);
    assert!(server.keys().is_empty());
}

#[test]
fn a_collected_bridge_job_s_key_is_revoked_right_away_and_a_hand_minted_key_stays_live() {
    let server = ProjectServer::start();
    let rig = rig(
        &server,
        &server.url(),
        "[pools.p.bridge]\nenabled = true\n",
        ECHO_HARNESS,
    );
    mint(rig.project.as_path(), "hand-made");

    let said = ok(&dispatch(&rig, "feat/collect"));
    let label = format!("pool-p-{}", job_of(&said));
    let key_id = key_id_by_label(&server, &label);

    let out = wait_human(&rig);
    let said = text(&out);
    assert!(out.status.success(), "{said}");
    assert!(
        said.contains(&format!("revoked bridge key {key_id}")),
        "the human wait says which key went: {said}"
    );
    assert_eq!(
        server.keys().iter().filter(|key| key.revoked).count(),
        1,
        "exactly the job's key: {said}"
    );
    assert!(
        revoked_by_label(&server, &label),
        "the job's key is revoked"
    );
    assert!(
        !revoked_by_label(&server, "hand-made"),
        "a hand-minted key is not the pool's to revoke"
    );
}

#[test]
fn destroying_the_pool_sweeps_its_labels_and_leaves_every_other_key_alone() {
    let server = ProjectServer::start();
    let rig = rig(
        &server,
        &server.url(),
        "[pools.p.bridge]\nenabled = true\n",
        ECHO_HARNESS,
    );
    mint(rig.project.as_path(), "hand-made");
    let said = ok(&dispatch(&rig, "feat/sweep"));
    let job_label = format!("pool-p-{}", job_of(&said));
    settle(&rig);

    // A live key wearing the pool's own label shape: what a lost or
    // uncollected job leaves behind.
    let own = mint(rig.project.as_path(), "pool-p-abc012def456");
    mint(rig.project.as_path(), "pool-q-abc012def456");

    let out = destroy_all(&rig);
    let said = text(&out);
    assert!(out.status.success(), "{said}");
    assert!(
        said.contains(&format!("revoked bridge key {own} (pool-p-abc012def456)")),
        "{said}"
    );
    assert!(
        revoked_by_label(&server, "pool-p-abc012def456"),
        "the pool's own leftover key is swept"
    );
    assert!(
        revoked_by_label(&server, &job_label),
        "the collected job's key (revoked at collect)"
    );
    assert!(
        !revoked_by_label(&server, "pool-q-abc012def456"),
        "another pool's key is not touched"
    );
    assert!(
        !revoked_by_label(&server, "hand-made"),
        "a hand-minted key is not touched"
    );
}

#[test]
fn with_the_server_down_a_collect_still_succeeds_and_warns_the_manual_command() {
    let server = ProjectServer::start();
    let rig = rig(
        &server,
        &server.url(),
        "[pools.p.bridge]\nenabled = true\n",
        ECHO_HARNESS,
    );
    let said = ok(&dispatch(&rig, "feat/down"));
    let label = format!("pool-p-{}", job_of(&said));

    repoint_sync(&rig, &format!("http://127.0.0.1:{}", closed_port()));
    let out = wait_human(&rig);
    let said = text(&out);
    assert!(
        out.status.success(),
        "the collect is not the revoke's hostage: {said}"
    );
    assert!(said.contains("warning: the bridge key of job"), "{said}");
    assert!(
        said.contains(&label),
        "the server being down, the warning names the key by its label: {said}"
    );
    assert!(
        said.contains("vivi sync api-key revoke <key_id>"),
        "the warning names the exact manual command: {said}"
    );
    assert!(
        !revoked_by_label(&server, &label),
        "nothing was revoked through the dead server"
    );
}

#[test]
fn with_the_server_down_a_destroy_still_succeeds_and_warns_the_manual_command() {
    let server = ProjectServer::start();
    let rig = rig(
        &server,
        &server.url(),
        "[pools.p.bridge]\nenabled = true\n",
        ECHO_HARNESS,
    );
    mint(rig.project.as_path(), "pool-p-abc012def456");

    repoint_sync(&rig, &format!("http://127.0.0.1:{}", closed_port()));
    let out = destroy_all(&rig);
    let said = text(&out);
    assert!(
        out.status.success(),
        "the destroy is not the sweep's hostage: {said}"
    );
    assert!(
        said.contains("warning: the bridge keys of pool p could not be revoked"),
        "{said}"
    );
    assert!(
        said.contains("vivi sync api-key revoke <key_id>"),
        "the warning names the exact manual command: {said}"
    );
    assert!(
        !revoked_by_label(&server, "pool-p-abc012def456"),
        "nothing was revoked through the dead server"
    );
}

#[test]
fn a_key_the_tool_may_not_revoke_is_warned_by_id_and_left_to_the_manual_command() {
    let server = ProjectServer::start();
    let rig = rig(
        &server,
        &server.url(),
        "[pools.p.bridge]\nenabled = true\n",
        ECHO_HARNESS,
    );
    mint(rig.project.as_path(), "pool-p-ffffffffffff");
    let bob = member_mailspace(&server, "bob");
    let foreign = mint(bob.path(), "pool-p-abc012def456");

    let out = destroy_all(&rig);
    let said = text(&out);
    assert!(
        out.status.success(),
        "the destroy is not the sweep's hostage: {said}"
    );
    assert!(
        said.contains(&format!(
            "warning: bridge key {foreign} (pool-p-abc012def456) could not be revoked"
        )),
        "{said}"
    );
    assert!(
        said.contains(&format!("vivi sync api-key revoke {foreign}")),
        "the warning names the exact manual command: {said}"
    );
    assert!(
        revoked_by_label(&server, "pool-p-ffffffffffff"),
        "the pool's own keys still go"
    );
    assert!(
        !revoked_by_label(&server, "pool-p-abc012def456"),
        "only its minter can revoke it"
    );
}

#[test]
fn a_pool_without_a_bridge_table_never_contacts_the_server_while_waiting_or_destroying() {
    let server = ProjectServer::start();
    let rig = rig(&server, &server.url(), "", ECHO_HARNESS);
    let said = ok(&dispatch(&rig, "feat/quiet"));
    assert!(!said.contains("bridge key"), "{said}");

    settle(&rig);
    assert_eq!(server.requests_seen(), 0, "the wait never called");

    ok(&destroy_all(&rig));
    assert_eq!(server.requests_seen(), 0, "the destroy never called");
    assert!(
        server.keys().is_empty(),
        "there was never anything to revoke"
    );
}

#[test]
fn a_failed_dispatch_revokes_the_minted_key_and_names_only_its_id() {
    let server = ProjectServer::start();
    let rig = rig(
        &server,
        &server.url(),
        "[pools.p.bridge]\nenabled = true\n",
        SLEEPING_HARNESS,
    );
    let first = ok(&dispatch(&rig, "feat/bridge"));
    let first_job = job_of(&first);
    let minted = server.keys();
    assert_eq!(minted.len(), 1, "the running dispatch minted one: {first}");
    assert!(!minted[0].revoked, "{first}");

    // The one slot is busy with the sleeping job: the second dispatch mints
    // its own key and only then fails to claim a slot.
    let out = dispatch(&rig, "feat/busy");
    assert!(
        !out.status.success(),
        "the busy slot must refuse the dispatch: {}",
        text(&out)
    );
    let said = text(&out);
    assert!(
        said.contains("no free slot"),
        "the failure is the claim: {said}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let Some(key_id) = stderr
        .lines()
        .find_map(|l| {
            l.split("revoked bridge key ")
                .nth(1)
                .and_then(|rest| rest.split_whitespace().next())
        })
        .map(str::to_string)
    else {
        panic!("the stderr names the revoked key id: {stderr}");
    };
    let keys = server.keys();
    let listed: Vec<String> = keys.iter().map(|k| k.key_id.clone()).collect();
    assert_eq!(keys.len(), 2, "each attempt minted exactly once: {said}");
    let Some(revoked) = keys.iter().find(|k| k.key_id == key_id) else {
        panic!("the fake lists the minted key {key_id}: {listed:?}");
    };
    assert!(
        revoked.revoked,
        "the failed dispatch revoked its key: {listed:?}"
    );
    let live = keys
        .iter()
        .find(|k| k.key_id != key_id)
        .expect("the running dispatch's key");
    assert!(!live.revoked, "the running dispatch keeps its key");
    assert_eq!(live.label, format!("pool-p-{first_job}"), "{first}");
    assert!(
        !said.contains(&revoked.secret),
        "the raw key is never printed: {said}"
    );

    settle(&rig);
    let keys = server.keys();
    let listed: Vec<String> = keys.iter().map(|k| k.key_id.clone()).collect();
    assert_eq!(keys.len(), 2, "collection mints and revokes nothing more");
    assert!(
        keys.iter()
            .find(|k| k.key_id == live.key_id)
            .is_some_and(|k| k.revoked),
        "collection revoked the collected job's key: {listed:?}"
    );
}

#[test]
fn a_chartered_role_on_a_bridge_pool_gets_the_bridge_note_and_off_the_no_bridge_note() {
    let server = ProjectServer::start();
    let bridged = rig(
        &server,
        &server.url(),
        "[pools.p.bridge]\nenabled = true\n",
        ECHO_HARNESS,
    );
    with_chartered_role(&bridged);
    let said = ok(&dispatch_to(&bridged, "helper", "feat/role-bridge"));
    assert!(
        said.contains("bridge key"),
        "the bridge minted a key: {said}"
    );
    settle(&bridged);
    let rules = std::fs::read_to_string(job_file(&bridged, "rules.md")).unwrap();
    assert!(rules.contains("Standing charter (role helper)"), "{rules}");
    assert!(
        rules.contains("vivi tools of the project server"),
        "the bridge note tells the worker its vivi tools: {rules}"
    );
    assert!(
        !rules.contains("no `vivi` command"),
        "the no-bridge note is gone with the bridge on: {rules}"
    );

    let plain = rig(&server, &server.url(), "", ECHO_HARNESS);
    with_chartered_role(&plain);
    let said = ok(&dispatch_to(&plain, "helper", "feat/role-plain"));
    assert!(!said.contains("bridge key"), "no bridge, no mint: {said}");
    settle(&plain);
    let rules = std::fs::read_to_string(job_file(&plain, "rules.md")).unwrap();
    assert!(rules.contains("Standing charter (role helper)"), "{rules}");
    assert!(
        rules.contains("no `vivi` command"),
        "without the bridge the worker is told it has no vivi: {rules}"
    );
    assert!(
        !rules.contains("vivi tools of the project server"),
        "the bridge note is gone with the bridge off: {rules}"
    );
}
