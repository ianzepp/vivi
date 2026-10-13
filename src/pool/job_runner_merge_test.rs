//! The job runner and the agent's own merge: the deepened history the extra
//! refs need before `git merge` accepts them, a merge the agent made pushed
//! with both parents, and the Git identity the agent's environment carries.
//! Run for real under bash against a throwaway origin repository: no slot,
//! no provider, a stand-in shell harness.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::RUNNER;
use super::with_runner_tests::tree_mentions_token;
use crate::pool::collect::JobResult;
use crate::pool::local_fixture::{Fixture, fixture, git};

struct Rig {
    f: Fixture,
    work: tempfile::TempDir,
    start: String,
}

impl Rig {
    /// A fixture where `A` (the job's FROM) and `B` (an extra ref holding
    /// `b.txt`) diverge -- the branch point is `ahead` commits behind A's
    /// tip -- and a shallow clone of A alone: the runner must fetch and,
    /// when the base is beyond its depth, deepen.
    fn new(ahead: usize) -> Self {
        let f = fixture();
        git(&f.repo, &["checkout", "-q", "-b", "B"]);
        std::fs::write(f.repo.join("b.txt"), "bee\n").unwrap();
        git(&f.repo, &["add", "-A"]);
        git(&f.repo, &["commit", "-q", "-m", "bee"]);
        git(&f.repo, &["checkout", "-q", "main"]);
        std::fs::write(f.repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        git(&f.repo, &["commit", "-qam", "third"]);
        for n in 0..ahead.saturating_sub(1) {
            let msg = format!("pad{n}");
            git(
                &f.repo,
                &["commit", "-q", "--allow-empty", "-m", msg.as_str()],
            );
        }
        git(&f.repo, &["branch", "A"]);
        let work = tempfile::tempdir().unwrap();
        git(
            work.path(),
            &[
                "clone",
                "-q",
                "--single-branch",
                "--branch",
                "A",
                "--depth",
                "1",
                &f.repo.display().to_string(),
                "repo",
            ],
        );
        let start = git(&work.path().join("repo"), &["rev-parse", "A"]);
        Self { f, work, start }
    }

    /// Point the checkout at a Git worktree of the fixture repository, the
    /// way a worktree slot is laid out: the runner then runs with
    /// LOCAL_ACTION=create and no remote.
    fn as_worktree(self) -> Self {
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

    fn a_sha(&self) -> String {
        git(&self.f.repo, &["rev-parse", "A"])
    }

    fn b_sha(&self) -> String {
        git(&self.f.repo, &["rev-parse", "B"])
    }

    /// Run the runner as a written fetch-mode job at the pool's `depth`.
    fn run_at(&self, harness: &str, with: &[&str], depth: u32) -> JobResult {
        self.run_as(harness, with, depth, false)
    }

    fn run(&self, harness: &str, with: &[&str]) -> JobResult {
        self.run_at(harness, with, 1)
    }

    /// As [`Rig::run`], in worktree mode.
    fn run_in_worktree(&self, harness: &str, with: &[&str]) -> JobResult {
        self.run_as(harness, with, 1, true)
    }

    fn stage(&self, params: &str, harness: &str, with: &[&str]) {
        let job = self.work.path().join("job");
        std::fs::create_dir_all(&job).unwrap();
        for (name, content) in [
            ("params.env", params),
            ("token", ""),
            ("harness.cmd", harness),
            ("brief.md", "the brief\n"),
            ("rules.md", "rules\n"),
        ] {
            std::fs::write(job.join(name), content).unwrap();
        }
        if !with.is_empty() {
            let list: String = with.iter().map(|e| format!("{e}\n")).collect();
            std::fs::write(job.join("with.list"), list).unwrap();
        }
    }

    fn run_as(&self, harness: &str, with: &[&str], depth: u32, worktree: bool) -> JobResult {
        let remote = if worktree {
            String::new()
        } else {
            format!("file://{}", self.f.repo.display())
        };
        let mut params = format!(
            "REMOTE='{remote}'\nJOB='j1'\nTASK='t1'\nFROM='A'\nBRANCH='pool/w'\nDEPTH='{depth}'\nBUDGET='60'\nMODEL='standin'\nSLOT='s1'\nSUBJECT='review'\nCOMMIT_NAME='Bot'\nCOMMIT_EMAIL='bot@localhost'\nENV_NAMES=''\n"
        );
        if worktree {
            params += &format!("LOCAL_ACTION='create'\nLOCAL_START='{}'\n", self.start);
        }
        self.stage(&params, harness, with);
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

    /// The run left no credential behind: no FETCH_HEAD (the URL in it holds
    /// the token), no token in .git/config, nothing in the agent's home.
    fn assert_no_token(&self) {
        assert!(
            !self.repo().join(".git/FETCH_HEAD").exists(),
            "FETCH_HEAD survived"
        );
        let config = std::fs::read_to_string(self.repo().join(".git/config")).unwrap();
        assert!(!config.contains("x-access-token"), "token in .git/config");
        assert!(
            !tree_mentions_token(&self.work.path().join("home")),
            "token under the agent's home"
        );
    }

    /// The commit the runner pushed: the pool branch on the origin.
    fn pushed(&self) -> String {
        git(&self.f.repo, &["rev-parse", "pool/w"])
    }
}

/// The parents of `sha` in the repository at `dir`.
fn parents(dir: &Path, sha: &str) -> Vec<String> {
    let line = git(dir, &["rev-list", "--parents", "-n", "1", sha]);
    line.split_whitespace()
        .skip(1)
        .map(str::to_string)
        .collect()
}

/// `got` is exactly the two commits `a` and `b`, in either order.
fn assert_both_parents(got: Vec<String>, a: &str, b: &str) {
    let mut want = vec![a.to_string(), b.to_string()];
    want.sort();
    assert_eq!(got, want, "expected exactly the two parents");
}

#[test]
fn an_agent_merge_of_an_extra_ref_is_pushed_with_both_parents() {
    let rig = Rig::new(1);
    let (a_sha, b_sha) = (rig.a_sha(), rig.b_sha());
    let env = rig.work.path().join("agent-env");
    let harness = format!(
        "env > {}; git merge --no-edit refs/remotes/origin/B",
        env.display()
    );
    let result = rig.run(&harness, &[&format!("B {b_sha}")]);
    assert_eq!(result.push(), "pushed", "{:?}", result.fields);
    assert_eq!(
        result.get("fetch_depth"),
        "2",
        "the base one commit behind both tips needed one doubling"
    );
    assert_eq!(
        result.get("with_1_merge_base"),
        git(&rig.repo(), &["rev-parse", "A~1"]),
        "the base is where B branched off"
    );
    let mut got = parents(&rig.repo(), &rig.pushed());
    got.sort();
    assert_both_parents(got, &a_sha, &b_sha);
    let dump = std::fs::read_to_string(&env).unwrap();
    assert!(
        !dump.contains("x-access-token"),
        "the agent's environment held a token"
    );
    rig.assert_no_token();
}

#[test]
fn a_merge_left_uncommitted_by_the_agent_is_published_with_both_parents() {
    let rig = Rig::new(1);
    let (a_sha, b_sha) = (rig.a_sha(), rig.b_sha());
    let b_line = format!("B {}", rig.b_sha());
    let result = rig.run("git merge --no-commit refs/remotes/origin/B", &[&b_line]);
    assert_eq!(result.push(), "pushed", "{:?}", result.fields);
    let pushed = rig.pushed();
    assert_eq!(
        git(&rig.repo(), &["log", "-1", "--format=%s", &pushed]),
        "review",
        "the runner committed the prepared merge"
    );
    let mut got = parents(&rig.repo(), &pushed);
    got.sort();
    assert_both_parents(got, &a_sha, &b_sha);
    rig.assert_no_token();
}

#[test]
fn a_merge_base_behind_the_shallow_boundary_is_reached_by_deepening() {
    let rig = Rig::new(120);
    let b_sha = rig.b_sha();
    let result = rig.run_at(
        "git merge --no-edit refs/remotes/origin/B",
        &[&format!("B {b_sha}")],
        50,
    );
    assert_eq!(result.push(), "pushed", "{:?}", result.fields);
    let depth: u32 = result
        .get("fetch_depth")
        .parse()
        .unwrap_or_else(|e| panic!("fetch_depth is not a number: {e}"));
    assert!(depth > 50, "fetch_depth {depth} was not deepened");
    assert_eq!(
        result.get("with_1_merge_base"),
        git(&rig.f.repo, &["rev-parse", "B~1"]),
        "the base is 120 commits behind A's tip"
    );
    let mut got = parents(&rig.repo(), &rig.pushed());
    got.sort();
    assert_both_parents(got, &rig.a_sha(), &b_sha);
    rig.assert_no_token();
}

#[test]
fn a_ref_without_common_history_is_recorded_and_the_job_goes_on() {
    let rig = Rig::new(1);
    git(&rig.f.repo, &["checkout", "-q", "--orphan", "C"]);
    std::fs::write(rig.f.repo.join("c.txt"), "sea\n").unwrap();
    git(&rig.f.repo, &["add", "-A"]);
    git(&rig.f.repo, &["commit", "-q", "-m", "sea"]);
    git(&rig.f.repo, &["checkout", "-q", "main"]);
    let c_sha = git(&rig.f.repo, &["rev-parse", "C"]);
    let result = rig.run("cat a.txt", &[&format!("C {c_sha}")]);
    assert_eq!(result.push(), "no_changes", "{:?}", result.fields);
    assert_eq!(result.get("with_1_local"), "refs/remotes/origin/C");
    assert_eq!(
        result.get("with_1_merge_base"),
        "none",
        "{:?}",
        result.fields
    );
    assert_eq!(
        result.get("fetch_depth"),
        "5000",
        "the deepening stopped at the cap"
    );
}

#[test]
fn a_fetch_mode_agent_that_commits_on_its_own_is_pushed() {
    let rig = Rig::new(1);
    let result = rig.run("git commit -q --allow-empty -m agent", &[]);
    assert_eq!(result.push(), "pushed", "{:?}", result.fields);
    assert!(result.fields.keys().all(|k| !k.starts_with("with_")));
    assert_eq!(
        git(&rig.repo(), &["log", "-1", "--format=%s", &rig.pushed()]),
        "agent",
        "the agent's own commit went out"
    );
    rig.assert_no_token();
}

#[test]
fn in_worktree_mode_an_agent_merge_of_a_local_branch_is_committed_with_both_parents() {
    let rig = Rig::new(1).as_worktree();
    let (a_sha, b_sha) = (rig.a_sha(), rig.b_sha());
    let result = rig.run_in_worktree("git merge --no-edit B", &[]);
    assert_eq!(result.push(), "committed", "{:?}", result.fields);
    let mut got = parents(&rig.f.repo, result.get("sha"));
    got.sort();
    assert_both_parents(got, &a_sha, &b_sha);
}
