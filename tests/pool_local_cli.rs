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

fn rig(extra_local: &str) -> Rig {
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
    git(&repo, &["branch", "feat/a"]);
    let root = project.to_str().unwrap();
    vivi(&["mailspace", "init", "--project", root]);
    for identity in ["mind", "hand"] {
        vivi(&["mailspace", "identity", "add", identity, "--project", root]);
    }
    let table = format!(
        "\n[pools.wt]\nprovider = \"local\"\nslots = 1\n\n[pools.wt.local]\nroot = \"{}\"\nworktree_of = \"{}\"\n{extra_local}{HARNESS}",
        base.join("lanes").display(),
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
        "from: main\nbranch: feat/new\nwith: nope/ref\n",
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
