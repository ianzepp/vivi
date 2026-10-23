//! End to end, against the loopback fake of the project server: a job
//! dispatched to a local worktree pool with the bridge enabled reads its
//! task through `show` and reports with `reply` and `done`, as the member
//! that minted the job's key. The raw key sits in no file — not the job's
//! slot, not the task thread, not the mailspace — and the runner's own
//! result is the fallback report when the worker never contacts the server.
//! `pool destroy --all` sweeps every key labelled for the pool and leaves
//! the others; a pool without a bridge mints nothing.

#[path = "support/project_server.rs"]
mod project_server;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

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

/// The harness script the rig writes under its temp directory; the pool's
/// `run` text invokes it with the brief's path and the built binary. It
/// reads its task handle out of the brief, then drives `vivi mcp` over
/// stdio: `initialize`, then the `show`, `reply` and `done` tool calls, one
/// JSON-RPC object per line. A brief that names `nobridge` never contacts
/// the server, so the runner's own result is the only report.
const HARNESS: &str = r#"#!/usr/bin/env bash
set -u
brief=$1
vivi_bin=$2
handle=$(sed -n 's/^Your task handle is \([^ ]*\) (.*/\1/p' "$brief" | head -n1)
echo "handle: ${handle:-none}"
if [ -z "$handle" ] || grep -q nobridge "$brief"; then
  echo "no bridge work: the runner's result is the report"
  exit 0
fi
answers=$(mktemp) || exit 4
{
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"harness","version":"0"}}}'
  printf '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"show","arguments":{"handle":"%s","project":"%s"}}}\n' "$handle" "${VIVI_MCP_PROJECT_ID:-}"
  printf '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"reply","arguments":{"handle":"%s","body":"receipt: read the task, checks pass, nothing left undone","project":"%s"}}}\n' "$handle" "${VIVI_MCP_PROJECT_ID:-}"
  printf '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"done","arguments":{"handle":"%s","project":"%s"}}}\n' "$handle" "${VIVI_MCP_PROJECT_ID:-}"
} | "$vivi_bin" mcp >"$answers" 2>"$answers.err"
status=$?
echo "vivi mcp exit $status"
cat "$answers"
if [ -s "$answers.err" ]; then
  echo "vivi mcp stderr:"
  cat "$answers.err"
fi
rm -f "$answers" "$answers.err"
exit "$status"
"#;

struct Rig {
    _dir: tempfile::TempDir,
    project: PathBuf,
    lanes: PathBuf,
}

/// A mailspace with member `alice` registered on `server` and a `[sync]`
/// table naming it, plus a one-slot local worktree pool `p` whose harness
/// drives `vivi mcp` itself. With `bridge`, the pool carries
/// `[pools.p.bridge] enabled = true`, so every dispatch mints one key.
fn rig(server: &ProjectServer, bridge: bool) -> Rig {
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
    let script = base.join("harness.sh");
    std::fs::write(&script, HARNESS).unwrap();
    let bridge_table = if bridge {
        "\n[pools.p.bridge]\nenabled = true\n"
    } else {
        ""
    };
    let table = format!(
        "\n[sync]\nserver = \"{server}\"\nproject = \"{project_id}\"\nname = \"alice\"\n\
         \n[pools.p]\nprovider = \"local\"\nslots = 1\n\
         \n[pools.p.local]\nroot = \"{lanes}\"\nworktree_of = \"{repo}\"\n{bridge_table}\
         \n[pools.p.harness]\nname = \"bridge-hand\"\n\
         run = \"bash '{script}' {{brief}} '{bin}'\"\n\
         models = [\"m\"]\n\
         \n[pools.p.committer]\nname = \"bot\"\nemail = \"bot@example.com\"\n",
        server = server.url(),
        project_id = server.project(),
        lanes = base.join("lanes").display(),
        repo = repo.display(),
        script = script.display(),
        bin = env!("CARGO_BIN_EXE_vivi"),
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
    Rig {
        _dir: dir,
        project,
        lanes: base.join("lanes"),
    }
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

fn dispatch_task(rig: &Rig, task: &str, branch: &str) -> Output {
    vivi(&[
        "pool",
        "dispatch",
        "--pool",
        "p",
        task,
        "--model",
        "m",
        "--from",
        "main",
        "--branch",
        branch,
        "--project",
        rig.project.to_str().unwrap(),
    ])
}

fn dispatch(rig: &Rig, subject: &str, branch: &str) -> (String, Output) {
    let task = handle(rig, subject);
    let out = dispatch_task(rig, &task, branch);
    (task, out)
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
fn mint(rig: &Rig, label: &str) -> String {
    let said = ok(&vivi(&[
        "sync",
        "api-key",
        "mint",
        "--label",
        label,
        "--project",
        rig.project.to_str().unwrap(),
    ]));
    said.lines()
        .find_map(|l| l.strip_prefix("key_id "))
        .map(str::to_string)
        .expect("the mint prints its key id")
}

/// The bodies of a task's thread, in order.
fn thread_bodies(rig: &Rig, task: &str) -> Vec<String> {
    let out = ok(&vivi(&[
        "task",
        "show",
        task,
        "--json",
        "--project",
        rig.project.to_str().unwrap(),
    ]));
    let thread: Value = serde_json::from_str(&out).unwrap();
    thread
        .as_array()
        .expect("a thread array")
        .iter()
        .map(|m| m["body"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The push outcome the runner recorded on the task: the first word after
/// `push: ` in the collected reply of `job` (the reply whose first line is
/// `pool result <job>: <outcome word>`; the outcome line reads e.g.
/// `push: committed <branch> <sha>` or `push: no_changes`).
fn runner_result_word(rig: &Rig, task: &str, job: &str) -> String {
    let marker = format!("pool result {job}:");
    thread_bodies(rig, task)
        .iter()
        .find(|body| body.contains(&marker))
        .and_then(|body| {
            body.lines()
                .find_map(|line| line.strip_prefix("push: "))
                .and_then(|rest| rest.split_whitespace().next())
                .map(str::to_string)
        })
        .unwrap_or_else(|| panic!("no `pool result {job}` with a `push: ` on the thread of {task}"))
}

/// Every file under `dir`, however deep (`dir` itself may be absent).
fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk_files(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// No file under `dir` holds the raw key: it is named by its key id only.
fn no_file_holds(dir: &Path, secret: &str, key_id: &str, context: &str) {
    for file in walk_files(dir) {
        let bytes = std::fs::read(&file).unwrap_or_default();
        assert!(
            !bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "{context}: {} holds the raw key of {key_id}",
            file.display()
        );
    }
}

/// POST one JSON-RPC message to the fake's MCP endpoint as `bearer`.
fn mcp(server: &ProjectServer, bearer: &str, message: Value) -> Value {
    let reply = project_server::http(
        &server.url(),
        "POST",
        "/v1/mcp",
        &[
            ("Content-Type", "application/json"),
            ("Authorization", &format!("Bearer {bearer}")),
        ],
        Some(&message.to_string()),
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    serde_json::from_str(&reply.body).expect("a JSON-RPC reply")
}

/// One `tools/call` of `name` with `arguments`, as `bearer`.
fn call(server: &ProjectServer, bearer: &str, id: u64, name: &str, arguments: Value) -> Value {
    let mut arguments = arguments;
    arguments["project"] = json!(server.project());
    mcp(
        server,
        bearer,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        }),
    )
}

#[test]
fn the_item_tools_read_and_record_against_a_seeded_item() {
    let server = ProjectServer::start();
    server.seed_item("t1", "Build it", "do the thing");
    let key = server.issue_api_key("alice");

    let listed = mcp(
        &server,
        &key,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
    );
    let names: Vec<String> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(names, ["echo", "show", "thread", "reply", "done"]);

    let show = call(&server, &key, 2, "show", json!({ "handle": "t1" }));
    assert_eq!(show["result"]["isError"], false, "{}", show);
    let text = show["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    for piece in [
        "handle: t1",
        "subject: Build it",
        "body: do the thing",
        "state: open",
    ] {
        assert!(text.contains(piece), "`{piece}` not in: {text}");
    }

    let thread = call(&server, &key, 3, "thread", json!({ "handle": "t1" }));
    let text = thread["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(text.contains("replies: none"), "{text}");

    let replied = call(
        &server,
        &key,
        4,
        "reply",
        json!({ "handle": "t1", "body": "did it" }),
    );
    assert_eq!(replied["result"]["isError"], false, "{}", replied);

    let thread = call(&server, &key, 5, "thread", json!({ "handle": "t1" }));
    let text = thread["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(text.contains("- alice: did it"), "{text}");

    let done = call(&server, &key, 6, "done", json!({ "handle": "t1" }));
    assert_eq!(done["result"]["isError"], false, "{}", done);
    let shown = call(&server, &key, 7, "show", json!({ "handle": "t1" }));
    let text = shown["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(text.contains("state: done"), "{text}");

    let unknown = call(&server, &key, 8, "show", json!({ "handle": "t9" }));
    assert_eq!(unknown["result"]["isError"], true, "{}", unknown);
    let text = unknown["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(text.contains("unknown item: t9"), "{text}");

    let reports = server.reports();
    assert_eq!(reports.len(), 2, "one reply and one done");
    assert_eq!(reports[0].kind, "reply");
    assert_eq!(reports[0].item, "t1");
    assert_eq!(reports[0].by, "alice", "the caller the bearer resolves to");
    assert_eq!(reports[0].body, "did it");
    assert_eq!(reports[1].kind, "done");
    assert_eq!(reports[1].item, "t1");
    assert_eq!(reports[1].by, "alice");
    assert_eq!(reports[1].body, "");
}

/// (a) (b) (c): the job reads its task through `show` and reports with
/// `reply` and `done` in order, as the member that minted the job's key;
/// the raw key sits in no file; the runner's own result is on the task too.
#[test]
fn a_bridge_job_reads_its_task_and_reports_as_the_member() {
    let server = ProjectServer::start();
    let rig = rig(&server, true);

    // The brief names the real mailspace task handle, so the fake must
    // hold that same item: create the task, seed the fake with its
    // handle, then dispatch.
    let task = handle(&rig, "bridge work");
    server.seed_item(&task, "bridge work", "do the thing end to end");

    let out = dispatch_task(&rig, &task, "feat/e2e");
    let said = ok(&out);
    let job = job_of(&said);

    let keys = server.keys();
    assert_eq!(keys.len(), 1, "exactly one mint: {said}");
    let key = &keys[0];
    assert_eq!(key.label, format!("pool-p-{job}"), "{said}");
    assert_eq!(key.created_by, "alice", "the dispatching member");
    assert!(
        said.contains(&format!("bridge key {}", key.key_id)),
        "{said}"
    );

    settle(&rig);

    let requests = server.mcp_requests();
    let calls: Vec<&project_server::McpSeen> = requests
        .iter()
        .filter(|seen| seen.body["method"].as_str() == Some("tools/call"))
        .collect();
    let names: Vec<&str> = calls
        .iter()
        .map(|seen| seen.body["params"]["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(names, ["show", "reply", "done"], "the tool calls, in order");
    for seen in &calls {
        assert_eq!(
            seen.bearer, key.secret,
            "every tool call bears the job's own key ({})",
            key.key_id
        );
    }
    for seen in &calls {
        assert_eq!(
            seen.body["params"]["arguments"]["project"].as_str(),
            Some(server.project()),
            "every tool call names the project: {}",
            seen.body
        );
    }
    for seen in &requests {
        assert_eq!(seen.path, "/v1/mcp", "the server's MCP endpoint");
    }
    let show = &calls[0].body;
    assert_eq!(
        show["params"]["arguments"]["handle"].as_str(),
        Some(task.as_str()),
        "the brief's handle: {}",
        show
    );
    let receipt = calls[1].body["params"]["arguments"]["body"]
        .as_str()
        .unwrap_or_default();
    assert!(
        receipt.contains("receipt:"),
        "the worker replied: {receipt}"
    );

    let reports = server.reports();
    assert_eq!(reports.len(), 2, "one reply and one done");
    assert_eq!(
        (
            reports[0].kind,
            reports[0].by.as_str(),
            reports[0].item.as_str()
        ),
        ("reply", "alice", task.as_str()),
        "the reply is recorded with the caller the key resolves to"
    );
    assert_eq!(reports[1].kind, "done");
    assert_eq!(reports[1].by, "alice");
    assert_eq!(reports[1].item, task, "done targets the same item");

    no_file_holds(&rig.lanes, &key.secret, &key.key_id, "the job's slot");
    no_file_holds(
        &rig.project.join(".vivi"),
        &key.secret,
        &key.key_id,
        "the mailspace",
    );
    for body in thread_bodies(&rig, &task) {
        assert!(
            !body.contains(&key.secret),
            "the task thread holds the raw key of {}",
            key.key_id
        );
    }

    let word = runner_result_word(&rig, &task, &job);
    assert!(
        word == "no_changes" || word == "committed",
        "the runner's own result is on the task: {word}"
    );
}

/// (d): a job whose worker never contacts the server still ends with the
/// runner's own result on the task, and the fake saw no report from it.
#[test]
fn a_silent_bridge_job_still_records_the_runner_s_result() {
    let server = ProjectServer::start();
    let rig = rig(&server, true);

    let (task, out) = dispatch(&rig, "nobridge: silent job", "feat/silent");
    let said = ok(&out);
    let job = job_of(&said);
    assert!(said.contains("bridge key"), "the pool still mints: {said}");

    settle(&rig);

    let word = runner_result_word(&rig, &task, &job);
    assert!(
        word == "no_changes" || word == "committed",
        "the runner's own result is on the task: {word}"
    );
    assert!(
        server.mcp_requests().is_empty(),
        "the worker never contacted the server"
    );
    assert!(
        server.reports().is_empty(),
        "the fake saw no report from the silent job"
    );
}

/// (e): `pool destroy --all --yes` leaves no live key with the pool's label
/// prefix, and a hand-minted key is not the pool's to revoke.
#[test]
fn destroying_the_pool_sweeps_its_labelled_keys_and_leaves_hand_made_ones() {
    let server = ProjectServer::start();
    let rig = rig(&server, true);
    mint(&rig, "hand-made");
    // A live key wearing the pool's own label shape: what a lost or
    // uncollected job leaves behind.
    mint(&rig, "pool-p-abc012def456");

    let (_, out) = dispatch(&rig, "bridge work", "feat/sweep");
    let job = job_of(&ok(&out));
    settle(&rig);

    ok(&destroy_all(&rig));

    let keys = server.keys();
    for key in &keys {
        if key.label.starts_with("pool-p-") {
            assert!(
                key.revoked,
                "no live key keeps the pool's prefix: {}",
                key.key_id
            );
        }
    }
    let job_key = keys
        .iter()
        .find(|key| key.label == format!("pool-p-{job}"))
        .expect("the job's key");
    assert!(job_key.revoked, "the collected job's key is revoked");
    let leftover = keys
        .iter()
        .find(|key| key.label == "pool-p-abc012def456")
        .expect("the leftover key");
    assert!(leftover.revoked, "the destroy swept the leftover");
    let hand = keys
        .iter()
        .find(|key| key.label == "hand-made")
        .expect("the hand-minted key");
    assert!(!hand.revoked, "a hand-minted key stays live");
}

/// (f): a dispatch on a pool with `[pools.p.bridge]` unset mints nothing.
#[test]
fn a_pool_without_a_bridge_mints_nothing_and_the_fake_sees_nothing() {
    let server = ProjectServer::start();
    let rig = rig(&server, false);

    let (_, out) = dispatch(&rig, "plain work", "feat/plain");
    let said = ok(&out);
    assert!(!said.contains("bridge key"), "{said}");

    settle(&rig);

    assert!(server.keys().is_empty(), "nothing was minted");
    assert!(server.mcp_requests().is_empty(), "no request reached /mcp");
}
