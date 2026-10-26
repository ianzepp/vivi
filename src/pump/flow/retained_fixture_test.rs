//! Retained-checkout regression support: a real throwaway Git origin, a host
//! whose Git reads see it, a runner adapter over a pump `JobSpec`, and the
//! seeds for completed-slice history.
//!
//! The origin's main lacks two files; its need branch gains them through two
//! distinct slice-branch merges, so every later retained regression works
//! against real commit topology instead of scripted `ls-remote` replies. The
//! runner adapter follows `src/pool/job_runner_with_test.rs`: a fresh
//! single-branch main-only clone, the real runner, a shell harness, and no
//! network, credentials, model, or production change.
//!
//! Later slices build on this module: roles, rework checkout, plan restart,
//! slice restart, and visibility tests each own their companion file.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::error::VivariumError;
use crate::pool::collect::JobResult;
use crate::pool::job::RUNNER;
use crate::pool::local_fixture::{Fixture, fixture, git};
use crate::pump::config::PumpConfig;
use crate::pump::flow::e2e_rig::push_receipt;
use crate::pump::flow::index::{
    FLOW_VERSION_NUMBER, RunRow, StepJobRow, StepRow, StepState, run_state,
};
use crate::pump::host::{CmdOutput, Ctx, Host, RealHost};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, PoolJob};
use crate::pump::store::PumpStore;

/// The need handle the fixture's branches and stores carry.
pub(crate) const NEED: &str = "regress1";
/// The merge message the origin's two slice merges carry, as the flow ships.
const MERGE_MSG: &str = "Merge one slice onto the need branch";
/// The files main lacks and the need branch gains, one per slice.
const SLICE_FILES: [(&str, &str); 2] = [("alpha.txt", "alpha\n"), ("beta.txt", "beta\n")];

/// A host whose Git source observations reflect a real repository: `git`
/// commands run for real through a [`RealHost`], while the `vivi` CLI reads
/// the pump makes beside them are stubbed to empty success (JSON reads get
/// `[]`), so a tick test can walk a real origin without a real binary.
pub(crate) struct GitHost {
    inner: RealHost,
}

impl GitHost {
    /// A host that logs to `<home>/log.txt` and runs Git for real.
    pub(crate) fn new(home: &Path) -> Self {
        Self {
            inner: RealHost::new(home),
        }
    }
}

impl Host for GitHost {
    fn now(&self) -> u64 {
        self.inner.now()
    }

    fn run(&self, cmd: &str, cwd: Option<&Path>, timeout_secs: u64) -> CmdOutput {
        if cmd.starts_with("vivi ") {
            let json = cmd.contains("--json");
            return CmdOutput {
                code: 0,
                text: if json { "[]".to_owned() } else { String::new() },
            };
        }
        self.inner.run(cmd, cwd, timeout_secs)
    }

    fn spawn(
        &self,
        cmd: &str,
        cwd: Option<&Path>,
        stdin: Option<&Path>,
        out: &Path,
    ) -> Option<u32> {
        self.inner.spawn(cmd, cwd, stdin, out)
    }

    fn log(&self, line: &str) {
        self.inner.log(line)
    }
}

/// The throwaway origin repository: `main` at [`Origin::base`] lacks both
/// slice files; the need branch `vivi/need/<NEED>` gains them through two
/// distinct slice-branch merges; main then advances past the base on its own.
pub(crate) struct Origin {
    f: Fixture,
    /// The main tip the need branch was cut from; lacks both slice files.
    pub base: String,
    /// The two slice (hand) commits, in merge order.
    pub hand_tips: [String; 2],
    /// The need branch tip after each slice merge.
    pub need_tips: [String; 2],
}

/// Build the origin: the pool fixture's main, then one slice branch and
/// `--no-ff` merge per file, then leave main checked out.
pub(crate) fn origin() -> Origin {
    let f = fixture();
    let base = git(&f.repo, &["rev-parse", "main"]);
    let mut hand_tips: [String; 2] = [String::new(), String::new()];
    let mut need_tips: [String; 2] = [String::new(), String::new()];
    git(&f.repo, &["branch", &need_branch(), "main"]);
    for (index, (file, body)) in SLICE_FILES.iter().enumerate() {
        hand_tips[index] = slice_commit(&f.repo, index, file, body);
        git(&f.repo, &["checkout", "-q", &need_branch()]);
        git(
            &f.repo,
            &[
                "merge",
                "-q",
                "--no-ff",
                "--no-edit",
                "-m",
                MERGE_MSG,
                &hand_tips[index],
            ],
        );
        need_tips[index] = git(&f.repo, &["rev-parse", "HEAD"]);
    }
    git(&f.repo, &["checkout", "-q", "main"]);
    Origin {
        f,
        base,
        hand_tips,
        need_tips,
    }
}

/// Commit one slice file on a fresh slice branch off the need branch; its tip.
fn slice_commit(repo: &Path, index: usize, file: &str, body: &str) -> String {
    let branch = format!("pump/slice-{}", index + 1);
    git(repo, &["checkout", "-q", "-B", &branch, &need_branch()]);
    std::fs::write(repo.join(file), body).unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", &format!("Add {file}")]);
    git(repo, &["rev-parse", "HEAD"])
}

/// The need branch name as a branch, without the `refs/heads/` prefix.
pub(crate) fn need_branch() -> String {
    format!("vivi/need/{NEED}")
}

/// The active need ref: the branch the flow publishes and a landing deletes.
pub(crate) fn need_ref() -> String {
    format!("refs/heads/{}", need_branch())
}

impl Origin {
    /// The origin repository path, as `file://{url()}` names it.
    pub(crate) fn repo(&self) -> &Path {
        &self.f.repo
    }

    /// The origin as a Git URL.
    pub(crate) fn url(&self) -> String {
        format!("file://{}", self.f.repo.display())
    }

    /// The final need branch tip: both files, both merges.
    pub(crate) fn need_tip(&self) -> &str {
        &self.need_tips[1]
    }

    /// `rev-parse` in the origin; panics like `local_fixture::git`.
    pub(crate) fn rev(&self, refname: &str) -> String {
        git(self.repo(), &["rev-parse", refname])
    }

    /// Whether `commit` carries `path` (a `git cat-file -e` probe).
    pub(crate) fn has(&self, commit: &str, path: &str) -> bool {
        raw_git(
            self.repo(),
            &["cat-file", "-e", &format!("{commit}:{path}")],
        )
        .map(|out| out.status.success())
        .unwrap_or(false)
    }

    /// Whether `commit` contains `ancestor` in its history.
    pub(crate) fn contains(&self, commit: &str, ancestor: &str) -> bool {
        raw_git(
            self.repo(),
            &["merge-base", "--is-ancestor", ancestor, commit],
        )
        .map(|out| out.status.success())
        .unwrap_or(false)
    }

    /// The main advancement: one more unrelated commit on main, its new tip.
    pub(crate) fn advance_main(&self) -> String {
        let notes = self.repo().join("notes.md");
        let mut text = std::fs::read_to_string(&notes).unwrap_or_default();
        text.push_str(&format!("move {}\n", text.lines().count() + 1));
        std::fs::write(&notes, text).unwrap();
        git(self.repo(), &["add", "-A"]);
        git(self.repo(), &["commit", "-q", "-m", "Advance main"]);
        git(self.repo(), &["rev-parse", "main"])
    }

    /// A remote-ref snapshot: one `refname sha` line per ref, `for-each-ref`.
    pub(crate) fn refs(&self) -> String {
        git(
            self.repo(),
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )
    }

    /// The sha `refname` sits at in the snapshot, `None` when it does not exist.
    pub(crate) fn ref_sha(&self, refname: &str) -> Option<String> {
        self.refs().lines().find_map(|line| {
            let (name, sha) = line.split_once(' ')?;
            (name == refname).then(|| sha.to_owned())
        })
    }

    /// The active-ref removal: delete the need branch, the way a landing
    /// runner deletes it after the work reached main.
    pub(crate) fn remove_need_ref(&self) {
        git(self.repo(), &["update-ref", "-d", &need_ref()]);
    }

    /// A hand-style job spec over this origin: `target` names the fixture's
    /// default target, `from` is the commit to start from, `branch` the write
    /// branch, `with` the refs the job must see. The runner adapter derives
    /// the remote and the rest of the params from this spec.
    pub(crate) fn hand_spec(&self, from: &str, branch: &str, with: &[String]) -> JobSpec {
        let mut job = JobSpec::new(
            format!("hand-{NEED}"),
            "slice",
            "hand",
            "one slice of the regression need",
            "Change one file and answer with the envelope.\n",
        );
        job.target = Some(String::from("default"));
        job.from_ref = Some(from.to_owned());
        job.branch = Some(branch.to_owned());
        job.with = with.to_vec();
        job.budget = 60;
        job
    }
}

/// Run `git` in `dir`, keeping the exit status instead of panicking.
fn raw_git(dir: &Path, args: &[&str]) -> Option<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .ok()
}

/// Resolve `name` to a commit in `repo`, `None` when Git cannot.
fn rev_commit(repo: &Path, name: &str) -> Option<String> {
    let out = raw_git(
        repo,
        &["rev-parse", "--verify", &format!("{name}^{{commit}}")],
    )?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// The 64-hex cache key of an extra target id, as dispatch names one.
fn cache_key(id: &str) -> String {
    let digest = Sha256::digest(id.as_bytes());
    hex::encode(digest)
}

/// One adapter run: the parsed result and the harness's own output.
pub(crate) struct RunOutcome {
    /// The runner's `result` file, parsed.
    pub result: JobResult,
    /// The harness's stdout, as the runner recorded it in `agent.out`.
    pub harness: String,
}

/// The job-runner adapter over a pump `JobSpec`: a fresh single-branch
/// main-only clone of the origin, the real runner, a shell harness. The
/// params come from the spec -- target remote, FROM, READ_ONLY, the with
/// list, the extra targets, the publish and delete guards -- never from a
/// substituted known-correct ref.
pub(crate) struct RunnerRig<'a> {
    origin: &'a Origin,
    work: tempfile::TempDir,
    url: String,
    remotes: BTreeMap<String, String>,
}

impl<'a> RunnerRig<'a> {
    /// A rig whose clone holds `main` only; `default` targets the origin.
    pub(crate) fn new(origin: &'a Origin) -> Self {
        let work = tempfile::tempdir().unwrap();
        git(
            work.path(),
            &[
                "clone",
                "-q",
                "--single-branch",
                "--branch",
                "main",
                &origin.url(),
                "repo",
            ],
        );
        let mut remotes = BTreeMap::new();
        remotes.insert(String::from("default"), origin.url());
        Self {
            origin,
            work,
            url: origin.url(),
            remotes,
        }
    }

    /// Also name target id `id` for the origin, so extra targets resolve.
    pub(crate) fn alias(mut self, id: &str) -> Self {
        self.remotes.insert(id.to_owned(), self.url.clone());
        self
    }

    /// The rig's work root; harness observations may write beside it.
    pub(crate) fn work(&self) -> &Path {
        self.work.path()
    }

    /// The fresh clone the runner runs in.
    pub(crate) fn repo(&self) -> PathBuf {
        self.work.path().join("repo")
    }

    /// The remote the spec's target names.
    fn remote_of(&self, target: Option<&str>) -> String {
        let id = target.unwrap_or("default");
        self.remotes
            .get(id)
            .unwrap_or_else(|| panic!("unknown target {id}"))
            .clone()
    }

    /// Run `spec` through the real runner with `harness` as the shell command.
    pub(crate) fn run(&self, spec: &JobSpec, harness: &str) -> RunOutcome {
        self.stage(spec, harness);
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
        let result = std::fs::read_to_string(self.work.path().join("job/result"))
            .unwrap_or_else(|e| panic!("no result ({e}): {log}"));
        let harness_out =
            std::fs::read_to_string(self.work.path().join("job/agent.out")).unwrap_or_default();
        RunOutcome {
            result: JobResult::parse(&result),
            harness: harness_out,
        }
    }

    /// Lay out the job directory from the spec.
    fn stage(&self, spec: &JobSpec, harness: &str) {
        let job = self.work.path().join("job");
        std::fs::create_dir_all(&job).unwrap();
        let remote = self.remote_of(spec.target.as_deref());
        let from = spec
            .from_ref
            .clone()
            .unwrap_or_else(|| String::from("main"));
        let branch = if spec.read_only {
            String::new()
        } else {
            spec.branch.clone().unwrap_or_default()
        };
        let mut params = format!(
            "REMOTE='{}'\nJOB='{}'\nTASK='{}'\nFROM='{}'\nBRANCH='{branch}'\nDEPTH='50'\n\
             BUDGET='{}'\nMODEL='standin'\nSLOT='s1'\nSUBJECT='review'\nCOMMIT_NAME='Bot'\n\
             COMMIT_EMAIL='bot@localhost'\nENV_NAMES=''\n",
            remote,
            spec.id,
            spec.task.as_deref().unwrap_or("t1"),
            from,
            spec.budget.max(60),
        );
        if spec.read_only {
            params.push_str("READ_ONLY='1'\n");
        }
        self.append_guards(&mut params, spec);
        for (name, content) in [
            ("params.env", params),
            ("token", String::new()),
            ("harness.cmd", harness.to_owned()),
            ("brief.md", String::from("the brief\n")),
            ("rules.md", String::from("rules\n")),
        ] {
            std::fs::write(job.join(name), content).unwrap();
        }
        self.stage_with(&job, &spec.with);
        self.stage_extras(&job, &spec.extra_targets);
    }

    /// The runner-only publish and delete guards, when the spec carries any.
    fn append_guards(&self, params: &mut String, spec: &JobSpec) {
        if spec.publish_ref.is_none() && spec.delete_ref.is_none() {
            return;
        }
        for (key, value) in [
            ("PUBLISH_REF", spec.publish_ref.as_deref()),
            ("PUBLISH_EXPECT", spec.publish_expect.as_deref()),
            ("PUBLISH_CONTAINS", spec.publish_contains.as_deref()),
            ("DELETE_REF", spec.delete_ref.as_deref()),
            ("DELETE_EXPECT", spec.delete_expect.as_deref()),
        ] {
            params.push_str(&format!("{key}='{}'\n", value.unwrap_or("")));
        }
    }

    /// `with.list`: each spec entry resolved against the origin, or `-`.
    fn stage_with(&self, job: &Path, with: &[String]) {
        if with.is_empty() {
            return;
        }
        let mut list = String::new();
        for entry in with {
            let commit = rev_commit(self.origin.repo(), entry).unwrap_or_else(|| "-".into());
            list.push_str(&format!("{entry} {commit}\n"));
        }
        std::fs::write(job.join("with.list"), list).unwrap();
    }

    /// `extra-targets.env`: one read-only checkout per spec extra target.
    fn stage_extras(&self, job: &Path, extras: &[String]) {
        if extras.is_empty() {
            return;
        }
        let from = git(self.origin.repo(), &["rev-parse", "main"]);
        let mut text = format!("EXTRA_COUNT='{}'\n", extras.len());
        for (index, id) in extras.iter().enumerate() {
            let key = cache_key(id);
            let fields = [
                ("ID", id.clone()),
                ("KEY", key),
                ("REPOSITORY", self.remote_of(Some(id))),
                ("FROM", from.clone()),
                ("LOCAL_SOURCE", String::new()),
                ("TOKEN", String::from("sekrit-extra")),
            ];
            for (name, value) in fields {
                text.push_str(&format!("EXTRA_{index}_{name}='{}'\n", sh_quote(&value)));
            }
        }
        std::fs::write(job.join("extra-targets.env"), text).unwrap();
    }
}

/// Quote a value for a shell-sourced file, like `pool::job::sh_quote`.
fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// A pump context whose Git source observations see `origin` for real: the
/// config names the origin as the remote, the repo is a fresh main-only
/// clone, and the host is a [`GitHost`]. Restart and tick tests open their
/// `PumpStore` at `<home>/pump.sqlite` and drive `ctx` directly.
pub(crate) struct RealGit {
    /// The context: remote at the origin, repo at the clone.
    pub ctx: Ctx,
    /// The host: real Git commands, stubbed `vivi` reads.
    pub host: Arc<GitHost>,
    clone: tempfile::TempDir,
}

/// Build [`RealGit`] over `origin`.
pub(crate) fn real_git(origin: &Origin) -> RealGit {
    let clone = tempfile::tempdir().unwrap();
    git(
        clone.path(),
        &[
            "clone",
            "-q",
            "--single-branch",
            "--branch",
            "main",
            &origin.url(),
            "repo",
        ],
    );
    let home = clone.path().join("pump-home");
    std::fs::create_dir_all(&home).unwrap();
    let host = Arc::new(GitHost::new(&home));
    let cfg = PumpConfig {
        remote: origin.url(),
        ..PumpConfig::default()
    };
    let mut ctx = Ctx::new(cfg, host.clone(), clone.path().to_path_buf(), "vivi".into());
    ctx.home = home;
    ctx.repo = clone.path().join("repo");
    RealGit { ctx, host, clone }
}

impl RealGit {
    /// The main-only clone the pump reads Git through.
    pub(crate) fn repo(&self) -> &Path {
        &self.ctx.repo
    }
}

/// Seed `store` with a live `build` run of `need` whose slices read as
/// completed: one done step (verdict `built`) and one job attempt per
/// `(node, task)`. Returns the run number. A restart or tick test pairs each
/// step with [`finished_hand_job`] under the same derived job id.
///
/// # Errors
/// Returns [`VivariumError`] when a row cannot be written.
pub(crate) fn seed_completed_run(
    store: &PumpStore,
    need: &str,
    entry: &str,
    slices: &[(&str, &str)],
) -> Result<u32, VivariumError> {
    let run = RunRow {
        need: need.to_owned(),
        run: 1,
        flow: String::from("build"),
        flow_version: FLOW_VERSION_NUMBER,
        kind: String::from("build"),
        entry: entry.to_owned(),
        state: run_state::LIVE.to_owned(),
    };
    store.insert_run(&run)?;
    for (node, task) in slices {
        store.insert_step(
            need,
            &StepRow {
                run: 1,
                node: (*node).to_owned(),
                task: (*task).to_owned(),
                state: StepState::Done,
                verdict: Some(String::from("built")),
            },
        )?;
        store.insert_step_job(
            need,
            &StepJobRow {
                job: hand_job_id(task),
                run: 1,
                node: (*node).to_owned(),
                task: (*task).to_owned(),
                attempt: 1,
            },
        )?;
    }
    Ok(1)
}

/// The job id [`seed_completed_run`] binds to `task`'s first attempt.
pub(crate) fn hand_job_id(task: &str) -> String {
    format!("{task}-a1")
}

/// A finished hand job for `task` on `branch` whose receipt reports the
/// pushed commit `sha`: the successful job source a completed slice pairs
/// with, without manufacturing one by hand.
#[must_use]
pub(crate) fn finished_hand_job(task: &str, branch: &str, sha: &str) -> PoolJob {
    let mut spec = JobSpec::new(
        hand_job_id(task),
        "slice",
        "hand",
        format!("one slice of {task}"),
        "Change one file and answer with the envelope.\n",
    );
    spec.task = Some(task.to_owned());
    spec.branch = Some(branch.to_owned());
    let mut job = PoolJob::from(spec);
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::new(),
        receipt: push_receipt(0, &format!("pushed {branch} {sha}"), "", "the envelope"),
        at: 0,
    });
    job
}

#[test]
fn the_origin_holds_base_two_slice_merges_main_advancement_and_ref_removal() {
    let origin = origin();
    // Main at base lacks both files the need branch gains.
    assert!(!origin.has(&origin.base, "alpha.txt"), "base has alpha");
    assert!(!origin.has(&origin.base, "beta.txt"), "base has beta");
    // The need branch gains them through two distinct slice merges.
    assert!(origin.has(origin.need_tip(), "alpha.txt"));
    assert!(origin.has(origin.need_tip(), "beta.txt"));
    assert!(origin.contains(&origin.need_tips[0], &origin.hand_tips[0]));
    assert!(origin.contains(&origin.need_tips[1], &origin.hand_tips[1]));
    assert_ne!(origin.need_tips[0], origin.need_tips[1]);
    assert_ne!(origin.hand_tips[0], origin.hand_tips[1]);
    // Main advances on its own line without taking the files.
    let advanced = origin.advance_main();
    assert_ne!(advanced, origin.base);
    assert!(
        !origin.has(&advanced, "alpha.txt"),
        "advanced main has alpha"
    );
    assert!(origin.contains(&advanced, &origin.base));
    // The snapshot sees the active ref until it is removed.
    assert_eq!(
        origin.ref_sha(&need_ref()).as_deref(),
        Some(origin.need_tip())
    );
    origin.remove_need_ref();
    assert_eq!(origin.ref_sha(&need_ref()), None, "need ref survived");
}

#[test]
fn the_main_only_rig_runs_a_spec_derived_hand_job_against_the_real_origin() {
    let origin = origin();
    let rig = RunnerRig::new(&origin);
    // The setup: a fresh single-branch main-only clone.
    assert_eq!(
        git(&rig.repo(), &["symbolic-ref", "--short", "HEAD"]),
        "main"
    );
    assert_eq!(
        git(
            &rig.repo(),
            &["for-each-ref", "--format=%(refname)", "refs/heads"]
        ),
        "refs/heads/main"
    );
    // The spec: start from base, must see the need branch.
    let spec = origin.hand_spec(&origin.base, "pool/hand", &[need_branch()]);
    let seen = rig.work().join("seen-alpha");
    let harness = format!(
        "git show refs/remotes/origin/{0}:alpha.txt > {1} && echo hand >> a.txt",
        need_branch(),
        seen.display()
    );
    let run = rig.run(&spec, &harness);
    assert_eq!(run.result.push(), "pushed", "{}", run.result.summary());
    assert_eq!(run.result.get("with_1_ref"), need_branch());
    assert_eq!(run.result.get("with_1_sha"), origin.need_tip());
    assert_eq!(std::fs::read_to_string(&seen).unwrap(), "alpha\n");
    // The job's branch landed on the origin at a commit above base.
    let pushed = origin
        .ref_sha("refs/heads/pool/hand")
        .expect("pool/hand missing");
    assert!(origin.contains(&pushed, &origin.base));
    assert_eq!(run.result.get("sha"), pushed);
}

#[test]
fn a_spec_driven_slice_merge_publishes_and_extends_the_need_branch() {
    let origin = origin();
    // One real hand run extends the topology: a third file above the tip.
    let hand = RunnerRig::new(&origin);
    let spec = origin.hand_spec(origin.need_tip(), "pool/hand-3", &[]);
    let run = hand.run(&spec, "echo gamma > gamma.txt");
    assert_eq!(run.result.push(), "pushed", "{}", run.result.summary());
    let hand_sha = origin
        .ref_sha("refs/heads/pool/hand-3")
        .expect("hand-3 missing");
    // The merge run: derived from its own spec, published to the active ref.
    let merge = RunnerRig::new(&origin);
    let mut spec = origin.hand_spec(
        origin.need_tip(),
        "pool/merge-3",
        &[String::from("pool/hand-3")],
    );
    spec.publish_ref = Some(need_ref());
    spec.publish_contains = Some(hand_sha.clone());
    let cmd =
        format!("git merge --no-ff --no-edit -m '{MERGE_MSG}' refs/remotes/origin/pool/hand-3");
    let run = merge.run(&spec, &cmd);
    assert_eq!(run.result.push(), "pushed", "{}", run.result.summary());
    assert_eq!(run.result.get("publish_state"), "pushed");
    let landed = origin.ref_sha(&need_ref()).expect("need ref missing");
    assert_ne!(landed, origin.need_tip(), "the need branch did not move");
    assert!(origin.contains(&landed, &hand_sha));
    assert!(origin.has(&landed, "gamma.txt"));
}
