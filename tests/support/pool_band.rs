#![allow(dead_code)]
//! A portable band-dispatch fixture for CLI integration tests.
//!
//! [`Rig`] builds a throwaway Git repository (with a bare origin) and a
//! project mailspace beside it, then appends a one-slot `local` worktree pool
//! whose band, cap and harness tables the test passes in, so every test picks
//! its own portable model slugs and harness command without editing this
//! file. Dispatch and collection run the real `vivi` binary; durable pool
//! events are inspected and injected through the library's own APIs
//! (`Mailspace::pool_events`, `Mailspace::record_pool_note`, the task
//! thread). Nothing here needs credentials, a network service, host
//! configuration, or a sleeping harness. Include it from a test target with
//! `#[path = "support/pool_band.rs"] mod pool_band;`; the
//! `#![allow(dead_code)]` above covers the subset one target does not use.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use vivi::mailspace::{Mailspace, PoolEventRow};

/// The pool every rig configures: one slot, provider `local`, a worktree of
/// the rig's repository.
pub const POOL: &str = "wt";

/// Where a collected result's receipt section starts in the reply text.
const RECEIPT_MARK: &str = "--- receipt\n";

/// A project mailspace, a repository for its pool to worktree, and the pool
/// itself.
pub struct Rig {
    dir: tempfile::TempDir,
    /// The project root holding `.vivi/`.
    pub project: PathBuf,
    /// The repository the pool's slots worktree.
    pub repo: PathBuf,
}

impl Rig {
    /// A rig whose pool table carries exactly the band, cap and harness
    /// fragments given; pass `""` for a table the test does not want. The
    /// pool exists in config only until [`Rig::create_pool`] seeds its slot.
    pub fn new(bands: &str, caps: &str, harness: &str) -> Rig {
        let (dir, project, repo) = scaffold();
        let lanes = project.parent().unwrap().join("lanes");
        let table = format!(
            "\n[pools.{POOL}]\nprovider = \"local\"\nslots = 1\n\n\
             [pools.{POOL}.local]\nroot = \"{}\"\nworktree_of = \"{}\"\n{bands}{caps}{harness}",
            lanes.display(),
            repo.display()
        );
        let config = project.join(".vivi/mailspace.toml");
        let mut written = fs::read_to_string(&config).unwrap();
        written.push_str(&table);
        fs::write(&config, written).unwrap();
        Rig { dir, project, repo }
    }

    /// The project root as a command-line argument.
    pub fn root(&self) -> &str {
        self.project.to_str().unwrap()
    }

    /// A path beside the mailspace for the test's own files.
    pub fn scratch(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Add a role: its `kind` (e.g. `auditor`, `hand`) and, when it carries
    /// one, its band or model slug.
    pub fn add_role(&self, name: &str, kind: &str, model: Option<&str>) {
        let mut args = vec!["role", "add", name, "--kind", kind];
        if let Some(model) = model {
            args.extend(["--model", model]);
        }
        args.extend(["--project", self.root()]);
        ok(&vivi(&args));
    }

    /// Replace a role's standing charter body.
    pub fn set_charter(&self, role: &str, body: &str) {
        let file = self.scratch("charter.md");
        fs::write(&file, body).unwrap();
        ok(&vivi(&[
            "role",
            "charter",
            "set",
            role,
            "--body-file",
            file.to_str().unwrap(),
            "--project",
            self.root(),
        ]));
    }

    /// Create and seed the pool's one slot.
    pub fn create_pool(&self) {
        ok(&vivi(&[
            "pool",
            "create",
            "--pool",
            POOL,
            "--project",
            self.root(),
        ]));
    }
}

/// Run the real `vivi` binary.
pub fn vivi(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

/// A command's combined output.
pub fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Assert a command succeeded and return its combined output.
pub fn ok(output: &Output) -> String {
    let said = text(output);
    assert!(output.status.success(), "{said}");
    said
}

/// Run `git -C dir ...` in a clean environment, assert it succeeded, and
/// return its standard output.
pub fn git_text(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", text(&out));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A `[pools.wt.bands]` table: one line per band, each band a preference list
/// of `(model, reasoning)` choices. Model slugs are arbitrary portable words
/// (the harness's `models` allow-list must carry them).
pub fn bands_table(bands: &[(&str, &[(&str, &str)])]) -> String {
    let mut table = String::from("\n[pools.wt.bands]\n");
    for (band, choices) in bands {
        let list = choices
            .iter()
            .map(|(model, reasoning)| {
                format!("{{ model = \"{model}\", reasoning = \"{reasoning}\" }}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        table.push_str(&format!("{band} = [{list}]\n"));
    }
    table
}

/// A `[pools.wt.caps]` table: one per-model concurrency cap inside the pool.
pub fn caps_table(caps: &[(&str, u32)]) -> String {
    let mut table = String::from("\n[pools.wt.caps]\n");
    for (model, cap) in caps {
        table.push_str(&format!("\"{model}\" = {cap}\n"));
    }
    table
}

/// A harness table: the command template a slot runs instead of a model (the
/// `{model}`, `{reasoning}`, `{brief}` and `{rules}` placeholders render) and
/// the portable model slugs it offers. The command must hold no `"` and no
/// `\`.
pub fn harness_table(run: &str, models: &[&str]) -> String {
    let models = models
        .iter()
        .map(|model| format!("\"{model}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "\n[pools.{POOL}.harness]\nname = \"standin\"\nrun = \"{run}\"\nmodels = [{models}]\n\n\
         [pools.{POOL}.committer]\nname = \"bot\"\nemail = \"bot@example.com\"\n"
    )
}

/// A whole-letter task body: pool refs live in the front matter, not in
/// prose.
pub fn task_body(
    prose: &str,
    from: Option<&str>,
    branch: Option<&str>,
    band: Option<&str>,
    with: &[&str],
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

/// Send a task to `to` and return its handle.
pub fn send_task(rig: &Rig, to: &str, subject: &str, body: &str) -> String {
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
    ok(&sent)
        .lines()
        .find_map(|line| line.strip_prefix("created "))
        .and_then(|rest| rest.split_whitespace().nth(1))
        .map(str::to_string)
        .expect("a task handle")
}

/// Dispatch a task on the pool; `more` carries extra flags (`--from`,
/// `--model`, ...). Returns whether the dispatch was accepted.
pub fn dispatch(rig: &Rig, task: &str, more: &[&str]) -> (bool, String) {
    let mut args = vec![
        "pool",
        "dispatch",
        "--pool",
        POOL,
        task,
        "--project",
        rig.root(),
    ];
    args.extend_from_slice(more);
    let out = vivi(&args);
    (out.status.success(), text(&out))
}

/// Block on the pool until `task` settles, and return the output.
pub fn wait_settled(rig: &Rig, task: &str) -> String {
    let said = ok(&vivi(&[
        "pool",
        "wait",
        "--pool",
        POOL,
        "--timeout",
        "60",
        "--project",
        rig.root(),
    ]));
    assert!(
        said.lines()
            .any(|line| line.starts_with(task) && line.contains("finished")),
        "{said}"
    );
    said
}

/// One collect round over the pool's open dispatches.
pub fn collect_round(rig: &Rig) -> String {
    ok(&vivi(&[
        "pool",
        "reconcile",
        "--pool",
        POOL,
        "--project",
        rig.root(),
    ]))
}

/// Every durable pool event in the mailspace, oldest first.
pub fn pool_events(rig: &Rig) -> Vec<PoolEventRow> {
    Mailspace::open_root(rig.project.clone())
        .unwrap()
        .pool_events()
        .unwrap()
}

/// Inject one durable pool event and its reply note on a task, as the pool
/// machinery itself would write them.
pub fn inject_pool_event(rig: &Rig, task: &str, event_type: &str, record: &str, text: &str) {
    Mailspace::open_root(rig.project.clone())
        .unwrap()
        .record_pool_note(task, event_type, record, text)
        .unwrap();
}

/// The receipt section of a task's collected result: what the harness wrote
/// to its standard output, as recorded on the task's thread.
pub fn receipt_of(rig: &Rig, task: &str) -> String {
    let space = Mailspace::open_root(rig.project.clone()).unwrap();
    let thread = space.thread(task, false, usize::MAX, usize::MAX).unwrap();
    let collected = thread
        .iter()
        .rev()
        .find(|message| message.body.contains(RECEIPT_MARK))
        .unwrap_or_else(|| panic!("no collected result on {task}"));
    collected
        .body
        .split(RECEIPT_MARK)
        .nth(1)
        .unwrap()
        .split("\n--- ")
        .next()
        .unwrap()
        .to_string()
}

/// A throwaway repository (a commit on `main`, a spare branch `feat/a`, a
/// bare origin) and the project mailspace beside it.
fn scaffold() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let (project, repo) = (base.join("project"), base.join("repo"));
    fs::create_dir(&project).unwrap();
    fs::create_dir(&repo).unwrap();
    git_text(&repo, &["init", "-q", "-b", "main"]);
    git_text(&repo, &["config", "user.name", "T"]);
    git_text(&repo, &["config", "user.email", "t@localhost"]);
    fs::write(repo.join("a.txt"), "one\n").unwrap();
    git_text(&repo, &["add", "-A"]);
    git_text(&repo, &["commit", "-q", "-m", "first"]);
    let origin = base.join("origin.git");
    git_text(
        &repo,
        &["clone", "-q", "--bare", ".", origin.to_str().unwrap()],
    );
    git_text(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git_text(&repo, &["fetch", "-q", "origin"]);
    git_text(&repo, &["branch", "feat/a"]);
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
