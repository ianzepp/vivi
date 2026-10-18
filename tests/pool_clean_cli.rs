//! `vivi pool clean`: the command exists, dry-run reports reclaimable build
//! output on a free local slot, and a live clean empties it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn vivi(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
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

fn scaffold() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let (project, repo, root) = (base.join("project"), base.join("repo"), base.join("pool"));
    std::fs::create_dir(&project).unwrap();
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "T"]);
    git(&repo, &["config", "user.email", "t@localhost"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    let origin = repo.parent().unwrap().join("origin.git");
    git(
        &repo,
        &["clone", "-q", "--bare", ".", origin.to_str().unwrap()],
    );
    git(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&repo, &["fetch", "-q", "origin"]);
    let proj = project.to_str().unwrap();
    vivi(&["mailspace", "init", "--project", proj]);
    for identity in ["mind", "hand"] {
        vivi(&["mailspace", "identity", "add", identity, "--project", proj]);
    }
    let config = project.join(".vivi/mailspace.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(&format!(
        "\n[pools.lanes]\nprovider = \"local\"\nslots = 1\nbuild_dir = \"target\"\n[pools.lanes.local]\nroot = \"{}\"\nworktree_of = \"{}\"\n[pools.lanes.harness]\nname = \"standin\"\nrun = \"true\"\nmodels = [\"standin\"]\n[pools.lanes.committer]\nname = \"bot\"\nemail = \"bot@example.com\"\n\n[pump]\nbranch = \"main\"\nhome = \"pump-home\"\n",
        root.display(),
        repo.display()
    ));
    std::fs::write(&config, text).unwrap();
    (dir, project, repo, root)
}

fn plant_slot(root: &Path, project_name: &str) -> PathBuf {
    let slug = project_name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    let dir = root
        .join(slug.trim_matches('-'))
        .join("lanes")
        .join("slot-1");
    std::fs::create_dir_all(dir.join("marks")).unwrap();
    std::fs::write(dir.join("marks/vivi_pool"), "lanes\n").unwrap();
    std::fs::write(
        dir.join("marks/vivi_pool_mailspace"),
        format!("{}\n", slug.trim_matches('-')),
    )
    .unwrap();
    std::fs::write(dir.join("marks/vivi_pool_slot"), "1\n").unwrap();
    std::fs::write(dir.join("marks/vivi_pool_state"), "ready\n").unwrap();
    std::fs::write(dir.join("marks/build_baseline"), "10\n").unwrap();
    std::fs::create_dir_all(dir.join("repo/target")).unwrap();
    std::fs::write(dir.join("repo/target/blob"), vec![0_u8; 80]).unwrap();
    std::fs::create_dir_all(dir.join("repo/src")).unwrap();
    std::fs::write(dir.join("repo/src/lib.rs"), "fn x() {}\n").unwrap();
    dir
}

#[test]
fn clean_help_names_dry_run() {
    let out = vivi(&["pool", "clean", "--help"]);
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("--dry-run"), "{said}");
    assert!(said.contains("reclaim"), "{said}");
    assert!(said.contains("--base"), "{said}");
}

#[test]
fn dry_run_reports_bytes_and_live_clean_empties_the_build_dir() {
    let (_tmp, project, _repo, root) = scaffold();
    let name = std::fs::read_to_string(project.join(".vivi/mailspace.toml"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("name = "))
        .map(|s| s.trim_matches('"').to_string())
        .unwrap();
    let slot = plant_slot(&root, &name);
    let proj = project.to_str().unwrap();
    let dry = vivi(&[
        "pool",
        "clean",
        "--project",
        proj,
        "--dry-run",
        "--porcelain",
    ]);
    assert!(dry.status.success(), "{}", text(&dry));
    let said = text(&dry);
    assert!(said.contains("would"), "{said}");
    assert!(said.contains("build"), "{said}");
    assert!(slot.join("repo/target/blob").is_file());
    let live = vivi(&["pool", "clean", "--project", proj, "--porcelain"]);
    assert!(live.status.success(), "{}", text(&live));
    assert!(text(&live).contains("cleaned"), "{}", text(&live));
    assert!(!slot.join("repo/target/blob").exists());
    assert_eq!(
        std::fs::read_to_string(slot.join("repo/src/lib.rs")).unwrap(),
        "fn x() {}\n"
    );
}

fn plant_worktree(root: &Path, project: &Path, repo: &Path, dirty: bool) -> PathBuf {
    let name = std::fs::read_to_string(project.join(".vivi/mailspace.toml"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("name = "))
        .map(|s| s.trim_matches('"').to_string())
        .unwrap();
    let slot = plant_slot(root, &name);
    std::fs::write(slot.join("marks/last_task"), "abcd1234\n").unwrap();
    std::fs::write(
        slot.join("marks/vivi_pool_base"),
        format!("{}\n", git_head(repo)),
    )
    .unwrap();
    std::fs::write(slot.join("worktree_of"), format!("{}\n", repo.display())).unwrap();
    std::fs::remove_dir_all(slot.join("repo")).unwrap();
    git(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            slot.join("repo").to_str().unwrap(),
        ],
    );
    if dirty {
        std::fs::write(slot.join("repo/a.txt"), "local edit\n").unwrap();
    }
    slot
}

fn git_head(repo: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write_merged_state(project: &Path) {
    let home = project.join("pump-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("state.json"),
        r#"{"units":{"u1":{"status":"merged","task":"abcd1234","branch":"unit/u1-a1","pool":"lanes"}}}"#,
    )
    .unwrap();
}

#[test]
fn dirty_worktree_is_reported_and_kept() {
    let (_tmp, project, repo, root) = scaffold();
    let slot = plant_worktree(&root, &project, &repo, true);
    write_merged_state(&project);
    let proj = project.to_str().unwrap();
    let out = vivi(&["pool", "clean", "--project", proj, "--porcelain"]);
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("skipped"), "{said}");
    assert!(said.contains("uncommitted"), "{said}");
    assert_eq!(
        std::fs::read_to_string(slot.join("repo/a.txt")).unwrap(),
        "local edit\n"
    );
}

#[test]
fn merged_into_the_merge_lane_recycles_a_clean_worktree() {
    let (_tmp, project, repo, root) = scaffold();
    let slot = plant_worktree(&root, &project, &repo, false);
    write_merged_state(&project);
    let proj = project.to_str().unwrap();
    let out = vivi(&["pool", "clean", "--project", proj, "--porcelain"]);
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("worktree"), "{said}");
    assert!(said.contains("cleaned"), "{said}");
    assert!(slot.join("repo/.git").exists());
    assert!(!slot.join("repo/target/blob").exists());
    assert_eq!(
        std::fs::read_to_string(slot.join("marks/base_ref"))
            .unwrap()
            .trim(),
        "main"
    );
}

fn advance_origin_main(repo: &Path, msg: &str) -> String {
    let origin = repo.parent().unwrap().join("origin.git");
    let tmp = repo.parent().unwrap().join(format!("push-{msg}"));
    git(
        &origin,
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            tmp.to_str().unwrap(),
        ],
    );
    git(&tmp, &["config", "user.name", "T"]);
    git(&tmp, &["config", "user.email", "t@localhost"]);
    std::fs::write(tmp.join("z.txt"), format!("{msg}\n")).unwrap();
    git(&tmp, &["add", "-A"]);
    git(&tmp, &["commit", "-q", "-m", msg]);
    git(&tmp, &["push", "-q", "origin", "HEAD:main"]);
    git_head(&tmp)
}

fn slot_id_of(root: &Path, slot: &Path) -> String {
    slot.strip_prefix(root)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[test]
fn reset_help_names_base() {
    let out = vivi(&["pool", "reset", "--help"]);
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("--base"), "{said}");
    assert!(said.contains("origin"), "{said}");
}

#[test]
fn reset_defaults_to_latest_origin_main_and_records_marks() {
    let (_tmp, project, repo, root) = scaffold();
    let slot = plant_worktree(&root, &project, &repo, false);
    let proj = project.to_str().unwrap();
    let id = slot_id_of(&root, &slot);
    let first = git_head(&repo);
    let out = vivi(&["pool", "reset", &id, "--project", proj, "--porcelain"]);
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("reset"), "{said}");
    assert!(said.contains("main"), "{said}");
    assert_eq!(
        std::fs::read_to_string(slot.join("marks/base_ref"))
            .unwrap()
            .trim(),
        "main"
    );
    assert_eq!(
        std::fs::read_to_string(slot.join("marks/base_sha"))
            .unwrap()
            .trim(),
        first
    );
    let status = vivi(&["pool", "status", "--project", proj, "--porcelain"]);
    assert!(status.status.success(), "{}", text(&status));
    assert!(text(&status).contains("main@"), "{}", text(&status));
    let later = advance_origin_main(&repo, "third");
    assert_ne!(later, first);
    let out = vivi(&["pool", "reset", &id, "--project", proj, "--porcelain"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains(&later), "{}", text(&out));
    assert_eq!(
        std::fs::read_to_string(slot.join("marks/base_sha"))
            .unwrap()
            .trim(),
        later
    );
    assert_eq!(git_head(&slot.join("repo")), later);
}

#[test]
fn reset_can_override_to_next() {
    let (_tmp, project, repo, root) = scaffold();
    git(&repo, &["branch", "next"]);
    git(&repo, &["push", "-q", "origin", "next"]);
    let slot = plant_worktree(&root, &project, &repo, false);
    let proj = project.to_str().unwrap();
    let id = slot_id_of(&root, &slot);
    let out = vivi(&[
        "pool",
        "reset",
        &id,
        "--base",
        "next",
        "--project",
        proj,
        "--porcelain",
    ]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("next"), "{}", text(&out));
    assert_eq!(
        std::fs::read_to_string(slot.join("marks/base_ref"))
            .unwrap()
            .trim(),
        "next"
    );
}

#[test]
fn reset_missing_origin_branch_fails_and_keeps_the_slot() {
    let (_tmp, project, repo, root) = scaffold();
    let slot = plant_worktree(&root, &project, &repo, false);
    let proj = project.to_str().unwrap();
    let id = slot_id_of(&root, &slot);
    let out = vivi(&[
        "pool",
        "reset",
        &id,
        "--base",
        "no-such-branch",
        "--project",
        proj,
        "--porcelain",
    ]);
    assert!(!out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(
        said.contains("no-such-branch") || said.contains("WARN"),
        "{said}"
    );
    assert!(slot.join("repo/a.txt").is_file());
    assert!(!slot.join("marks/base_ref").is_file());
}
