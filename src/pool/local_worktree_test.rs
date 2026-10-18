//! A worktree pool end to end: the real provider, the real boot and runner
//! scripts, real processes and a throwaway repository, with a stand-in shell
//! harness. Each test has a rig of its own.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::*;
use crate::mailspace::{Mailspace, SendRequest};

use crate::pool::collect::{ErrorWatch, JobResult, Pacing, RoundLine};
use crate::pool::config::PoolConfig;
use crate::pool::dispatch::Launch;
use crate::pool::dispatch::{self, DispatchFlags};
use crate::pool::github::Secret;
use crate::pool::job::DEFAULT_RULES;
use crate::pool::local_branch;
use crate::pool::local_fixture::{Fixture, fixture, git};
use crate::pool::local_git::Repo;
use crate::pool::pools::PoolSet;
use crate::pool::progress::Progress;
use crate::pool::record::DispatchRecord;
use crate::pool::run::{self, CreateOptions};
use crate::pool::slot::PoolId;
use crate::pool::work::{self, WaitOptions, Waited};

/// The harness that appends a line to a file and prints a receipt.
const WRITE: &str = "echo work >> standin.txt; echo receipt";
const NOTHING: &str = "echo receipt";

struct Rig {
    f: Fixture,
    _root_dir: tempfile::TempDir,
    root: PathBuf,
    provider: LocalProvider,
    config: PoolConfig,
    id: PoolId,
    space: Mailspace,
    _space_dir: tempfile::TempDir,
    sent: u32,
}

fn pacing() -> Pacing {
    Pacing {
        poll: Duration::from_millis(50),
        ready_timeout: Duration::from_secs(30),
        stop_timeout: Duration::from_secs(30),
        start_grace: Duration::ZERO,
    }
}

impl Rig {
    fn new() -> Self {
        let f = fixture();
        let root_dir = tempfile::tempdir().unwrap();
        let root = root_dir.path().canonicalize().unwrap().join("lanes");
        let config: PoolConfig = toml::from_str(&format!(
            "provider = \"local\"\nslots = 2\nbuild_dir = \"build\"\nseed = [\"mkdir -p build && echo warm > build/warm.txt\"]\n[local]\nroot = \"{}\"\nworktree_of = \"{}\"\n[harness]\nname = \"standin\"\nrun = \"true\"\nmodels = [\"standin\"]\n[committer]\nname = \"Pool Test\"\nemail = \"pool-test@localhost\"\n",
            root.display(),
            f.repo.display()
        ))
        .unwrap();
        let provider = LocalProvider::new(&config);
        let id = PoolId::new("e2e", "wt");
        let base = local_branch::base_commit(&Repo::new(&f.repo), None).unwrap();
        let options = CreateOptions {
            slots: Some(2),
            spacing: Duration::ZERO,
            poll: Duration::from_millis(50),
            timeout: Duration::from_mins(1),
            confirm_timeout: Duration::from_secs(20),
            heartbeat: Duration::from_hours(1),
        };
        let seed = || Ok((Secret::new(String::new()), base));
        let report =
            run::create_slots(&provider, &config, &id, &options, &Progress::silent(), seed)
                .unwrap();
        assert!(
            report.created.iter().all(run::SlotOutcome::seeded),
            "{:?}",
            report.created
        );
        let space_dir = tempfile::tempdir().unwrap();
        let mut space = Mailspace::init(Some(space_dir.path())).unwrap();
        space.add_identity("mind").unwrap();
        space.add_identity("hand").unwrap();
        Self {
            f,
            _root_dir: root_dir,
            root,
            provider,
            config,
            id,
            space,
            _space_dir: space_dir,
            sent: 0,
        }
    }

    fn task(&mut self) -> String {
        self.sent += 1;
        let sent = self
            .space
            .send(SendRequest {
                from: "mind".into(),
                to: vec!["hand".into()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: format!("job {}", self.sent),
                body: "a throwaway task\n".into(),
                role: "tasks".into(),
                kind: Some("task".into()),
                reply_to: None,
                depends_on: Vec::new(),
            })
            .unwrap();
        sent.delivered[0].handle.clone()
    }

    /// Decide, claim a slot and start the job, as the command does.
    fn dispatch(
        &mut self,
        (from, branch): (&str, &str),
        harness: &str,
        budget: u64,
    ) -> Result<(String, DispatchRecord), VivariumError> {
        self.dispatch_to(from, Some(branch), harness, budget)
    }

    /// As `dispatch`; without a branch the job is read-only.
    fn dispatch_to(
        &mut self,
        from: &str,
        branch: Option<&str>,
        harness: &str,
        budget: u64,
    ) -> Result<(String, DispatchRecord), VivariumError> {
        let task = self.task();
        let mut config = self.config.clone();
        config.harness.as_mut().unwrap().run = harness.to_string();
        let flags = DispatchFlags {
            model: "standin".into(),
            from: Some(from.into()),
            branch: branch.map(str::to_string),
            with: Vec::new(),
            budget: Some(budget),
            allow_default_branch: false,
            read_only: branch.is_none(),
            reasoning: None,
        };
        let job = dispatch::new_job_id(&task, u128::from(self.sent), std::process::id());
        let mut plan = dispatch::plan_job(
            &config,
            "wt",
            (&task, "subject", ""),
            &flags,
            DEFAULT_RULES.into(),
            job,
        )?;
        let repo = Repo::new(&self.f.repo);
        let checkout = if plan.read_only {
            local_branch::prepare_read_only(&repo, &plan.refs.from)?
        } else {
            local_branch::prepare(&repo, &plan.refs, false)?
        };
        let start = checkout.start.clone();
        plan.checkout = Some(checkout);
        let token = Secret::new(String::new());
        let progress = Progress::silent();
        let launch = Launch {
            config: &self.config,
            plan: &plan,
            token: &token,
            start: &start,
            progress: &progress,
            ready_timeout: None,
            reset_base: None,
        };
        let made = work::submit(&self.space, &self.provider, &self.id, &launch, &pacing())?;
        Ok((task, made))
    }

    fn wait(&self, task: &str) -> RoundLine {
        let options = WaitOptions {
            limit: Duration::from_mins(1),
            poll: Duration::from_millis(50),
            pacing: pacing(),
            heartbeat: Duration::from_hours(1),
            cancelled: &|| false,
        };
        let set = PoolSet::single(&self.id, &self.provider);
        let mut watch = ErrorWatch::default();
        match work::wait(
            &self.space,
            &set,
            &[task.to_string()],
            &options,
            &mut watch,
            &mut |_| {},
        )
        .unwrap()
        {
            Waited::Settled(mut lines) => lines.pop().unwrap(),
            Waited::NothingOpen => panic!("nothing was open"),
        }
    }

    /// Dispatch, wait, and read the result the runner left.
    fn run(&mut self, refs: (&str, &str), harness: &str) -> (JobResult, DispatchRecord) {
        let (task, made) = self.dispatch(refs, harness, 120).unwrap();
        let line = self.wait(&task);
        assert_eq!(line.state, "finished", "{line:?}");
        (self.result(&made), made)
    }

    /// Dispatch a read-only job from `from`, wait, and read the result.
    fn run_read_only(&mut self, from: &str, harness: &str) -> (JobResult, DispatchRecord) {
        let (task, made) = self.dispatch_to(from, None, harness, 120).unwrap();
        let line = self.wait(&task);
        assert_eq!(line.state, "finished", "{line:?}");
        (self.result(&made), made)
    }

    /// Every local branch and where it points.
    fn branches(&self) -> String {
        git(
            &self.f.repo,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/heads",
            ],
        )
    }

    fn result(&self, made: &DispatchRecord) -> JobResult {
        let text =
            std::fs::read_to_string(self.root.join(&made.machine).join("job/result")).unwrap();
        JobResult::parse(&text)
    }

    fn checkout(&self, made: &DispatchRecord) -> PathBuf {
        self.root.join(&made.machine).join("repo")
    }

    /// The slots' worktrees as Git lists them: `(path, branch)`.
    fn slot_trees(&self) -> Vec<(PathBuf, Option<String>)> {
        let trees = Repo::new(&self.f.repo).worktrees().unwrap();
        trees
            .into_iter()
            .skip(1)
            .map(|t| (t.path, t.branch))
            .collect()
    }

    fn assert_all_detached(&self) {
        let trees = self.slot_trees();
        assert_eq!(trees.len(), 2, "{trees:?}");
        assert!(
            trees.iter().all(|(_, branch)| branch.is_none()),
            "a slot at rest holds a branch: {trees:?}"
        );
    }

    fn tip(&self, branch: &str) -> String {
        git(&self.f.repo, &["rev-parse", branch])
    }
}

fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_new_branch_gets_a_commit_in_the_local_repository_and_the_slot_is_detached_again() {
    let mut rig = Rig::new();
    let base = rig.tip("main");
    let (result, made) = rig.run(("main", "topic"), WRITE);
    assert_eq!(
        result.push(),
        "committed",
        "not \"pushed\": nothing leaves the machine"
    );
    let sha = result.get("sha").to_string();
    assert_eq!(result.get("branch"), "topic");
    assert_eq!(
        rig.tip("topic"),
        sha,
        "the commit is on the branch in the repository"
    );
    assert_eq!(git(&rig.f.repo, &["rev-parse", "topic~1"]), base);
    assert_eq!(
        git(&rig.f.repo, &["log", "-1", "--format=%an <%ae>", "topic"]),
        "Pool Test <pool-test@localhost>"
    );
    assert!(git(&rig.f.repo, &["log", "-1", "--format=%B", "topic"]).contains("Pool-Run: slot="));
    assert_eq!(
        git(&rig.f.repo, &["show", "--stat", "--format=", "topic"])
            .matches("standin.txt")
            .count(),
        1
    );
    assert!(
        result.push_line().starts_with("committed topic "),
        "{}",
        result.push_line()
    );
    rig.assert_all_detached();
    // The primary checkout was never touched.
    assert_eq!(
        git(&rig.f.repo, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main"
    );
    assert_eq!(rig.tip("main"), base);
    assert_eq!(git(&rig.f.repo, &["status", "--short"]), "");
    assert!(
        rig.checkout(&made).join("build/warm.txt").is_file(),
        "the warm build directory is kept"
    );
    assert!(
        !git(&rig.f.repo, &["ls-tree", "-r", "--name-only", "topic"]).contains("build/"),
        "and never committed"
    );
}

#[test]
fn continuing_a_branch_puts_the_next_commit_on_top_and_releases_it_again() {
    let mut rig = Rig::new();
    let (first, _) = rig.run(("main", "topic"), WRITE);
    let (second, _) = rig.run(("topic", "topic"), WRITE);
    assert_eq!(second.push(), "committed");
    assert_eq!(
        second.get("start"),
        first.get("sha"),
        "it started at the branch tip"
    );
    assert_eq!(rig.tip("topic"), second.get("sha"));
    assert_eq!(
        git(&rig.f.repo, &["rev-parse", "topic~1"]),
        first.get("sha")
    );
    rig.assert_all_detached();
    // Released: the primary checkout can take the branch and give it back.
    git(&rig.f.repo, &["checkout", "-q", "topic"]);
    git(&rig.f.repo, &["checkout", "-q", "main"]);
}

#[test]
fn a_stack_starts_a_new_branch_at_the_tip_of_another_and_leaves_that_one_alone() {
    let mut rig = Rig::new();
    let (first, _) = rig.run(("main", "topic"), WRITE);
    let (stacked, _) = rig.run(("topic", "topic-2"), WRITE);
    assert_eq!(stacked.get("start"), first.get("sha"));
    assert_eq!(
        git(&rig.f.repo, &["rev-parse", "topic-2~1"]),
        first.get("sha")
    );
    assert_eq!(
        rig.tip("topic"),
        first.get("sha"),
        "the base branch did not move"
    );
    rig.assert_all_detached();
}

#[test]
fn a_job_with_nothing_to_commit_makes_no_branch() {
    let mut rig = Rig::new();
    let (result, _) = rig.run(("main", "topic"), NOTHING);
    assert_eq!(result.push(), "no_changes");
    assert_eq!(
        git(&rig.f.repo, &["branch", "--list", "topic"]),
        "",
        "no empty branch is left behind"
    );
    rig.assert_all_detached();
}

#[test]
fn nothing_changes_for_a_continued_branch_that_the_job_leaves_alone() {
    let mut rig = Rig::new();
    let (first, _) = rig.run(("main", "topic"), WRITE);
    let (second, _) = rig.run(("topic", "topic"), NOTHING);
    assert_eq!(second.push(), "no_changes");
    assert_eq!(rig.tip("topic"), first.get("sha"), "the branch is intact");
    rig.assert_all_detached();
}

#[test]
fn a_branch_that_appeared_after_the_decision_is_refused_by_the_runner_and_never_reset() {
    let mut rig = Rig::new();
    let (task, made) = {
        // Decide while "topic" does not exist, then make it before the job runs.
        let task = rig.task();
        let mut plan = dispatch::plan_job(
            &rig.config,
            "wt",
            (&task, "subject", ""),
            &DispatchFlags {
                model: "standin".into(),
                from: Some("main".into()),
                branch: Some("topic".into()),
                with: Vec::new(),
                budget: Some(120),
                allow_default_branch: false,
                read_only: false,
                reasoning: None,
            },
            DEFAULT_RULES.into(),
            "0123456789ab".into(),
        )
        .unwrap();
        plan.harness_cmd = WRITE.into();
        let checkout = local_branch::prepare(&Repo::new(&rig.f.repo), &plan.refs, false).unwrap();
        plan.checkout = Some(checkout);
        git(&rig.f.repo, &["branch", "topic", "main~1"]);
        let token = Secret::new(String::new());
        let progress = Progress::silent();
        let launch = Launch {
            config: &rig.config,
            plan: &plan,
            token: &token,
            start: "main",
            progress: &progress,
            ready_timeout: None,
            reset_base: None,
        };
        let made = work::submit(&rig.space, &rig.provider, &rig.id, &launch, &pacing()).unwrap();
        (task, made)
    };
    let line = rig.wait(&task);
    assert_eq!(line.state, "finished");
    assert_eq!(rig.result(&made).push(), "branch_exists");
    assert_eq!(
        rig.tip("topic"),
        git(&rig.f.repo, &["rev-parse", "main~1"]),
        "the branch was not moved"
    );
    rig.assert_all_detached();
}

#[test]
fn a_job_killed_by_its_budget_leaves_the_slot_detached_and_the_branch_intact() {
    let mut rig = Rig::new();
    let (first, _) = rig.run(("main", "topic"), WRITE);
    let (task, made) = rig.dispatch(("topic", "topic"), "sleep 30", 2).unwrap();
    until("the slot to hold the branch", || {
        rig.slot_trees()
            .iter()
            .any(|(_, b)| b.as_deref() == Some("topic"))
    });
    let line = rig.wait(&task);
    assert_eq!(line.state, "finished", "{line:?}");
    let result = rig.result(&made);
    assert!(
        matches!(result.get("agent_rc"), "124" | "137"),
        "{result:?}"
    );
    assert_eq!(result.push(), "no_changes");
    assert_eq!(rig.tip("topic"), first.get("sha"));
    rig.assert_all_detached();
}

#[test]
fn a_slot_stopped_while_it_holds_a_branch_releases_it_and_ends_the_job() {
    let mut rig = Rig::new();
    rig.run(("main", "topic"), WRITE);
    let (_task, made) = rig.dispatch(("topic", "topic"), "sleep 60", 600).unwrap();
    until("the slot to hold the branch", || {
        rig.slot_trees()
            .iter()
            .any(|(_, b)| b.as_deref() == Some("topic"))
    });
    rig.provider.stop(&made.machine).unwrap();
    assert!(
        !rig.provider
            .machine(&made.machine)
            .unwrap()
            .state
            .eq("started")
    );
    rig.assert_all_detached();
    git(&rig.f.repo, &["checkout", "-q", "topic"]);
    git(&rig.f.repo, &["checkout", "-q", "main"]);
    let record = rig.provider.machine(&made.machine).unwrap();
    assert_eq!(record.state, "stopped");
    // The hour-long sleep of the harness is gone with the slot.
    let listing = proc::listing().unwrap();
    let dir = rig.root.join(&made.machine);
    assert!(proc::slot_pids(&dir, &listing).is_empty());
}

#[test]
fn freeing_a_slot_whose_boot_died_with_the_branch_checked_out_detaches_it() {
    let mut rig = Rig::new();
    rig.run(("main", "topic"), WRITE);
    let (_task, made) = rig.dispatch(("topic", "topic"), "sleep 60", 600).unwrap();
    until("the slot to hold the branch", || {
        rig.slot_trees()
            .iter()
            .any(|(_, b)| b.as_deref() == Some("topic"))
    });
    rig.provider.stop(&made.machine).unwrap();
    // Put the branch back on the slot by hand, as a runner killed hard would leave it.
    git(&rig.checkout(&made), &["checkout", "-q", "topic"]);
    assert!(
        rig.slot_trees()
            .iter()
            .any(|(_, b)| b.as_deref() == Some("topic"))
    );
    rig.provider.unclaim(&made.machine).unwrap();
    rig.assert_all_detached();
}

#[test]
fn two_dispatches_at_once_land_on_different_slots() {
    let mut rig = Rig::new();
    let (t1, a) = rig
        .dispatch(("main", "one"), "sleep 1; echo a >> standin.txt", 120)
        .unwrap();
    let (t2, b) = rig
        .dispatch(("main", "two"), "sleep 1; echo b >> standin.txt", 120)
        .unwrap();
    assert_ne!(a.machine, b.machine);
    let third = rig
        .dispatch(("main", "three"), WRITE, 120)
        .unwrap_err()
        .to_string();
    assert!(third.contains("no free slot"), "{third}");
    rig.wait(&t1);
    rig.wait(&t2);
    assert_eq!(
        git(&rig.f.repo, &["branch", "--list", "one", "two"])
            .lines()
            .count(),
        2
    );
    rig.assert_all_detached();
}

#[test]
fn the_refusals_come_before_a_slot_is_claimed() {
    let mut rig = Rig::new();
    rig.run(("main", "topic"), WRITE);
    for (refs, wanted) in [
        (("main", "topic"), "already exists"),
        (("main", "main"), "default branch"),
        (("nosuchref", "fresh"), "not a commit or branch"),
        (("fresh", "fresh"), "does not exist"),
    ] {
        let said = rig.dispatch(refs, WRITE, 120).unwrap_err().to_string();
        assert!(said.contains(wanted), "{refs:?}: {said}");
    }
    let held = rig.f.dir.path().join("held");
    git(
        &rig.f.repo,
        &["worktree", "add", "-q", held.to_str().unwrap(), "topic"],
    );
    let said = rig
        .dispatch(("topic", "topic"), WRITE, 120)
        .unwrap_err()
        .to_string();
    assert!(
        said.contains("checked out in") && said.contains("held"),
        "{said}"
    );
    let free = run::pool_status(&rig.provider, &rig.id).unwrap();
    assert!(
        free.iter().all(|s| s.state == "free"),
        "no slot was claimed: {free:?}"
    );
}

#[test]
fn scale_down_style_destroy_takes_the_worktree_with_it_and_every_branch_stays() {
    let mut rig = Rig::new();
    rig.run(("main", "topic"), WRITE);
    let slots = run::pool_status(&rig.provider, &rig.id).unwrap();
    rig.provider.destroy(&slots[1].id).unwrap();
    assert_eq!(rig.slot_trees().len(), 1);
    run::destroy_slots(&rig.provider, &rig.id, &run::DestroyTarget::All).unwrap();
    assert_eq!(rig.slot_trees().len(), 0);
    assert!(
        !rig.f.repo.join(".git/worktrees").exists(),
        "no stale entry"
    );
    assert_eq!(
        git(&rig.f.repo, &["branch", "--list", "topic", "main"])
            .lines()
            .count(),
        2
    );
    assert!(!rig.root.exists());
}

#[test]
fn a_read_only_job_in_a_worktree_slot_touches_no_branch_and_reports_an_unchanged_tree() {
    let mut rig = Rig::new();
    let (branches, main) = (rig.branches(), rig.tip("main"));
    let (result, made) = rig.run_read_only("main", NOTHING);
    assert!(made.read_only);
    assert_eq!(made.branch, "");
    assert_eq!(result.push(), "read_only_unchanged");
    assert_eq!(result.get("start"), main);
    assert_eq!(rig.branches(), branches, "no branch was made or moved");
    assert_eq!(git(&rig.checkout(&made), &["status", "--porcelain"]), "");
    rig.assert_all_detached();
    assert_eq!(git(&rig.f.repo, &["status", "--short"]), "");
}

#[test]
fn a_read_only_job_in_a_worktree_slot_discards_what_it_changed_and_says_what() {
    let mut rig = Rig::new();
    let branches = rig.branches();
    let (result, made) = rig.run_read_only("main", "echo more >> a.txt; echo n > new.txt");
    assert_eq!(result.push(), "read_only_discarded");
    assert_eq!(result.get("discarded_count"), "2");
    assert_eq!(result.discarded_paths(), ["a.txt", "new.txt"]);
    let slot = rig.checkout(&made);
    assert_eq!(git(&slot, &["status", "--porcelain"]), "");
    assert!(!slot.join("new.txt").exists());
    assert_eq!(rig.branches(), branches, "still no branch");
    rig.assert_all_detached();
    // The slot is clean for the next job, which can be a normal one.
    let (next, _) = rig.run(("main", "topic"), WRITE);
    assert_eq!(next.push(), "committed");
}

#[test]
fn a_read_only_job_can_start_from_a_commit_or_another_branch_and_the_branch_checks_do_not_apply() {
    let mut rig = Rig::new();
    let first = git(&rig.f.repo, &["rev-parse", "main~1"]);
    let (result, _) = rig.run_read_only(&first, NOTHING);
    assert_eq!(result.get("start"), first);
    git(&rig.f.repo, &["branch", "side", "main"]);
    let (result, _) = rig.run_read_only("side", NOTHING);
    assert_eq!(result.get("start"), rig.tip("side"));
    // The primary checkout's own branch and the default branch are fine to read.
    assert_eq!(
        rig.run_read_only("main", NOTHING).0.push(),
        "read_only_unchanged"
    );
    let missing = rig.dispatch_to("no-such-ref", None, NOTHING, 60);
    assert!(
        missing
            .unwrap_err()
            .to_string()
            .contains("not a commit or branch")
    );
}

#[test]
fn a_shared_slot_keeps_dynamic_target_worktrees_and_removes_them_on_destroy() {
    let rig = Rig::new();
    let second = fixture();
    let slot = rig.provider.list().unwrap().remove(0);
    let target = |id: &str, source: &std::path::Path, key: char| dispatch::target::JobTarget {
        id: id.into(),
        source: source.to_string_lossy().into_owned(),
        cache_key: key.to_string().repeat(64),
        local_source: Some(source.to_string_lossy().into_owned()),
        seed: Vec::new(),
        requires: Vec::new(),
        build_dir: None,
    };
    let a = target("a", &rig.f.repo, 'a');
    let b = target("b", &second.repo, 'b');
    let start_a = git(&rig.f.repo, &["rev-parse", "HEAD"]);
    let start_b = git(&second.repo, &["rev-parse", "HEAD"]);
    rig.provider
        .prepare_target_checkout(&slot.id, &a, &start_a)
        .unwrap();
    rig.provider
        .prepare_target_checkout(&slot.id, &b, &start_b)
        .unwrap();
    rig.provider
        .prepare_target_checkout(&slot.id, &a, &start_a)
        .unwrap();
    let caches = rig.root.join(&slot.id).join("targets");
    assert_eq!(
        Repo::new(&caches.join(&a.cache_key).join("repo"))
            .common_dir()
            .unwrap(),
        Repo::new(&rig.f.repo).common_dir().unwrap()
    );
    assert_eq!(
        Repo::new(&caches.join(&b.cache_key).join("repo"))
            .common_dir()
            .unwrap(),
        Repo::new(&second.repo).common_dir().unwrap()
    );
    rig.provider.destroy(&slot.id).unwrap();
    assert_eq!(Repo::new(&second.repo).worktrees().unwrap().len(), 1);
    assert!(!caches.exists());
}
