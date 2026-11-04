//! The job runner's extra refs (the pool's `with` list), run for real under
//! bash against a throwaway origin repository: no slot, no provider, a
//! stand-in shell harness. The pool names refs the agent must be able to see;
//! fetch mode fetches each into a local ref and leaves no credential behind,
//! worktree mode finds them already in the shared repository.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::RUNNER;
use crate::pool::collect::JobResult;
use crate::pool::local_fixture::{Fixture, fixture, git};

struct Rig {
    f: Fixture,
    work: tempfile::TempDir,
    start: String,
    /// The text of `extra-targets.env`, when the job carries extra checkouts.
    extras: Option<String>,
}

impl Rig {
    /// A fixture with branches `A` (the job's FROM) and `B` (an extra ref
    /// holding `b.txt`), and a clone that holds only A: B must be fetched.
    fn new() -> Self {
        let f = fixture();
        git(&f.repo, &["branch", "A"]);
        git(&f.repo, &["checkout", "-q", "-b", "B"]);
        std::fs::write(f.repo.join("b.txt"), "bee\n").unwrap();
        git(&f.repo, &["add", "-A"]);
        git(&f.repo, &["commit", "-q", "-m", "bee"]);
        git(&f.repo, &["checkout", "-q", "main"]);
        let work = tempfile::tempdir().unwrap();
        git(
            work.path(),
            &[
                "clone",
                "-q",
                "--single-branch",
                "--branch",
                "A",
                &f.repo.display().to_string(),
                "repo",
            ],
        );
        let start = git(&work.path().join("repo"), &["rev-parse", "A"]);
        Self {
            f,
            work,
            start,
            extras: None,
        }
    }

    /// Deliver `text` as the job's `extra-targets.env`.
    fn with_extras(mut self, text: String) -> Self {
        self.extras = Some(text);
        self
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

    fn b_sha(&self) -> String {
        git(&self.f.repo, &["rev-parse", "B"])
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

    fn origin_refs(&self) -> String {
        git(&self.f.repo, &["for-each-ref"])
    }

    /// Run the runner as a written job with `with` as the pool's with.list.
    fn run(&self, harness: &str, with: &[&str]) -> JobResult {
        self.run_as(harness, with, false, false)
    }

    /// As [`Rig::run`], read-only.
    fn run_read_only(&self, harness: &str, with: &[&str]) -> JobResult {
        self.run_as(harness, with, true, false)
    }

    /// As [`Rig::run`], in worktree mode.
    fn run_in_worktree(&self, harness: &str, with: &[&str]) -> JobResult {
        self.run_as(harness, with, false, true)
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
        if let Some(extras) = &self.extras {
            std::fs::write(job.join("extra-targets.env"), extras).unwrap();
        }
    }

    fn run_as(&self, harness: &str, with: &[&str], read_only: bool, worktree: bool) -> JobResult {
        let remote = if worktree {
            String::new()
        } else {
            format!("file://{}", self.f.repo.display())
        };
        let branch = if read_only { "" } else { "pool/w" };
        let mut params = format!(
            "REMOTE='{remote}'\nJOB='j1'\nTASK='t1'\nFROM='A'\nBRANCH='{branch}'\nDEPTH='1'\nBUDGET='60'\nMODEL='standin'\nSLOT='s1'\nSUBJECT='review'\nCOMMIT_NAME='Bot'\nCOMMIT_EMAIL='bot@localhost'\nENV_NAMES=''\n"
        );
        if worktree {
            params += &format!("LOCAL_ACTION='create'\nLOCAL_START='{}'\n", self.start);
        }
        if read_only {
            params += "READ_ONLY='1'\n";
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
}

/// Whether any file under `dir` mentions the https credential prefix.
pub(super) fn tree_mentions_token(dir: &Path) -> bool {
    let mut hit = false;
    for entry in std::fs::read_dir(dir).into_iter().flatten() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            hit |= tree_mentions_token(&path);
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            hit |= text.contains("x-access-token");
        }
    }
    hit
}

#[test]
fn an_extra_branch_named_by_the_job_is_fetched_and_visible_to_the_agent() {
    let rig = Rig::new();
    let b_sha = rig.b_sha();
    let seen = rig.work.path().join("seen-b");
    let harness = format!("git rev-parse refs/remotes/origin/B > {}", seen.display());
    let result = rig.run(&harness, &[&format!("B {b_sha}")]);
    assert_eq!(result.push(), "no_changes", "{:?}", result.fields);
    assert_eq!(result.get("with_count"), "1");
    assert_eq!(result.get("with_1_ref"), "B");
    assert_eq!(result.get("with_1_local"), "refs/remotes/origin/B");
    assert_eq!(result.get("with_1_sha"), b_sha);
    assert_eq!(
        std::fs::read_to_string(&seen).unwrap().trim(),
        b_sha,
        "the harness saw B at refs/remotes/origin/B"
    );
    rig.assert_no_token();
}

#[test]
fn a_named_ref_the_remote_does_not_have_ends_the_job_fetch_failed() {
    let rig = Rig::new();
    let b_sha = rig.b_sha();
    let result = rig.run("cat a.txt", &[&format!("ghost {b_sha}")]);
    assert_eq!(result.push(), "fetch_failed", "{:?}", result.fields);
    assert!(result.fields.keys().all(|k| !k.starts_with("with_")));
}

#[test]
fn a_forty_hex_ref_is_fetched_to_refs_vivi_with() {
    let rig = Rig::new();
    let b_sha = rig.b_sha();
    let local = format!("refs/vivi/with/{b_sha}");
    let result = rig.run("cat a.txt", &[&format!("{b_sha} {b_sha}")]);
    assert_eq!(result.push(), "no_changes", "{:?}", result.fields);
    assert_eq!(result.get("with_1_ref"), b_sha);
    assert_eq!(result.get("with_1_local"), local);
    assert_eq!(result.get("with_1_sha"), b_sha);
    assert_eq!(
        git(&rig.repo(), &["rev-parse", &local]),
        b_sha,
        "the commit sits at refs/vivi/with/<commit>"
    );
    rig.assert_no_token();
}

#[test]
fn a_read_only_job_can_read_an_extra_ref_and_publishes_nothing() {
    let rig = Rig::new();
    let b_sha = rig.b_sha();
    let (branches, refs) = (rig.branches(), rig.origin_refs());
    let seen = rig.work.path().join("seen-b");
    let harness = format!("git show refs/remotes/origin/B:b.txt > {}", seen.display());
    let result = rig.run_read_only(&harness, &[&format!("B {b_sha}")]);
    assert_eq!(result.push(), "read_only_unchanged", "{:?}", result.fields);
    assert_eq!(result.get("with_count"), "1");
    assert_eq!(result.get("with_1_local"), "refs/remotes/origin/B");
    assert_eq!(result.get("with_1_sha"), b_sha);
    assert_eq!(
        std::fs::read_to_string(&seen).unwrap(),
        "bee\n",
        "the read-only agent read B's content through the ref"
    );
    assert_eq!(rig.branches(), branches, "no branch made or moved");
    assert_eq!(rig.origin_refs(), refs, "nothing was published");
}

#[test]
fn without_a_with_list_nothing_is_fetched_or_reported() {
    let rig = Rig::new();
    let result = rig.run("echo change >> a.txt", &[]);
    assert_eq!(result.push(), "pushed", "{:?}", result.fields);
    assert!(result.fields.keys().all(|k| !k.starts_with("with_")));
}

#[test]
fn in_worktree_mode_a_named_branch_is_reported_without_a_fetch() {
    let rig = Rig::new().as_worktree();
    let b_sha = rig.b_sha();
    let result = rig.run_in_worktree("echo change >> a.txt", &[&format!("B {b_sha}")]);
    assert_eq!(result.push(), "committed", "{:?}", result.fields);
    assert_eq!(result.get("with_count"), "1");
    assert_eq!(result.get("with_1_ref"), "B");
    assert_eq!(result.get("with_1_local"), "B", "the ref as named");
    assert_eq!(result.get("with_1_sha"), b_sha);
}

/// One extra checkout entry of `extra-targets.env`.
fn extra_entry(index: usize, id: &str, key: &str, repo: &Path, from: &str, local: &str) -> String {
    format!(
        "EXTRA_{index}_ID='{id}'\nEXTRA_{index}_KEY='{key}'\nEXTRA_{index}_REPOSITORY='file://{}'\n\
         EXTRA_{index}_FROM='{from}'\nEXTRA_{index}_LOCAL_SOURCE='{local}'\nEXTRA_{index}_TOKEN='sekrit-extra'\n",
        repo.display()
    )
}

const KEY_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const KEY_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[test]
fn extra_targets_are_fetched_read_only_beside_the_job_and_named_in_the_brief() {
    let other = fixture();
    let other_sha = git(&other.repo, &["rev-parse", "main"]);
    let env = format!(
        "EXTRA_COUNT='1'\n{}",
        extra_entry(0, "gradus", KEY_A, &other.repo, &other_sha, "")
    );
    let rig = Rig::new().with_extras(env);

    let result = rig.run_read_only("cat a.txt", &[]);

    assert_eq!(result.push(), "read_only_unchanged", "{:?}", result.fields);
    let checkout = rig.work.path().join("targets").join(KEY_A).join("repo");
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), other_sha);
    assert!(checkout.join("a.txt").exists());
    assert_eq!(result.get("extra_count"), "1");
    assert_eq!(result.get("extra_0_target"), "gradus");
    assert_eq!(result.get("extra_0_path"), checkout.to_string_lossy());
    let brief = std::fs::read_to_string(rig.work.path().join("home/brief.md")).unwrap();
    assert!(brief.contains("the brief"), "{brief}");
    assert!(brief.contains("read-only"), "{brief}");
    assert!(
        brief.contains(&format!(
            "- gradus: {} (at {})",
            checkout.display(),
            &other_sha[..12]
        )),
        "{brief}"
    );
    // The token file is gone and no credential is left in the extra checkout.
    assert!(!rig.work.path().join("job/extra-targets.env").exists());
    assert!(!checkout.join(".git/FETCH_HEAD").exists());
    let config = std::fs::read_to_string(checkout.join(".git/config")).unwrap();
    assert!(!config.contains("sekrit-extra"), "{config}");
    assert!(
        !tree_mentions_token(&rig.work.path().join("home")),
        "token under the agent's home"
    );
    let brief_only = std::fs::read_to_string(rig.work.path().join("home/brief.md")).unwrap();
    assert!(!brief_only.contains("sekrit-extra"));
}

#[test]
fn extra_targets_in_a_worktree_slot_are_the_providers_checkouts_moved_to_their_commit() {
    let other = fixture();
    let first = git(&other.repo, &["rev-parse", "main~1"]);
    let rig = Rig::new().as_worktree();
    let checkout = rig.work.path().join("targets").join(KEY_B).join("repo");
    std::fs::create_dir_all(checkout.parent().unwrap()).unwrap();
    git(
        &other.repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            checkout.to_str().unwrap(),
        ],
    );
    let source = other.repo.display().to_string();
    let env = format!(
        "EXTRA_COUNT='1'\n{}",
        extra_entry(0, "inferentia", KEY_B, &other.repo, &first, &source)
    );
    let rig = rig.with_extras(env);

    let result = rig.run_as("cat a.txt", &[], true, true);

    assert_eq!(result.push(), "read_only_unchanged", "{:?}", result.fields);
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), first);
    assert_eq!(result.get("extra_0_target"), "inferentia");
}

#[test]
fn a_bad_extra_target_ends_the_job_before_the_agent_runs() {
    let other = fixture();
    let sha = git(&other.repo, &["rev-parse", "main"]);
    // A key that is not 64 hex digits never names a path.
    let env = format!(
        "EXTRA_COUNT='1'\n{}",
        extra_entry(0, "gradus", "../../etc", &other.repo, &sha, "")
    );
    let rig = Rig::new().with_extras(env);
    let marker = rig.work.path().join("agent-ran");
    let result = rig.run_read_only(&format!("touch {}", marker.display()), &[]);
    assert_eq!(result.push(), "target_key_invalid", "{:?}", result.fields);
    assert!(!marker.exists());

    // A worktree entry whose checkout the provider never made is refused too.
    let env = format!(
        "EXTRA_COUNT='1'\n{}",
        extra_entry(0, "gradus", KEY_A, &other.repo, &sha, "/not/prepared")
    );
    let rig = Rig::new().with_extras(env);
    let result = rig.run_read_only("true", &[]);
    assert_eq!(
        result.push(),
        "target_worktree_missing",
        "{:?}",
        result.fields
    );
}
