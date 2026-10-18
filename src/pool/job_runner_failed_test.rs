//! The job runner when the agent fails, run for real under bash against a
//! throwaway origin repository with a stand-in shell harness. Whatever the
//! agent left is committed on the attempt branch whatever its exit code, and a
//! failed attempt says so in its commit subject, in fetch mode (pushed) and in
//! worktree mode (committed, nothing pushed).

use std::path::{Path, PathBuf};
use std::process::Command;

use super::RUNNER;
use crate::pool::collect::JobResult;
use crate::pool::local_fixture::{Fixture, fixture, git};

const FAILED_SUBJECT: &str = "WIP (attempt failed): review";

struct Rig {
    f: Fixture,
    work: tempfile::TempDir,
    start: String,
}

impl Rig {
    fn new() -> Self {
        let f = fixture();
        let work = tempfile::tempdir().unwrap();
        git(
            work.path(),
            &["clone", "-q", &f.repo.display().to_string(), "repo"],
        );
        let start = git(&f.repo, &["rev-parse", "main"]);
        Self { f, work, start }
    }

    /// Point the checkout at a Git worktree of the fixture repository, the
    /// way a worktree slot is laid out.
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

    /// The subject of the tip of `pool/w` in `repo`.
    fn subject(repo: &Path) -> String {
        git(repo, &["log", "-1", "--format=%s", "pool/w"])
    }

    /// Run the runner as a written job with `harness` and a budget of
    /// `budget` seconds.
    fn run(&self, harness: &str, budget: u32, worktree: bool) -> JobResult {
        let remote = if worktree {
            String::new()
        } else {
            format!("file://{}", self.f.repo.display())
        };
        let mut params = format!(
            "REMOTE='{remote}'\nJOB='j1'\nTASK='t1'\nFROM='main'\nBRANCH='pool/w'\nDEPTH='1'\nBUDGET='{budget}'\nMODEL='standin'\nSLOT='s1'\nSUBJECT='review'\nCOMMIT_NAME='Bot'\nCOMMIT_EMAIL='bot@localhost'\nENV_NAMES=''\n"
        );
        if worktree {
            params += &format!("LOCAL_ACTION='create'\nLOCAL_START='{}'\n", self.start);
        }
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
        let text = std::fs::read_to_string(job.join("result"))
            .unwrap_or_else(|e| panic!("no result ({e}): {log}"));
        JobResult::parse(&text)
    }
}

#[test]
fn a_failed_agents_leftover_work_is_pushed_with_the_failure_subject() {
    let rig = Rig::new();
    let result = rig.run("echo partial >> a.txt; exit 3", 60, false);
    assert_eq!(result.push(), "pushed", "{:?}", result.fields);
    assert_eq!(result.get("agent_rc"), "3");
    assert_eq!(Rig::subject(&rig.f.repo), FAILED_SUBJECT);
    assert!(git(&rig.f.repo, &["show", "pool/w:a.txt"]).contains("partial"));
    let body = git(&rig.f.repo, &["log", "-1", "--format=%b", "pool/w"]);
    assert!(
        body.contains("exit=3"),
        "the trailer names the exit: {body}"
    );
}

#[test]
fn a_successful_agent_keeps_the_plain_subject() {
    let rig = Rig::new();
    let result = rig.run("echo done >> a.txt", 60, false);
    assert_eq!(result.push(), "pushed", "{:?}", result.fields);
    assert_eq!(result.get("agent_rc"), "0");
    assert_eq!(Rig::subject(&rig.f.repo), "review");
}

#[test]
fn a_timed_out_agents_leftover_work_is_pushed_with_the_failure_subject() {
    let rig = Rig::new();
    let result = rig.run("echo slow >> a.txt; sleep 20", 1, false);
    assert_eq!(result.push(), "pushed", "{:?}", result.fields);
    assert_eq!(result.get("agent_rc"), "124");
    assert_eq!(Rig::subject(&rig.f.repo), FAILED_SUBJECT);
}

#[test]
fn a_failed_agents_own_commit_gets_a_failure_marker_on_the_tip() {
    let rig = Rig::new();
    let harness = "echo x > own.txt; git add own.txt; git commit -q -m 'agent commit'; exit 1";
    let result = rig.run(harness, 60, false);
    assert_eq!(result.push(), "pushed", "{:?}", result.fields);
    assert_eq!(Rig::subject(&rig.f.repo), FAILED_SUBJECT);
    assert_eq!(
        git(&rig.f.repo, &["log", "-1", "--format=%s", "pool/w~1"]),
        "agent commit",
        "the agent's own commit is kept underneath"
    );
}

#[test]
fn a_failed_agent_that_left_nothing_makes_no_branch() {
    let rig = Rig::new();
    let result = rig.run("exit 2", 60, false);
    assert_eq!(result.push(), "no_changes", "{:?}", result.fields);
    assert_eq!(result.get("agent_rc"), "2");
    assert_eq!(git(&rig.f.repo, &["branch", "--list", "pool/w"]), "");
}

#[test]
fn in_worktree_mode_a_failed_agents_work_is_committed_with_the_failure_subject() {
    let rig = Rig::new().as_worktree();
    let result = rig.run("echo partial >> a.txt; exit 3", 60, true);
    assert_eq!(result.push(), "committed", "{:?}", result.fields);
    assert_eq!(result.get("agent_rc"), "3");
    assert_eq!(Rig::subject(&rig.repo()), FAILED_SUBJECT);
    assert_eq!(
        git(&rig.repo(), &["rev-parse", "--abbrev-ref", "HEAD"]),
        "HEAD",
        "the worktree goes back to a detached HEAD"
    );
}
