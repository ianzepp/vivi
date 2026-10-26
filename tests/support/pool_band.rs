#![allow(dead_code)]
//! A portable local band-dispatch fixture for integration tests.
//!
//! Each rig is one throwaway tree: a mailspace project and a Git source
//! repository (with a bare `origin.git` beside it), plus a `local` worktree
//! pool whose `[pools.<pool>]` table the test composes from the band, cap,
//! harness and committer fragments below. Everything is portable: no real
//! provider credentials, no network services, no host configuration, and no
//! sleeping harness commands — the harness the fragments carry is a plain
//! `echo` that finishes immediately.
//!
//! The lifecycle goes through the real binary (`vivi pool create | dispatch
//! | wait`), and the durable pool events are inspected and injected through
//! the library (`Mailspace::pool_events` / `Mailspace::record_pool_note`),
//! never by guessing internal directory layouts — except `job_file`, which
//! reads the per-job files the slot writes.
//!
//! Mount it from any integration test file with an explicit path module:
//!
//! ```ignore
//! #[path = "support/pool_band.rs"]
//! mod pool_band;
//! ```
//!
//! The module is self-contained — its `vivi`, `git` and output helpers come
//! with it — so several test files can mount it side by side; the
//! `#![allow(dead_code)]` above covers the subset one target does not use.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Run the real `vivi` binary.
pub fn vivi(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

/// stdout + stderr of `output` as one string, the way a human reads them.
pub fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Assert `output` succeeded and return its stdout.
pub fn ok(output: &Output) -> String {
    assert!(output.status.success(), "{}", text(output));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// One rig: the throwaway tree and the one pool it configures.
pub struct BandRig {
    /// Keeps the tree alive until the test ends.
    pub dir: tempfile::TempDir,
    /// The project root holding `.vivi/mailspace.toml`.
    pub project: PathBuf,
    /// The source repository the pool's worktrees come from.
    pub repo: PathBuf,
    /// The pool's name, the key of its `[pools.<pool>]` table.
    pub pool: String,
}

impl BandRig {
    /// The project root, as a `--project` argument.
    pub fn root(&self) -> &str {
        self.project.to_str().expect("a utf-8 project path")
    }

    /// Where the pool keeps its slot directories.
    pub fn lanes(&self) -> PathBuf {
        self.project
            .parent()
            .expect("a parent for the project")
            .join("lanes")
    }
}

/// `git` in `dir`, panicking on failure.
fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap()
            .status
            .success(),
        "git {args:?}"
    );
}

/// The stdout of one `git` invocation in `dir`, panicking on failure.
pub fn git_out(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The throwaway tree: `project/` (a mailspace with `mind` and `hand`) and
/// `repo/` (a repository with `main`, one commit, a bare `origin.git` remote
/// and a resting branch `feat/a`).
pub fn scaffold() -> (tempfile::TempDir, PathBuf, PathBuf) {
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
    git(&repo, &["branch", "feat/a"]);
    let root = project.to_str().unwrap();
    ok(&vivi(&["mailspace", "init", "--project", root]));
    for identity in ["mind", "hand"] {
        ok(&vivi(&[
            "mailspace",
            "identity",
            "add",
            identity,
            "--project",
            root,
        ]));
    }
    (dir, project, repo)
}

/// The `[pools.<pool>.bands]` fragment: one `(band, model, reasoning)` triple
/// per band, each written as a one-entry list, so the first (only) choice
/// serves the band.
pub fn bands_table(pool: &str, choices: &[(&str, &str, &str)]) -> String {
    let mut table = format!("\n[pools.{pool}.bands]\n");
    for (band, model, reasoning) in choices {
        table.push_str(&format!(
            "{band} = [{{ model = \"{model}\", reasoning = \"{reasoning}\" }}]\n"
        ));
    }
    table
}

/// The `[pools.<pool>.caps]` fragment: one per-model concurrency cap per
/// pair. Empty `caps` writes nothing — the absent table is "no limits".
pub fn caps_table(pool: &str, caps: &[(&str, u32)]) -> String {
    if caps.is_empty() {
        return String::new();
    }
    let mut table = format!("\n[pools.{pool}.caps]\n");
    for (model, limit) in caps {
        table.push_str(&format!("{model} = {limit}\n"));
    }
    table
}

/// The `[pools.<pool>.harness]` fragment: `run` is the command template the
/// slot runs (placeholders `{model}`, `{reasoning}`, `{brief}`, `{rules}`;
/// its stdout is the job receipt) and `models` the slugs it may be given.
pub fn harness_table(pool: &str, name: &str, run: &str, models: &[&str]) -> String {
    let listed = models
        .iter()
        .map(|model| format!("\"{model}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!("\n[pools.{pool}.harness]\nname = \"{name}\"\nrun = \"{run}\"\nmodels = [{listed}]\n")
}

/// The `[pools.<pool>.committer]` fragment: a fixed portable bot identity.
pub fn committer_table(pool: &str) -> String {
    format!("\n[pools.{pool}.committer]\nname = \"bot\"\nemail = \"bot@example.com\"\n")
}

/// A rig on `scaffold()`: a `local` worktree pool named `pool` with `slots`
/// resting slots and `tables` (the fragments above) appended to its
/// `[pools.<pool>]` table in `.vivi/mailspace.toml`.
pub fn rig(pool: &str, slots: u32, tables: &str) -> BandRig {
    let (dir, project, repo) = scaffold();
    let table = format!(
        "\n[pools.{pool}]\nprovider = \"local\"\nslots = {slots}\n\n[pools.{pool}.local]\nroot = \"{}\"\nworktree_of = \"{}\"\n{tables}",
        project.parent().unwrap().join("lanes").display(),
        repo.display()
    );
    let config = project.join(".vivi/mailspace.toml");
    let mut written = std::fs::read_to_string(&config).unwrap();
    written.push_str(&table);
    std::fs::write(&config, written).unwrap();
    BandRig {
        dir,
        project,
        repo,
        pool: pool.to_string(),
    }
}

/// Add a local role through the CLI; `kind` and `model` are the optional
/// `--kind` / `--model` values (a band string such as `P1`, or a slug).
pub fn add_role(rig: &BandRig, name: &str, kind: Option<&str>, model: Option<&str>) {
    let mut args = vec!["role", "add", name];
    if let Some(kind) = kind {
        args.extend(["--kind", kind]);
    }
    if let Some(model) = model {
        args.extend(["--model", model]);
    }
    args.extend(["--project", rig.root()]);
    ok(&vivi(&args));
}

/// Give `role` the `charter` body through the CLI.
pub fn set_charter(rig: &BandRig, role: &str, charter: &str) {
    let file = rig.dir.path().join("charter.md");
    std::fs::write(&file, charter).unwrap();
    ok(&vivi(&[
        "role",
        "charter",
        "set",
        role,
        "--body-file",
        file.to_str().unwrap(),
        "--project",
        rig.root(),
    ]));
}

/// A whole-letter task body: pool refs live in the front matter, not in prose.
pub fn task_body(
    from: Option<&str>,
    branch: Option<&str>,
    band: Option<&str>,
    with: &[&str],
    prose: &str,
) -> String {
    let front = vivi::letter::work::TaskFront {
        from: from.map(str::to_string),
        branch: branch.map(str::to_string),
        band: band.map(str::to_string),
        with: with.iter().map(|w| (*w).to_string()).collect(),
        ..vivi::letter::work::TaskFront::default()
    };
    vivi::letter::render(&front, prose).expect("a rendered task letter")
}

/// Send a task from `mind` to `to` and return its handle.
pub fn send_task(rig: &BandRig, to: &str, subject: &str, body: &str) -> String {
    let sent = vivi(&[
        "task",
        "send",
        "--project",
        rig.root(),
        "--from",
        "mind",
        "--to",
        to,
        "--subject",
        subject,
        "--body",
        body,
    ]);
    assert!(sent.status.success(), "{}", text(&sent));
    String::from_utf8_lossy(&sent.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("created "))
        .and_then(|r| r.split_whitespace().nth(1))
        .map(str::to_string)
        .expect("a task handle")
}

/// `vivi pool create`: make the pool's home and seed its slots.
pub fn create(rig: &BandRig) {
    ok(&vivi(&[
        "pool",
        "create",
        "--pool",
        &rig.pool,
        "--project",
        rig.root(),
    ]));
}

/// `vivi pool dispatch` of `task` with `more` flags; returns (success, output).
pub fn dispatch(rig: &BandRig, task: &str, more: &[&str]) -> (bool, String) {
    let mut args = vec![
        "pool",
        "dispatch",
        "--pool",
        rig.pool.as_str(),
        task,
        "--project",
        rig.root(),
    ];
    args.extend_from_slice(more);
    let out = vivi(&args);
    (out.status.success(), text(&out))
}

/// `vivi pool wait`, which collects the settled job: blocks until `task` has
/// finished and returns the output.
pub fn collect(rig: &BandRig, task: &str) -> String {
    let out = vivi(&[
        "pool",
        "wait",
        "--pool",
        rig.pool.as_str(),
        "--timeout",
        "60",
        "--project",
        rig.root(),
    ]);
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(
        said.lines()
            .any(|l| l.starts_with(task) && l.contains("finished")),
        "{said}"
    );
    said
}

/// Open the rig's mailspace through the library.
pub fn mailspace(rig: &BandRig) -> vivi::mailspace::Mailspace {
    vivi::mailspace::Mailspace::discover(Some(&rig.project)).expect("the rig's mailspace")
}

/// The durable pool events, oldest first, read through the library.
pub fn events(rig: &BandRig) -> Vec<vivi::mailspace::PoolEventRow> {
    mailspace(rig).pool_events().expect("the pool event log")
}

/// The bodies of `handle`'s thread, oldest first, through the library. The
/// collected reply of a job carries its result and its receipt (the
/// harness stdout) under `--- receipt`.
pub fn thread_bodies(rig: &BandRig, handle: &str) -> Vec<String> {
    mailspace(rig)
        .thread(handle, false, usize::MAX, usize::MAX)
        .expect("the task thread")
        .into_iter()
        .map(|message| message.body)
        .collect()
}

/// Inject one durable pool event on `task` through the library: `record`
/// lands as the event's note, `text` as the reply receipt beside it.
pub fn inject_event(rig: &BandRig, task: &str, event_type: &str, record: &str, text: &str) {
    mailspace(rig)
        .record_pool_note(task, event_type, record, text)
        .expect("the injected pool event");
}

/// Whether `vivi pool status` reports the pool's slot free.
pub fn slot_free(rig: &BandRig) -> bool {
    text(&vivi(&[
        "pool",
        "status",
        "--pool",
        rig.pool.as_str(),
        "--project",
        rig.root(),
    ]))
    .contains("free")
}

/// The one `job/<name>` file under the pool's slot directories.
pub fn job_file(rig: &BandRig, name: &str) -> PathBuf {
    let mut found = Vec::new();
    let mut stack = vec![rig.lanes()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some(name)
                && path
                    .parent()
                    .is_some_and(|p| p.file_name().and_then(|n| n.to_str()) == Some("job"))
            {
                found.push(path);
            }
        }
    }
    assert_eq!(found.len(), 1, "one {name} under the slots: {found:?}");
    found.pop().unwrap()
}
