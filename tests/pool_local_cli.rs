//! The `local` host through the CLI: the pool-table rules and the branch
//! refusals of a worktree pool, which happen before any slot is claimed or
//! any process started. A throwaway Git repository; nothing is launched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

struct Rig {
    _dir: tempfile::TempDir,
    project: PathBuf,
    repo: PathBuf,
}

const HARNESS: &str = r#"
[pools.wt.harness]
name = "standin"
run = "echo {model} @{brief}"
models = ["standin"]
env = ["VIVI_TEST_SURELY_UNSET_NAME"]

[pools.wt.committer]
name = "bot"
email = "bot@example.com"
"#;

fn scaffold() -> (tempfile::TempDir, PathBuf, PathBuf) {
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
    vivi(&["mailspace", "init", "--project", root]);
    for identity in ["mind", "hand"] {
        vivi(&["mailspace", "identity", "add", identity, "--project", root]);
    }
    (dir, project, repo)
}

fn rig(extra_local: &str) -> Rig {
    let (dir, project, repo) = scaffold();
    let table = format!(
        "\n[pools.wt]\nprovider = \"local\"\nslots = 1\n\n[pools.wt.local]\nroot = \"{}\"\nworktree_of = \"{}\"\n{extra_local}{HARNESS}",
        project.parent().unwrap().join("lanes").display(),
        repo.display()
    );
    let config = project.join(".vivi/mailspace.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(&table);
    std::fs::write(&config, text).unwrap();
    Rig {
        _dir: dir,
        project,
        repo,
    }
}

/// A whole-letter task body: pool refs live in the front matter, not in prose.
fn pool_body(
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

fn dispatch(rig: &Rig, from: &str, branch: &str, more: &[&str]) -> (bool, String) {
    let task = handle(rig, branch);
    let mut args = vec![
        "pool",
        "dispatch",
        "--pool",
        "wt",
        &task,
        "--model",
        "standin",
        "--from",
        from,
        "--branch",
        branch,
        "--project",
        rig.project.to_str().unwrap(),
    ];
    args.extend_from_slice(more);
    let out = vivi(&args);
    (out.status.success(), text(&out))
}

fn nothing_was_made(rig: &Rig) {
    let lanes = rig.project.parent().unwrap().join("lanes");
    assert!(
        !lanes.exists(),
        "a refused dispatch must not create the pool root"
    );
    let list = Command::new("git")
        .arg("-C")
        .arg(&rig.repo)
        .args(["worktree", "list"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&list.stdout).lines().count(), 1);
}

#[test]
fn an_existing_branch_with_another_from_is_refused_and_nothing_moves() {
    let rig = rig("");
    let (ok, said) = dispatch(&rig, "main", "feat/a", &[]);
    assert!(
        !ok && said.contains("already exists") && said.contains("refusing"),
        "{said}"
    );
    nothing_was_made(&rig);
}

#[test]
fn a_ready_timeout_on_a_pool_with_resting_slots_is_a_usage_error_and_nothing_moves() {
    let rig = rig("");
    let (ok, said) = dispatch(&rig, "main", "feat/new", &["--ready-timeout", "60"]);
    assert!(
        !ok && said.contains("--ready-timeout") && said.contains("per dispatch"),
        "{said}"
    );
    nothing_was_made(&rig);
}

#[test]
fn the_default_branch_and_the_primary_checkouts_branch_are_refused() {
    let rig = rig("");
    let (ok, said) = dispatch(&rig, "main", "main", &[]);
    assert!(!ok && said.contains("main"), "{said}");
    let (ok, said) = dispatch(&rig, "feat/a", "main", &[]);
    assert!(!ok, "{said}");
    nothing_was_made(&rig);
}

#[test]
fn a_branch_checked_out_elsewhere_is_refused_naming_the_path() {
    let rig = rig("");
    let elsewhere = rig.repo.parent().unwrap().join("elsewhere");
    git(
        &rig.repo,
        &[
            "worktree",
            "add",
            "-q",
            elsewhere.to_str().unwrap(),
            "feat/a",
        ],
    );
    let (ok, said) = dispatch(&rig, "feat/a", "feat/a", &[]);
    assert!(!ok && said.contains(elsewhere.to_str().unwrap()), "{said}");
    assert!(said.contains("one worktree at a time"), "{said}");
}

#[test]
fn a_continue_of_a_branch_that_does_not_exist_is_refused() {
    let rig = rig("");
    let (ok, said) = dispatch(&rig, "feat/none", "feat/none", &[]);
    assert!(!ok && said.contains("cannot be continued"), "{said}");
}

#[test]
fn an_unknown_with_ref_is_refused_before_a_slot_is_claimed() {
    let rig = rig("");
    let (ok, said) = dispatch(&rig, "main", "feat/new", &["--with", "nope/ref"]);
    assert!(!ok && said.contains("nope/ref"), "{said}");
    assert!(
        said.contains(rig.repo.to_str().unwrap()),
        "the refusal names the repository: {said}"
    );
    nothing_was_made(&rig);
}

#[test]
fn an_unknown_with_ref_in_the_body_is_refused_before_a_slot_is_claimed() {
    let rig = rig("");
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
        "body names a ref",
        "--body",
        &pool_body("", Some("main"), Some("feat/new"), None, &["nope/ref"]),
    ]);
    assert!(sent.status.success());
    let task = String::from_utf8_lossy(&sent.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("created hand "))
        .and_then(|r| r.split_whitespace().next())
        .map(str::to_string)
        .expect("a task handle");
    let out = vivi(&[
        "pool",
        "dispatch",
        "--pool",
        "wt",
        &task,
        "--model",
        "standin",
        "--project",
        rig.project.to_str().unwrap(),
    ]);
    let said = text(&out);
    assert!(!out.status.success() && said.contains("nope/ref"), "{said}");
    nothing_was_made(&rig);
}

#[test]
fn a_pool_table_with_both_worktree_of_and_repo_is_an_error_before_anything_runs() {
    let rig = rig("");
    let config = rig.project.join(".vivi/mailspace.toml");
    let table = std::fs::read_to_string(&config).unwrap();
    let both = table.replace("[pools.wt]\n", "[pools.wt]\nrepo = \"acme/widgets\"\n")
        + "\n[pools.wt.github]\napp_id = 1\nprivate_key = \"/nonexistent.pem\"\n";
    std::fs::write(&config, both).unwrap();
    let out = vivi(&[
        "pool",
        "status",
        "--pool",
        "wt",
        "--project",
        rig.project.to_str().unwrap(),
    ]);
    let said = text(&out);
    assert!(
        !out.status.success() && said.contains("worktree_of and repo cannot both be set"),
        "{said}"
    );
}

#[test]
fn the_doctor_reports_the_source_and_unset_harness_names_without_creating_the_root() {
    let rig = rig("");
    let out = vivi(&[
        "pool",
        "doctor",
        "--pool",
        "wt",
        "--project",
        rig.project.to_str().unwrap(),
    ]);
    let said = text(&out);
    assert!(
        said.contains("worktree mode") && said.contains("1 worktree(s)"),
        "{said}"
    );
    assert!(said.contains("VIVI_TEST_SURELY_UNSET_NAME"), "{said}");
    assert!(!rig.project.parent().unwrap().join("lanes").exists());
}

/// The pool table of the role dispatch tests: a band table (P1, P2) and a
/// harness whose run text needs the `{reasoning}` a band picks.
const ROLE_TABLE: &str = r#"
[pools.wt.bands]
P1 = [{ model = "standin", reasoning = "mid" }]
P2 = [{ model = "writer", reasoning = "xhigh" }]

[pools.wt.harness]
name = "standin"
run = "echo work >> standin.txt; echo model={model} reasoning={reasoning} @{brief}"
models = ["standin", "writer"]

[pools.wt.committer]
name = "bot"
email = "bot@example.com"
"#;

const CHARTER: &str =
    "Audit faithfully.\nReport every finding.\nReply inside a v20 envelope:\n+++\n";

/// A rig with roles: `auditor` (kind auditor, band P1, a charter) and
/// `helper` (kind hand, no band), a bands table and a `{reasoning}` harness,
/// and one resting slot, so a refusal can be shown to leave it free.
fn role_rig() -> Rig {
    let (dir, project, repo) = scaffold();
    let root = project.to_str().unwrap();
    let table = format!(
        "\n[pools.wt]\nprovider = \"local\"\nslots = 1\n\n[pools.wt.local]\nroot = \"{}\"\nworktree_of = \"{}\"\n{ROLE_TABLE}",
        project.parent().unwrap().join("lanes").display(),
        repo.display()
    );
    let config = project.join(".vivi/mailspace.toml");
    let mut written = std::fs::read_to_string(&config).unwrap();
    written.push_str(&table);
    std::fs::write(&config, written).unwrap();
    vivi(&[
        "role",
        "add",
        "auditor",
        "--kind",
        "auditor",
        "--model",
        "P1",
        "--project",
        root,
    ]);
    vivi(&["role", "add", "helper", "--kind", "hand", "--project", root]);
    let charter = dir.path().join("charter.md");
    std::fs::write(&charter, CHARTER).unwrap();
    vivi(&[
        "role",
        "charter",
        "set",
        "auditor",
        "--body-file",
        charter.to_str().unwrap(),
        "--project",
        root,
    ]);
    let made = vivi(&["pool", "create", "--pool", "wt", "--project", root]);
    assert!(made.status.success(), "{}", text(&made));
    Rig {
        _dir: dir,
        project,
        repo,
    }
}

fn git_out(dir: &Path, args: &[&str]) -> String {
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

/// Send a task to `to` and return its handle.
fn send_to(rig: &Rig, to: &str, subject: &str, body: &str) -> String {
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

/// Dispatch `task` on the role pool with no model and no read-only flag
/// unless `more` says otherwise; returns (success, output).
fn dispatch_to(rig: &Rig, task: &str, more: &[&str]) -> (bool, String) {
    let mut args = vec![
        "pool",
        "dispatch",
        "--pool",
        "wt",
        task,
        "--project",
        rig.project.to_str().unwrap(),
    ];
    args.extend_from_slice(more);
    let out = vivi(&args);
    (out.status.success(), text(&out))
}

/// Block on the task's dispatch and return the output (the settled line).
fn wait(rig: &Rig, task: &str) -> String {
    let out = vivi(&[
        "pool",
        "wait",
        "--pool",
        "wt",
        "--timeout",
        "60",
        "--project",
        rig.project.to_str().unwrap(),
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

fn slot_free(rig: &Rig) -> bool {
    let out = vivi(&[
        "pool",
        "status",
        "--pool",
        "wt",
        "--project",
        rig.project.to_str().unwrap(),
    ]);
    text(&out).contains("free")
}

/// The one `job/<name>` file under the pool's slot directories.
fn job_file(rig: &Rig, name: &str) -> PathBuf {
    let mut found = Vec::new();
    let mut stack = vec![rig.project.parent().unwrap().join("lanes")];
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

#[test]
fn an_auditor_role_task_runs_read_only_on_the_band_model_with_the_charter_above_the_rules() {
    let rig = role_rig();
    let task = send_to(
        &rig,
        "auditor",
        "Audit it",
        &pool_body("work", Some("main"), None, None, &[]),
    );
    let (ok, said) = dispatch_to(&rig, &task, &["--from", "main"]);
    assert!(ok, "{said}");
    assert!(said.contains("model standin"), "{said}");
    assert!(said.contains("read-only"), "{said}");
    assert!(
        said.contains("role auditor band P1 -> standin mid read-only (role kind auditor)"),
        "{said}"
    );
    let waited = wait(&rig, &task);
    assert!(waited.contains("read_only_"), "{waited}");
    // Nothing was committed and no branch appeared.
    let heads = git_out(
        &rig.repo,
        &["for-each-ref", "--format=%(refname)", "refs/heads"],
    );
    assert_eq!(heads.lines().count(), 2, "{heads}");
    // The standing prompt: the charter above the pool rules.
    let rules = std::fs::read_to_string(job_file(&rig, "rules.md")).unwrap();
    assert!(rules.contains("Audit faithfully."), "{rules}");
    assert!(
        rules.contains("Standing rules for the remote worker"),
        "{rules}"
    );
    assert!(
        rules.contains("# Standing charter (role auditor)"),
        "{rules}"
    );
    assert!(rules.contains("# Pool rules"), "{rules}");
}

#[test]
fn a_hand_role_task_with_a_band_line_writes_on_the_band_model() {
    let rig = role_rig();
    let task = send_to(
        &rig,
        "helper",
        "Write it",
        &pool_body("work", Some("main"), Some("feat/b"), Some("P2"), &[]),
    );
    let (ok, said) = dispatch_to(&rig, &task, &[]);
    assert!(ok, "{said}");
    assert!(said.contains("model writer"), "{said}");
    assert!(said.contains("branch feat/b"), "{said}");
    assert!(
        said.contains("role helper band P2 -> writer xhigh"),
        "{said}"
    );
    let waited = wait(&rig, &task);
    assert!(waited.contains("committed feat/b"), "{waited}");
    let heads = git_out(
        &rig.repo,
        &["for-each-ref", "--format=%(refname)", "refs/heads"],
    );
    assert!(heads.contains("refs/heads/feat/b"), "{heads}");
}

#[test]
fn a_task_with_neither_band_nor_model_is_refused_and_the_slot_stays_free() {
    let rig = role_rig();
    let task = send_to(
        &rig,
        "helper",
        "No band",
        &pool_body("work", Some("main"), Some("feat/c"), None, &[]),
    );
    let (ok, said) = dispatch_to(&rig, &task, &[]);
    assert!(!ok && said.contains("no model:"), "{said}");
    assert!(slot_free(&rig), "the slot was consumed by a refusal");
}

#[test]
fn a_pool_without_the_band_refuses_naming_it_and_the_slot_stays_free() {
    let rig = role_rig();
    let task = send_to(
        &rig,
        "helper",
        "Bad band",
        &pool_body("work", Some("main"), Some("feat/d"), Some("P5"), &[]),
    );
    let (ok, said) = dispatch_to(&rig, &task, &[]);
    assert!(!ok && said.contains("band P5"), "{said}");
    assert!(slot_free(&rig), "the slot was consumed by a refusal");
}

#[test]
fn a_model_flag_still_works_on_a_role_task_and_read_only_still_forces() {
    let rig = role_rig();
    let banded = vivi(&[
        "role",
        "set",
        "helper",
        "--model",
        "P1",
        "--project",
        rig.project.to_str().unwrap(),
    ]);
    assert!(banded.status.success(), "{}", text(&banded));
    // `--model` still wins over the role's band and the job writes.
    let write = send_to(
        &rig,
        "helper",
        "Flag write",
        &pool_body("work", Some("main"), Some("feat/e"), None, &[]),
    );
    let (ok, said) = dispatch_to(&rig, &write, &["--model", "standin"]);
    assert!(ok && said.contains("model standin"), "{said}");
    assert!(
        said.contains("role helper band P1 -> standin mid"),
        "{said}"
    );
    let waited = wait(&rig, &write);
    assert!(waited.contains("committed feat/e"), "{waited}");
    // `--read-only` still forces a read-only job on a hand-kind role.
    let ro = send_to(
        &rig,
        "helper",
        "Flag ro",
        &pool_body("work", Some("main"), None, None, &[]),
    );
    let (ok, said) = dispatch_to(
        &rig,
        &ro,
        &["--model", "standin", "--read-only", "--from", "main"],
    );
    assert!(ok && said.contains("read-only"), "{said}");
    let waited = wait(&rig, &ro);
    assert!(waited.contains("read_only_"), "{waited}");
    // The read-only job added no branch: only main, feat/a and feat/e exist.
    let heads = git_out(
        &rig.repo,
        &["for-each-ref", "--format=%(refname)", "refs/heads"],
    );
    assert_eq!(heads.lines().count(), 3, "{heads}");
}

fn porcelain_dispatch(rig: &Rig, subject: &str, more: &[&str]) -> Output {
    let task = handle(rig, subject);
    let mut args = vec![
        "pool",
        "dispatch",
        "--porcelain",
        "--pool",
        "wt",
        &task,
        "--model",
        "standin",
        "--project",
        rig.project.to_str().unwrap(),
    ];
    args.extend_from_slice(more);
    vivi(&args)
}

#[test]
fn a_porcelain_dispatch_with_no_from_prints_a_refused_line() {
    let rig = rig("");
    let out = porcelain_dispatch(&rig, "nofrom", &["--read-only"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let reason = vivi::pool::porcelain::refused_reason(&stdout);
    assert!(
        !out.status.success() && reason.is_some_and(|r| r.contains("no from")),
        "{stdout}"
    );
    nothing_was_made(&rig);
}

#[test]
fn a_porcelain_dispatch_that_fails_after_planning_prints_no_refused_line() {
    let rig = rig("");
    let out = porcelain_dispatch(&rig, "feat/a", &["--from", "main", "--branch", "feat/a"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("already exists"), "{}", text(&out));
    assert_eq!(vivi::pool::porcelain::refused_reason(&stdout), None);
}
