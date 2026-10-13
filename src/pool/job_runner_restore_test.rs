//! The job runner's restore step, run for real under bash against a
//! throwaway origin repository: no slot, no provider, a stand-in shell
//! harness. Written jobs commit and push (or commit on the branch, in
//! worktree mode); the pool's `restore` list must keep a rewritten tracked
//! file out of the commit and name it in the result.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::RUNNER;
use crate::pool::collect::JobResult;
use crate::pool::local_fixture::{Fixture, fixture, git};

struct Rig {
    f: Fixture,
    work: tempfile::TempDir,
    start: String,
}

impl Rig {
    fn new() -> Self {
        Self::with_files(&[])
    }

    /// A rig whose repository also tracks `files`, committed on `main`.
    fn with_files(files: &[(&str, &str)]) -> Self {
        let f = fixture();
        for (path, content) in files {
            seed(&f.repo, path, content);
        }
        git(&f.repo, &["add", "-A"]);
        git(&f.repo, &["commit", "-q", "-m", "seed"]);
        let work = tempfile::tempdir().unwrap();
        git(
            work.path(),
            &["clone", "-q", &f.repo.display().to_string(), "repo"],
        );
        let start = git(&work.path().join("repo"), &["rev-parse", "main"]);
        Self { f, work, start }
    }

    /// Point the checkout at a Git worktree of the fixture repository, the
    /// way a worktree slot is laid out: the runner then runs with
    /// LOCAL_ACTION=create and no remote.
    fn as_worktree(mut self) -> Self {
        let repo = self.repo();
        std::fs::remove_dir_all(&repo).unwrap();
        git(
            &self.f.repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                repo.to_str().unwrap(),
                &self.start,
            ],
        );
        self
    }

    fn repo(&self) -> PathBuf {
        self.work.path().join("repo")
    }

    fn origin_refs(&self) -> String {
        git(&self.f.repo, &["for-each-ref"])
    }

    fn branches(&self) -> String {
        git(
            &self.repo(),
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/heads",
            ],
        )
    }

    /// The paths the pushed or committed branch changed against the start.
    fn changed_on_branch(&self, worktree: bool) -> Vec<String> {
        let repo = if worktree {
            self.repo()
        } else {
            self.f.repo.clone()
        };
        let out = git(
            &repo,
            &["diff", "--name-only", &format!("{}..pool/w", self.start)],
        );
        if out.is_empty() {
            Vec::new()
        } else {
            out.lines().map(str::to_string).collect()
        }
    }

    /// Run the runner as a written job with `restore` as the pool's list.
    fn run(&self, harness: &str, restore: &[&str]) -> JobResult {
        self.run_as(harness, restore, false, false)
    }

    /// As [`Rig::run`], read-only.
    fn run_read_only(&self, harness: &str, restore: &[&str]) -> JobResult {
        self.run_as(harness, restore, true, false)
    }

    /// As [`Rig::run`], in worktree mode.
    fn run_in_worktree(&self, harness: &str, restore: &[&str]) -> JobResult {
        self.run_as(harness, restore, false, true)
    }

    fn stage(&self, params: &str, harness: &str, restore: &[&str]) {
        let job = self.work.path().join("job");
        std::fs::create_dir_all(&job).unwrap();
        for (name, content) in [
            ("params.env", params.as_str()),
            ("token", ""),
            ("harness.cmd", harness),
            ("brief.md", "review it\n"),
            ("rules.md", "rules\n"),
        ] {
            std::fs::write(job.join(name), content).unwrap();
        }
        if !restore.is_empty() {
            let list: String = restore.iter().map(|p| format!("{p}\n")).collect();
            std::fs::write(job.join("restore.list"), list).unwrap();
        }
    }

    fn run_as(
        &self,
        harness: &str,
        restore: &[&str],
        read_only: bool,
        worktree: bool,
    ) -> JobResult {
        let remote = if worktree {
            String::new()
        } else {
            format!("file://{}", self.f.repo.display())
        };
        let mut params = format!(
            "REMOTE='{remote}'\nJOB='j1'\nTASK='t1'\nFROM='main'\nBRANCH='pool/w'\nDEPTH='1'\nBUDGET='60'\nMODEL='standin'\nSLOT='s1'\nSUBJECT='review'\nCOMMIT_NAME='Bot'\nCOMMIT_EMAIL='bot@localhost'\nENV_NAMES=''\n"
        );
        if worktree {
            params += &format!("LOCAL_ACTION='create'\nLOCAL_START='{}'\n", self.start);
        }
        if read_only {
            params += "READ_ONLY='1'\n";
        }
        self.stage(&params, harness, restore);
        let home = self.work.path().join("home");
        let shims = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/pool/local-bin");
        let path = format!("{}:{}", shims.display(), std::env::var("PATH").unwrap());
        let out = Command::new("bash")
            .arg("-c")
            .arg(RUNNER)
            .env("WORK", self.work.path())
            .env("UNPRIVILEGED", "1")
            .env("BUILD_DIR", "build")
            .env("BUILD_PERSISTS", "1")
            .env("HOME", &home)
            .env("PATH", path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&out.stdout).into_owned();
        let text = std::fs::read_to_string(self.work.path().join("job/result"))
            .unwrap_or_else(|e| panic!("no result ({e}): {log}"));
        JobResult::parse(&text)
    }
}

/// Write one tracked file into `repo`, making its directory.
fn seed(repo: &Path, path: &str, content: &str) {
    let file = repo.join(path);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(file, content).unwrap();
}

const LOCK: &str = "runtime/llvm/Cargo.lock";
const SEEDED_LOCK: &str = "lock v1\n";
const LIB: &str = "src/lib.rs";
const REWRITES: &str = "echo regenerated > runtime/llvm/Cargo.lock; echo change >> src/lib.rs";

#[test]
fn a_restored_path_is_left_out_of_the_pushed_commit_and_named_in_the_result() {
    let rig = Rig::with_files(&[(LOCK, SEEDED_LOCK), (LIB, "lib v1\n")]);
    let result = rig.run(REWRITES, &[LOCK]);
    assert_eq!(result.push(), "pushed", "{:?}", result.fields);
    assert_eq!(result.get("restored_count"), "1");
    assert_eq!(result.restored_paths(), [LOCK]);
    assert_eq!(
        git(&rig.f.repo, &["show", &format!("pool/w:{LOCK}")]),
        SEEDED_LOCK.trim(),
        "the pushed commit holds the start content, not the rewrite"
    );
    assert_eq!(rig.changed_on_branch(false), [LIB]);
    assert_eq!(
        std::fs::read_to_string(rig.repo().join(LOCK)).unwrap(),
        SEEDED_LOCK,
        "the file is back at the start content"
    );
}

#[test]
fn without_the_setting_the_same_job_commits_both_files() {
    let rig = Rig::with_files(&[(LOCK, SEEDED_LOCK), (LIB, "lib v1\n")]);
    let result = rig.run(REWRITES, &[]);
    assert_eq!(result.push(), "pushed", "{:?}", result.fields);
    assert!(result.fields.keys().all(|k| !k.starts_with("restored_")));
    assert_eq!(rig.changed_on_branch(false), [LOCK, LIB]);
}

#[test]
fn an_unchanged_configured_path_produces_no_restored_lines() {
    let rig = Rig::with_files(&[(LOCK, SEEDED_LOCK), (LIB, "lib v1\n")]);
    let result = rig.run("echo change >> src/lib.rs", &[LOCK]);
    assert_eq!(result.push(), "pushed", "{:?}", result.fields);
    assert!(result.fields.keys().all(|k| !k.starts_with("restored_")));
    assert_eq!(rig.changed_on_branch(false), [LIB]);
}

#[test]
fn a_path_not_tracked_at_the_start_is_ignored_and_not_deleted() {
    let rig = Rig::with_files(&[(LIB, "lib v1\n")]);
    let harness = "echo new > ghost.txt; echo oops > missing.txt";
    let result = rig.run(harness, &["ghost.txt", "missing.txt"]);
    assert_eq!(result.push(), "pushed", "{:?}", result.fields);
    assert!(result.fields.keys().all(|k| !k.starts_with("restored_")));
    assert_eq!(
        git(&rig.f.repo, &["show", "pool/w:ghost.txt"]),
        "new",
        "the agent's untracked file is not restored away"
    );
    assert!(rig.repo().join("ghost.txt").is_file(), "not deleted");
}

#[test]
fn in_worktree_mode_the_restored_path_is_left_out_of_the_branch() {
    let rig = Rig::with_files(&[(LOCK, SEEDED_LOCK), (LIB, "lib v1\n")]).as_worktree();
    let result = rig.run_in_worktree(REWRITES, &[LOCK]);
    assert_eq!(result.push(), "committed", "{:?}", result.fields);
    assert_eq!(result.get("branch"), "pool/w");
    assert_eq!(result.get("restored_count"), "1");
    assert_eq!(result.restored_paths(), [LOCK]);
    assert_eq!(
        git(&rig.repo(), &["show", &format!("pool/w:{LOCK}")]),
        SEEDED_LOCK.trim(),
        "the committed branch holds the start content, not the rewrite"
    );
    assert_eq!(rig.changed_on_branch(true), [LIB]);
    assert_eq!(
        git(&rig.repo(), &["rev-parse", "--abbrev-ref", "HEAD"]),
        "HEAD",
        "the worktree goes back to a detached HEAD"
    );
}

#[test]
fn a_path_holding_a_space_is_restored_and_listed_whole() {
    let spaced = "weird dir/lock file.txt";
    let rig = Rig::with_files(&[(spaced, SEEDED_LOCK), (LIB, "lib v1\n")]);
    let harness = "echo regenerated > 'weird dir/lock file.txt'; echo change >> src/lib.rs";
    let result = rig.run(harness, &[spaced]);
    assert_eq!(result.push(), "pushed", "{:?}", result.fields);
    assert_eq!(result.get("restored_count"), "1");
    assert_eq!(result.restored_paths(), [spaced]);
    assert_eq!(
        git(&rig.f.repo, &["show", &format!("pool/w:{spaced}")]),
        SEEDED_LOCK.trim()
    );
    assert_eq!(rig.changed_on_branch(false), [LIB]);
}

#[test]
fn a_read_only_job_with_the_setting_is_unaffected() {
    let rig = Rig::with_files(&[(LOCK, SEEDED_LOCK), (LIB, "lib v1\n")]);
    let (branches, refs) = (rig.branches(), rig.origin_refs());
    let result = rig.run_read_only("echo regenerated > runtime/llvm/Cargo.lock", &[LOCK]);
    assert_eq!(result.push(), "read_only_discarded");
    assert_eq!(result.discarded_paths(), [LOCK]);
    assert!(result.fields.keys().all(|k| !k.starts_with("restored_")));
    assert_eq!(rig.branches(), branches, "no branch made or moved");
    assert_eq!(rig.origin_refs(), refs, "nothing was published");
    assert_eq!(
        std::fs::read_to_string(rig.repo().join(LOCK)).unwrap(),
        SEEDED_LOCK,
        "the read-only restore puts the file back as before"
    );
}
