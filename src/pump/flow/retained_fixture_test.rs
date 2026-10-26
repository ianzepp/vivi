//! The retained-checkout regression fixture: a throwaway Git repository whose
//! `main` lacks two files and whose need branch gains them through two
//! distinct slice-branch merges, a [`GitHost`] that observes the real
//! repository while stubbing unrelated CLI reads, and a runner adapter that
//! executes a pump [`JobSpec`] through the real runner on a fresh
//! single-branch main-only clone.
//!
//! Restart, rework-checkout, visibility and role tests build on this so their
//! Git source observations and completed-slice history always agree with a
//! real repository instead of manufactured SHAs.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use crate::error::VivariumError;
use crate::pool::collect::JobResult;
use crate::pool::job::{RUNNER, sh_quote};
use crate::pool::local_fixture::{self, git};
use crate::pump::flow::e2e_rig::{Jobs, push_receipt};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::need_branch::branch as need_branch_name;
use crate::pump::host::{CmdOutput, Host, RealHost};
use crate::pump::jobs::{self, JobOutcome, JobSpec, JobStatus, JobStore};
use crate::pump::store::{NeedBranchRow, PumpStore};

type R = Result<(), VivariumError>;

/// The target id the fixture's origin repository answers for.
pub(crate) const TARGET: &str = "vivi";
/// The file the first slice merge brings onto the need branch.
pub(crate) const FILE_ONE: &str = "one.txt";
/// The file the second slice merge brings onto the need branch.
pub(crate) const FILE_TWO: &str = "two.txt";
/// Job kind of the merge job the seeded history carries.
const MERGE_KIND: &str = "slice-merge";
/// The verdict a merged slice step carries.
const MERGED: &str = "merged";

/// A throwaway origin repository with the retained-checkout topology.
pub(crate) struct Fixture {
    dir: TempDir,
    /// The origin repository, left checked out on `main`.
    pub origin: PathBuf,
    /// The need id the branches are named after.
    pub need: String,
    /// The main commit the need branch was cut from.
    pub base: String,
    /// The need-branch tip after the first and the second slice merge.
    pub tips: [String; 2],
    /// The slice commit each merge brought in, matching `tips` by index.
    pub slices: [String; 2],
    /// Extra throwaway repositories registered for `spec.extra_targets`.
    extras: Mutex<BTreeMap<String, PathBuf>>,
}

/// A fixture for `need`: `main` at [`FILE_ONE`]-lacking state, the need branch
/// cut at that commit, then two slice branches each adding one file and
/// merged `--no-ff` into the need branch in turn.
pub(crate) fn fixture(need: &str) -> Fixture {
    let plain = local_fixture::fixture();
    let origin = plain.repo.clone();
    let base = git(&origin, &["rev-parse", "main"]);
    let branch = need_branch_name(need);
    git(&origin, &["branch", &branch, "main"]);
    let slice0 = add_file(&origin, &branch, FILE_ONE, "one\n", "slice one", &base, 0);
    let tip0 = merge_slice(&origin, &branch, &slice0);
    let slice1 = add_file(&origin, &branch, FILE_TWO, "two\n", "slice two", &tip0, 1);
    let tip1 = merge_slice(&origin, &branch, &slice1);
    git(&origin, &["checkout", "-q", "main"]);
    Fixture {
        dir: plain.dir,
        origin,
        need: need.to_string(),
        base,
        tips: [tip0, tip1],
        slices: [slice0, slice1],
        extras: Mutex::new(BTreeMap::new()),
    }
}

/// Commit `content` at `file` on a new slice branch cut at `start`, and check
/// the need branch out again; the slice commit.
fn add_file(
    origin: &Path,
    need_branch: &str,
    file: &str,
    content: &str,
    what: &str,
    start: &str,
    index: usize,
) -> String {
    let slice = format!("vivi/unit/s{index}");
    git(origin, &["checkout", "-q", "-B", &slice, start]);
    std::fs::write(origin.join(file), content).unwrap();
    git(origin, &["add", "-A"]);
    git(origin, &["commit", "-q", "-m", what]);
    git(origin, &["checkout", "-q", need_branch]);
    git(origin, &["rev-parse", &slice])
}

/// Merge one slice commit `--no-ff` into the need branch; the merged tip.
fn merge_slice(origin: &Path, need_branch: &str, slice: &str) -> String {
    git(
        origin,
        &[
            "merge",
            "--no-ff",
            "--no-edit",
            "-m",
            "Merge one slice onto the need branch",
            slice,
        ],
    );
    git(origin, &["rev-parse", "HEAD"])
}

impl Fixture {
    /// The `file://` URL of the origin repository.
    pub fn url(&self) -> String {
        format!("file://{}", self.origin.display())
    }

    /// The need branch name (`vivi/need/<need>`).
    pub fn need_branch(&self) -> String {
        need_branch_name(&self.need)
    }

    /// The need branch's full ref name.
    pub fn need_ref(&self) -> String {
        format!("refs/heads/{}", self.need_branch())
    }

    /// Slice branch `index`'s full ref name.
    pub fn slice_ref(&self, index: usize) -> String {
        format!("refs/heads/vivi/unit/s{index}")
    }

    /// The current tip of `main`, read from the real repository.
    pub fn main_tip(&self) -> String {
        git(&self.origin, &["rev-parse", "main"])
    }

    /// The current tip of the need branch, read from the real repository.
    pub fn need_tip(&self) -> String {
        git(&self.origin, &["rev-parse", self.need_ref()])
    }

    /// A `git ls-remote` snapshot of every ref the origin publishes.
    pub fn remote_refs(&self) -> String {
        git(self.dir.path(), &["ls-remote", &self.url()])
    }

    /// Advance `main` by one real commit and return its new tip.
    pub fn advance_main(&self) -> String {
        git(&self.origin, &["checkout", "-q", "main"]);
        let mut text = std::fs::read_to_string(self.origin.join("a.txt")).unwrap();
        text.push_str("more\n");
        std::fs::write(self.origin.join("a.txt"), text).unwrap();
        git(&self.origin, &["commit", "-q", "-am", "advance main"]);
        self.main_tip()
    }

    /// Remove the need branch and both slice branches from the origin, the
    /// way prune does; the commits stay in the object store.
    pub fn remove_active_refs(&self) {
        for name in [self.need_branch(), String::from("vivi/unit/s0"), String::from("vivi/unit/s1")] {
            git(&self.origin, &["branch", "-qD", &name]);
        }
    }

    /// A fresh full clone of the origin whose remote is named `origin`,
    /// under the fixture's directory.
    pub fn checkout(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join("checkout").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        git(
            self.dir.path(),
            &["clone", "-q", &self.url(), path.to_str().unwrap()],
        );
        path
    }

    /// The `file://` URL of the extra repository registered for target `id`,
    /// creating the throwaway repository on first use.
    fn extra_url(&self, id: &str) -> String {
        let mut extras = self
            .extras
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let repo = extras.entry(id.to_string()).or_insert_with(|| {
            let plain = local_fixture::fixture();
            let path = self.dir.path().join("extra").join(id);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::rename(&plain.repo, &path).unwrap();
            path
        });
        format!("file://{}", repo.display())
    }
}

/// Whether `git` succeeds with `args` in `dir`; for negative assertions the
/// panicking [`git`] helper cannot make.
fn git_ok(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// A host whose Git commands run for real against the fixture while every
/// other CLI read is stubbed: rules answer first, non-Git commands without a
/// rule get a harmless empty success, and Git commands are delegated to a
/// [`RealHost`], so `ls-remote` observes the real repository.
pub(crate) struct GitHost {
    real: RealHost,
    clock: AtomicU64,
    stubs: Mutex<Vec<(String, i32, String)>>,
    commands: Mutex<Vec<String>>,
    logs: Mutex<Vec<String>>,
}

impl GitHost {
    /// A host whose clock starts at `start`.
    pub fn new(start: u64) -> Arc<GitHost> {
        Arc::new(GitHost {
            real: RealHost::new(Path::new("/tmp")),
            clock: AtomicU64::new(start),
            stubs: Mutex::new(Vec::new()),
            commands: Mutex::new(Vec::new()),
            logs: Mutex::new(Vec::new()),
        })
    }

    /// Persistent rule: a `run` whose command holds `contains` answers with
    /// `code`/`text` without reaching the real host.
    pub fn stub(&self, contains: &str, code: i32, text: &str) {
        let mut stubs = self.stubs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        stubs.push((contains.to_string(), code, text.to_string()));
    }

    /// Every `run` command line, in order.
    pub fn commands(&self) -> Vec<String> {
        self.commands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

fn first_word(cmd: &str) -> &str {
    cmd.split_whitespace().next().unwrap_or("")
}

impl Host for GitHost {
    fn now(&self) -> u64 {
        self.clock.load(Ordering::SeqCst)
    }

    fn run(&self, cmd: &str, cwd: Option<&Path>, timeout_secs: u64) -> CmdOutput {
        self.commands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(cmd.to_string());
        let stubs = self.stubs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, code, text)) = stubs.iter().find(|(frag, _, _)| cmd.contains(frag)) {
            return CmdOutput {
                code: *code,
                text: text.clone(),
            };
        }
        if first_word(cmd) == "git" {
            return self.real.run(cmd, cwd, timeout_secs);
        }
        CmdOutput {
            code: 0,
            text: String::new(),
        }
    }

    fn spawn(&self, cmd: &str, cwd: Option<&Path>, stdin: Option<&Path>, out: &Path) -> Option<u32> {
        self.real.spawn(cmd, cwd, stdin, out)
    }

    fn log(&self, line: &str) {
        self.logs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(line.to_string());
    }
}

/// The outcome of running one pump [`JobSpec`] through the real runner.
pub(crate) struct JobRun {
    /// The parsed `job/result` file the runner wrote.
    pub result: JobResult,
    work: TempDir,
}

impl JobRun {
    /// The clone the runner worked in.
    pub fn repo(&self) -> PathBuf {
        self.work.path().join("repo")
    }

    /// The runner's own stdout, for asserting on its log.
    pub fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.work.path().join(rel))
            .unwrap_or_else(|e| panic!("no {rel} under the work root: {e}"))
    }
}

impl Fixture {
    /// Run the real runner for `spec` with `harness` as the stand-in shell
    /// command, starting from a fresh single-branch `main`-only clone of the
    /// origin. The remote, `FROM`, `READ_ONLY`, the with list and the extra
    /// targets all derive from the spec; with refs resolve against the real
    /// origin, never a substituted ref.
    pub fn run_job(&self, spec: &JobSpec, harness: &str) -> JobRun {
        let work = tempfile::tempdir().unwrap();
        let repo = work.path().join("repo");
        git(
            work.path(),
            &[
                "clone",
                "-q",
                "--single-branch",
                "--branch",
                "main",
                &self.url(),
                repo.to_str().unwrap(),
            ],
        );
        let job = work.path().join("job");
        std::fs::create_dir_all(&job).unwrap();
        self.stage(&job, spec, harness);
        let out = Command::new("bash")
            .arg("-c")
            .arg(RUNNER)
            .env("WORK", work.path())
            .env("UNPRIVILEGED", "1")
            .env("BUILD_DIR", "build")
            .env("BUILD_PERSISTS", "1")
            .env("HOME", work.path().join("home"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("src/pool/local-bin")
                        .display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&out.stdout).into_owned();
        let text = std::fs::read_to_string(job.join("result"))
            .unwrap_or_else(|e| panic!("no result ({e}): {log}"));
        JobRun {
            result: JobResult::parse(&text),
            work,
        }
    }

    /// Write the job files the runner reads: params, harness, brief, rules,
    /// and the with list and extra targets the spec names.
    fn stage(&self, job: &Path, spec: &JobSpec, harness: &str) {
        let mut files = vec![
            ("params.env", self.params(spec)),
            ("token", String::new()),
            ("harness.cmd", harness.to_string()),
            ("brief.md", String::from("the brief\n")),
            ("rules.md", String::from("rules\n")),
        ];
        let with = self.with_list(spec);
        if !with.is_empty() {
            files.push(("with.list", with));
        }
        let extras = self.extras_env(spec);
        if !extras.is_empty() {
            files.push(("extra-targets.env", extras));
        }
        for (name, content) in files {
            std::fs::write(job.join(name), content).unwrap();
        }
    }

    /// The `params.env` text: every field derived from the spec, with the
    /// stand-in identity, slot and depth a pool would add.
    fn params(&self, spec: &JobSpec) -> String {
        let branch = match (spec.read_only, &spec.branch) {
            (true, _) => String::new(),
            (false, Some(branch)) => branch.clone(),
            (false, None) => String::from("pool/w"),
        };
        let fields = [
            ("REMOTE", self.remote_of(spec)),
            ("JOB", spec.id.clone()),
            ("TASK", spec.id.clone()),
            ("FROM", spec.from_ref.clone().unwrap_or_else(|| "main".into())),
            ("BRANCH", branch),
            ("DEPTH", String::from("1")),
            ("BUDGET", spec.budget.to_string()),
            ("MODEL", spec.model.clone().unwrap_or_else(|| "standin".into())),
            ("SLOT", String::from("s1")),
            ("SUBJECT", spec.subject.clone()),
            ("COMMIT_NAME", String::from("Bot")),
            ("COMMIT_EMAIL", String::from("bot@localhost")),
            ("ENV_NAMES", String::new()),
        ];
        let mut params: String = fields
            .iter()
            .map(|(key, value)| format!("{key}={}\n", sh_quote(value)))
            .collect();
        if spec.read_only {
            params.push_str("READ_ONLY='1'\n");
        }
        params
    }

    /// The remote the job's own checkout uses: the origin for the fixture's
    /// target (or none), the registered extra repository for an extra target.
    fn remote_of(&self, spec: &JobSpec) -> String {
        match spec.target.as_deref() {
            None | Some(TARGET) => self.url(),
            Some(id) => self.extra_url(id),
        }
    }

    /// The `with.list` text: one `<name> <sha>` line per spec with ref, the
    /// SHA resolved from the real origin; 40-hex names travel as given.
    fn with_list(&self, spec: &JobSpec) -> String {
        spec.with
            .iter()
            .map(|name| {
                let sha = if is_sha(name) {
                    name.clone()
                } else {
                    git(&self.origin, &["rev-parse", name])
                };
                format!("{name} {sha}\n")
            })
            .collect()
    }

    /// The `extra-targets.env` text for the spec's extra targets, each a
    /// read-only checkout of its registered repository at its `main` tip.
    fn extras_env(&self, spec: &JobSpec) -> String {
        if spec.extra_targets.is_empty() {
            return String::new();
        }
        let mut text = format!(
            "EXTRA_COUNT='{}'\n",
            sh_quote(&spec.extra_targets.len().to_string())
        );
        for (index, id) in spec.extra_targets.iter().enumerate() {
            let repository = self.extra_url(id);
            let from = git(Path::new(repository.trim_start_matches("file://")), &[
                "rev-parse",
                "main",
            ]);
            for (name, value) in [
                ("ID", id.clone()),
                ("KEY", format!("{index:0>64}")),
                ("REPOSITORY", repository),
                ("FROM", from),
                ("LOCAL_SOURCE", String::new()),
                ("TOKEN", String::from("sekrit-extra")),
            ] {
                text.push_str(&format!("EXTRA_{index}_{name}={}\n", sh_quote(&value)));
            }
        }
        text
    }
}

/// Whether `value` is a full 40- or 64-character hex commit name.
fn is_sha(value: &str) -> bool {
    (value.len() == 40 || value.len() == 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
}

impl Fixture {
    /// Seed the need branch row: the fixture's branch, live on `run`, cut at
    /// the fixture's base, on the default target.
    ///
    /// # Errors
    /// Returns a store error.
    pub fn seed_need_branch(&self, store: &PumpStore, need: &str, run: u32) -> R {
        store.upsert_need_branch(&NeedBranchRow {
            need: need.to_string(),
            branch: self.need_branch(),
            base: Some(self.base.clone()),
            status: String::from("live"),
            run,
            target: String::from("default"),
            priors: Vec::new(),
        })
    }

    /// Insert `run`'s step for `node` as done with the merged verdict, the
    /// store side of a completed slice.
    ///
    /// # Errors
    /// Returns a store error.
    pub fn complete_slice_step(
        &self,
        store: &PumpStore,
        run: &RunRow,
        node: &str,
        task: &str,
    ) -> R {
        store.insert_step(
            &run.need,
            &StepRow {
                run: run.run,
                node: node.to_string(),
                task: task.to_string(),
                state: StepState::Done,
                verdict: Some(MERGED.to_string()),
            },
        )
    }

    /// Seed the job side of a completed slice: the hand job that pushed
    /// slice `index` and the merge job that published the need branch at the
    /// matching merged tip, both finished with receipts naming the real
    /// commits, the merge marked applied. Job ids derive from `index`, so
    /// one history per index fits a store.
    ///
    /// # Errors
    /// Returns an enqueue error.
    pub fn seed_merged_slice(
        &self,
        jobs: &mut impl JobStore,
        task: &str,
        node: &str,
        index: usize,
    ) -> R {
        let hand_id = format!("hand-{index}");
        let mut hand = JobSpec::new(&hand_id, "hand", "hand", "one slice", "the brief");
        hand.task = Some(task.to_string());
        hand.from_ref = Some(self.base.clone());
        hand.branch = Some(format!("vivi/unit/s{index}"));
        hand.payload = serde_json::json!({"node": node, "attempt": 1});
        jobs::enqueue(jobs, hand)?;
        let slice_branch = self.slice_ref(index);
        finish(
            jobs,
            &hand_id,
            &push_receipt(
                0,
                &format!("pushed {slice_branch} {}", self.slices[index]),
                "",
                "hand's final message",
            ),
        );
        let merge_id = format!("merge-{index}");
        let mut merge = JobSpec::new(&merge_id, MERGE_KIND, "merge", "merge one slice", "the brief");
        merge.task = Some(task.to_string());
        merge.from_ref = Some(if index == 0 {
            self.base.clone()
        } else {
            self.tips[index - 1].clone()
        });
        merge.branch = Some(self.need_branch());
        merge.publish_ref = Some(self.need_ref());
        merge.payload = serde_json::json!({"node": node, "attempt": 1});
        jobs::enqueue(jobs, merge)?;
        finish(
            jobs,
            &merge_id,
            &push_receipt(
                0,
                &format!("pushed {} {}", self.need_ref(), self.tips[index]),
                &format!(
                    "runner publish: pushed {} {} {}",
                    self.need_ref(),
                    self.slices[index],
                    self.tips[index]
                ),
                "merged",
            ),
        );
        jobs::mark_applied(jobs, &merge_id, 0);
        Ok(())
    }
}

/// Finish job `id` the pool way: `finished` with `receipt` as the summary.
fn finish(jobs: &mut impl JobStore, id: &str, receipt: &str) {
    let job = jobs
        .jobs_mut()
        .get_mut(id)
        .unwrap_or_else(|| panic!("no job {id}"));
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::new(),
        receipt: receipt.to_string(),
        at: 0,
    });
}

#[test]
fn the_two_slice_merges_build_the_need_branch_over_a_main_that_lacks_both_files() {
    let f = fixture("n1");
    assert!(!git_ok(
        &f.origin,
        &["cat-file", "-e", &format!("main:{FILE_ONE}")]
    ));
    assert!(!git_ok(
        &f.origin,
        &["cat-file", "-e", &format!("main:{FILE_TWO}")]
    ));
    assert_eq!(
        git(&f.origin, &["show", &format!("{}:{FILE_ONE}", f.need_branch())]),
        "one\n"
    );
    assert_eq!(
        git(&f.origin, &["show", &format!("{}:{FILE_TWO}", f.need_branch())]),
        "two\n"
    );
    // Each merged tip: first parent the prior tip (or the base), second
    // parent its own slice commit.
    assert_eq!(git(&f.origin, &["rev-parse", &format!("{}^1", f.tips[0])]), f.base);
    assert_eq!(
        git(&f.origin, &["rev-parse", &format!("{}^2", f.tips[0])]),
        f.slices[0]
    );
    assert_eq!(git(&f.origin, &["rev-parse", &format!("{}^1", f.tips[1])]), f.tips[0]);
    assert_eq!(
        git(&f.origin, &["rev-parse", &format!("{}^2", f.tips[1])]),
        f.slices[1]
    );
    let refs = f.remote_refs();
    for name in [f.need_ref(), f.slice_ref(0), f.slice_ref(1)] {
        assert!(refs.contains(&name), "{refs}");
    }
}

#[test]
fn main_advances_and_the_active_refs_can_be_removed_on_the_origin() {
    let f = fixture("n2");
    let before = f.main_tip();
    let advanced = f.advance_main();
    assert_ne!(advanced, before);
    assert_eq!(f.main_tip(), advanced);
    assert!(
        f.remote_refs()
            .contains(&format!("{advanced}\trefs/heads/main")),
        "{}",
        f.remote_refs()
    );
    let tip = f.need_tip();
    f.remove_active_refs();
    let refs = f.remote_refs();
    assert!(refs.contains("refs/heads/main"), "{refs}");
    assert!(!refs.contains("refs/heads/vivi/"), "{refs}");
    // The merged commits stay in the object store.
    assert!(git_ok(&f.origin, &["cat-file", "-e", &tip]));
}

#[test]
fn a_read_only_job_spec_runs_on_a_main_only_clone_from_its_own_refs() {
    let f = fixture("n3");
    let mut spec = JobSpec::new("j1", "gate", "gate", "verify the need branch", "check it");
    spec.read_only = true;
    spec.target = Some(TARGET.to_string());
    spec.from_ref = Some(f.need_branch());
    spec.with = vec![f.need_branch()];
    let run = f.run_job(&spec, "test -f one.txt && test -f two.txt && test ! -f b.txt");
    assert_eq!(run.result.push(), "read_only_unchanged", "{:?}", run.result.fields);
    assert_eq!(run.result.get("with_1_ref"), f.need_branch());
    // The SHA was resolved from the real origin, not substituted.
    assert_eq!(run.result.get("with_1_sha"), f.need_tip());
    assert_eq!(
        run.result.get("with_1_local"),
        format!("refs/remotes/origin/{}", f.need_branch())
    );
    // The clone held only main before the runner fetched FROM.
    assert_eq!(
        git(&run.repo(), &["for-each-ref", "--format=%(refname)", "refs/heads"]),
        "refs/heads/main"
    );
}

#[test]
fn the_seeded_history_carries_the_real_published_commits() {
    let f = fixture("n4");
    let dir = tempfile::tempdir().unwrap();
    let store = PumpStore::open(&dir.path().join(crate::pump::store::DB_FILENAME)).unwrap();
    let run = RunRow {
        need: f.need.clone(),
        run: 1,
        flow: String::from("build"),
        flow_version: 2,
        kind: String::from("build"),
        entry: String::from("rework1"),
        state: String::from("live"),
    };
    store.insert_run(&run).unwrap();
    f.seed_need_branch(&store, &f.need, 1).unwrap();
    f.complete_slice_step(&store, &run, "slice:a", "t-a").unwrap();
    let mut jobs = Jobs::default();
    f.seed_merged_slice(&mut jobs, "t-a", "slice:a", 0).unwrap();
    let branch = store.need_branch(&f.need).unwrap().unwrap();
    assert_eq!((branch.base.as_deref(), branch.run), (Some(f.base.as_str()), 1));
    let hand = &jobs.jobs["hand-0"];
    assert_eq!(hand.status, JobStatus::Finished);
    let publish = jobs.jobs["merge-0"]
        .outcome
        .as_ref()
        .unwrap()
        .runner_publish()
        .unwrap();
    assert!(publish.succeeded());
    assert_eq!(publish.remote_sha, f.tips[0]);
    assert_eq!(jobs.jobs["merge-0"].applied_at, Some(0));
}

#[test]
fn the_git_host_observes_the_real_repository_and_stubs_other_reads() {
    let f = fixture("n5");
    let clone = f.checkout("probe");
    let host = GitHost::new(1_000_000);
    let out = host.run(
        &format!("git ls-remote origin {}", f.need_ref()),
        Some(&clone),
        30,
    );
    assert_eq!(out.code, 0);
    assert!(out.text.contains(&f.need_tip()), "{:?}", out.text);
    let advanced = f.advance_main();
    let out = host.run("git ls-remote origin refs/heads/main", Some(&clone), 30);
    assert!(out.text.contains(&advanced), "{:?}", out.text);
    host.stub("need show", 0, "scripted\n");
    let out = host.run("vivi need show n5", None, 10);
    assert_eq!((out.code, out.text.as_str()), (0, "scripted\n"));
    let out = host.run("vivi graph ready", None, 10);
    assert_eq!((out.code, out.text.as_str()), (0, ""));
    assert_eq!(host.commands().len(), 4);
}
