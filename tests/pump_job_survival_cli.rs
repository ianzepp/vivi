//! A real local pool outlives its pump. The pump is killed after all workers
//! started, then restarted to reattach and apply each role receipt once.

use std::fs::{self, OpenOptions};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use vivi::letter::render;
use vivi::letter::work::{NeedFront, TaskFront};
use vivi::mailspace::{Mailspace, PumpState, SendRequest};
use vivi::pump::flow::index::{FLOW_VERSION_NUMBER, RunRow, StepState, run_state};
use vivi::pump::flow::mail::{MailWriter, MailspacePort};
use vivi::pump::flow::need_branch;
use vivi::pump::flow::prune::PRUNE_FAILED;
use vivi::pump::flow::tick::RUN_ERROR;
use vivi::pump::jobs::{self, JobSpec, JobStatus};
use vivi::pump::state::State;
use vivi::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};

const WORKERS: &[(&str, &str, &str)] = &[
    ("cadence-survival", "cadence", "mind"),
    ("digest-survival", "digest", "digest"),
    ("hand-survival", "hand", "hand"),
];
/// Every slot is a git worktree made by `pool create` (about 1.3 s each), so a
/// test asks for the few slots it needs, not a large pool.
const WORKER_SLOTS: usize = WORKERS.len() + 2;
const MERGE_SLOTS: usize = 2;
const TEST_HARNESS: &str = r#"#!/bin/sh
set -eu
brief="$1"
id=$(sed -n 's/^id = "\(.*\)"$/\1/p' "$brief" | head -n 1)
kind=$(sed -n 's/^kind = "\(.*\)"$/\1/p' "$brief" | head -n 1)
printf '%s %s\n' "$kind" "$id" > "$VIVI_TEST_MARKERS/$id.started"
while [ ! -f "$VIVI_TEST_MARKERS/release-workers" ]; do sleep 0.1; done
case "$kind" in
  cadence)
    printf 'cadence survived\n'
    ;;
  digest)
    printf 'digest survived\n'
    ;;
  hand)
    printf '%s\n' '+++' 'answer = "built"' 'change = []' 'deviations = []' 'remaining = []' '+++' 'hand survived'
    ;;
  *)
    printf 'unexpected worker kind: %s\n' "$kind" >&2
    exit 2
    ;;
esac
printf 'done\n' > "$VIVI_TEST_MARKERS/$id.done"
"#;

struct Rig {
    _dir: tempfile::TempDir,
    project: PathBuf,
    home: PathBuf,
    marker_dir: PathBuf,
    child_log: PathBuf,
    tip: String,
    bare: PathBuf,
    seed: PathBuf,
}

struct PumpChild {
    child: Child,
    release_path: PathBuf,
}

impl Deref for PumpChild {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        &self.child
    }
}

impl DerefMut for PumpChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.child
    }
}

impl Drop for PumpChild {
    fn drop(&mut self) {
        let _ = fs::write(&self.release_path, "release\n");
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn local_pool_jobs_survive_sigkill_and_apply_once() {
    let rig = scaffold(WORKER_SLOTS);
    seed_state(&rig);

    let mut first = pump_child(&rig);
    let expected: Vec<String> = WORKERS.iter().map(|(id, _, _)| (*id).to_owned()).collect();
    let workers_started = wait_until(Duration::from_secs(30), || {
        expected
            .iter()
            .all(|id| rig.marker_dir.join(format!("{id}.started")).is_file())
            && dispatched(&rig)
    });
    if !workers_started {
        let pump_state = first.try_wait();
        let details = startup_diagnostics(&rig);
        let _ = first.kill();
        let _ = first.wait();
        let _ = fs::write(rig.marker_dir.join("release-workers"), "release\n");
        let saved = rig._dir.keep();
        panic!(
            "workers did not all start; pump={pump_state:?}; fixture={}\n{details}",
            saved.display()
        );
    }

    // The pump has saved every task handle before it dies. Only the pump is
    // killed; the local pool runner and its harness processes stay alive.
    first.kill().unwrap();
    let _ = first.wait().unwrap();
    fs::write(rig.marker_dir.join("release-workers"), "release\n").unwrap();
    assert!(
        wait_until(Duration::from_secs(20), || {
            expected
                .iter()
                .all(|id| rig.marker_dir.join(format!("{id}.done")).is_file())
        }),
        "one or more pool workers died with the pump"
    );
    let stopped_state = load_state(&rig);
    for (id, _, _) in WORKERS {
        let job = &stopped_state.jobs[*id];
        assert_eq!(job.status, JobStatus::Dispatched, "{id}");
        assert!(job.task.is_some(), "{id} did not save its task handle");
    }

    let mut restarted = pump_child(&rig);
    assert!(
        wait_until(Duration::from_secs(40), || applied(&rig)),
        "restart did not apply all receipts; pump status: {}",
        fs::read_to_string(rig.home.join("status.txt")).unwrap_or_default()
    );
    stop_pump(&rig, &mut restarted);

    let final_state = load_state(&rig);
    for (id, _, _) in WORKERS {
        let job = &final_state.jobs[*id];
        assert_eq!(job.status, JobStatus::Finished, "{id}");
        let receipt = &job.outcome.as_ref().unwrap().receipt;
        let marker = match job.kind.as_str() {
            "cadence" => "cadence survived",
            "digest" => "digest survived",
            "hand" => "hand survived",
            other => panic!("unexpected seeded job kind {other}"),
        };
        assert!(
            receipt.contains(marker),
            "{id} has the wrong saved receipt: {receipt}"
        );
        if pump_applies(&job.kind) {
            assert!(job.applied_at.is_some(), "{id} receipt was not applied");
        }
        assert_eq!(
            task_occurrences(&rig.project, &format!("[pump-job:{id}]"), role_for(id)),
            1,
            "restart sent a duplicate task for {id}"
        );
    }
}

/// A publish-bearing flow job outlives its pump. A build run waits at the
/// `merge` step; the pump starts the no-model merge-main job and is killed
/// once the pool holds it. The restarted pump collects the finished job: main
/// moves by exactly one merge commit and the flow step advances once.
#[test]
fn flow_merge_main_job_survives_sigkill_and_applies_once() {
    let rig = scaffold(MERGE_SLOTS);
    let seeded = seed_merge_run(&rig, None, 1);
    let main_before = git_output(&rig.seed, &["rev-parse", "main"]);

    let mut first = pump_child(&rig);
    let held = wait_until(Duration::from_secs(30), || {
        merge_job(&rig).is_some_and(|job| job.status == JobStatus::Dispatched && job.task.is_some())
    });
    if !held {
        let details = startup_diagnostics(&rig);
        let _ = first.kill();
        let _ = first.wait();
        let saved = rig._dir.keep();
        panic!(
            "merge-main job was not dispatched; fixture={}\n{details}",
            saved.display()
        );
    }
    first.kill().unwrap();
    let _ = first.wait().unwrap();

    let stopped = merge_job(&rig).expect("merge-main job after the kill");
    assert_eq!(stopped.status, JobStatus::Dispatched, "{}", stopped.id);
    assert!(
        stopped.task.is_some(),
        "{} lost its task handle",
        stopped.id
    );
    assert_eq!(
        stopped.publish_ref.as_deref(),
        Some("refs/heads/main"),
        "the job is not publish-bearing"
    );
    assert_eq!(step_state(&rig, &seeded.step_task), StepState::Running);

    let mut restarted = pump_child(&rig);
    let landed = wait_until(Duration::from_secs(40), || {
        open_store(&rig)
            .run_row(&seeded.need, 1)
            .is_ok_and(|run| run.state == run_state::DONE)
    });
    if !landed {
        let details = startup_diagnostics(&rig);
        let _ = restarted.kill();
        let _ = restarted.wait();
        let saved = rig._dir.keep();
        panic!(
            "restart did not land the run; fixture={}\n{details}",
            saved.display()
        );
    }
    stop_pump(&rig, &mut restarted);

    // The publish applied once: one merge commit joins the old main and the
    // need tip, and nothing else moved main.
    git(
        &rig.seed,
        &[
            "fetch",
            "-q",
            path(&rig.bare),
            "main:refs/remotes/bare/main",
        ],
    );
    let range = format!("{main_before}..bare/main");
    assert_eq!(
        git_output(&rig.seed, &["rev-list", "--count", &range]),
        "2",
        "main gained something other than the need commit and one merge"
    );
    assert_eq!(
        git_output(&rig.seed, &["rev-list", "--count", "--merges", &range]),
        "1"
    );
    let parents = git_output(&rig.seed, &["rev-parse", "bare/main^1", "bare/main^2"]);
    assert_eq!(parents, format!("{main_before}\n{}", seeded.need_tip));

    // The flow step advanced once: one merge row, done, one task, one job.
    let store = open_store(&rig);
    let merge_rows: Vec<_> = store
        .steps_of(&seeded.need, 1)
        .unwrap()
        .into_iter()
        .filter(|row| row.node == "merge")
        .collect();
    assert_eq!(merge_rows.len(), 1, "{merge_rows:?}");
    assert_eq!(merge_rows[0].state, StepState::Done);
    assert_eq!(merge_rows[0].verdict.as_deref(), Some("merged"));
    assert_eq!(store.last_attempt(&seeded.step_task).unwrap(), 1);
    assert_eq!(
        task_occurrences(&rig.project, &format!("{} r1 merge", seeded.need), "mind"),
        1,
        "restart sent a duplicate merge step"
    );
    let finished = merge_job(&rig).unwrap();
    assert_eq!(finished.status, JobStatus::Finished, "{}", finished.id);
    assert_eq!(
        finished
            .outcome
            .as_ref()
            .unwrap()
            .receipt
            .matches("runner publish: ")
            .count(),
        1
    );
}

/// A reopened need lands twice over the same reused need branch: run 1 and
/// run 2 each publish, close `landed`, and enqueue their own scoped cleanup
/// job, and each applied cleanup really deletes the branch from the bare
/// remote before the pump is stopped and restarted between the landings.
#[test]
fn a_reopened_need_lands_and_prunes_its_branch_twice() {
    let rig = scaffold(MERGE_SLOTS);
    let run1 = seed_merge_run(&rig, None, 1);
    let need = run1.need.clone();
    let delete_ref = format!("refs/heads/{}", need_branch::branch(&need));
    assert_eq!(
        remote_tip(&rig, &delete_ref).as_deref(),
        Some(run1.need_tip.as_str()),
        "the seeded need branch is missing from the bare remote"
    );

    // Run 1 lands and its cleanup job is finished, applied and receipted.
    let mut first = pump_child(&rig);
    let landed1 = wait_until(Duration::from_secs(90), || {
        run_done(&rig, &need, 1) && cleanup_jobs(&rig, &delete_ref).iter().any(cleanup_deleted)
    });
    if !landed1 {
        let details = startup_diagnostics(&rig);
        let _ = first.kill();
        let _ = first.wait();
        let saved = rig._dir.keep();
        panic!(
            "run 1 did not land and clean up; fixture={}\n{details}",
            saved.display()
        );
    }
    assert!(
        remote_tip(&rig, &delete_ref).is_none(),
        "run 1 left {delete_ref} on the bare remote"
    );
    stop_pump(&rig, &mut first);

    // The real lifecycle reopens the same need; run 2 reuses the branch name.
    assert_success(&vivi([
        "need",
        "reopen",
        &need,
        "--for",
        "operator",
        "--project",
        path(&rig.project),
    ]));
    let run2 = seed_merge_run(&rig, Some(&need), 2);
    assert_eq!(run2.need, need, "run 2 seeded a replacement need");
    assert_ne!(run2.need_tip, run1.need_tip, "run 2 reused the run-1 tip");
    assert_eq!(
        remote_tip(&rig, &delete_ref).as_deref(),
        Some(run2.need_tip.as_str()),
        "the reused branch does not hold the run-2 candidate"
    );

    // The clean stop left the durable control mode stopped; the real
    // lifecycle re-enables the pump before the service child runs again.
    assert_success(&vivi(["pump", "start", "--project", path(&rig.project)]));

    // Run 2 lands and its own cleanup job deletes the reused branch again.
    let mut second = pump_child(&rig);
    let landed2 = wait_until(Duration::from_secs(90), || {
        let cleanups = cleanup_jobs(&rig, &delete_ref);
        run_done(&rig, &need, 2) && cleanups.len() == 2 && cleanups.iter().all(cleanup_deleted)
    });
    if !landed2 {
        let details = startup_diagnostics(&rig);
        let _ = second.kill();
        let _ = second.wait();
        let saved = rig._dir.keep();
        panic!(
            "run 2 did not land and clean up; fixture={}\n{details}",
            saved.display()
        );
    }
    assert!(
        remote_tip(&rig, &delete_ref).is_none(),
        "run 2 left {delete_ref} on the bare remote"
    );

    // Further production ticks run after the second cleanup without adding
    // to or disturbing the settled need-branch cleanup identity set.
    let settled_at_cleanup = cleanup_jobs(&rig, &delete_ref);
    let cycles_at_cleanup = completed_cycles(&rig.home);
    assert!(
        wait_until(Duration::from_secs(30), || completed_cycles(&rig.home)
            .iter()
            .filter(|id| !cycles_at_cleanup.contains(*id))
            .count()
            >= 2),
        "no additional pump tick followed the run-2 cleanup"
    );
    stop_pump(&rig, &mut second);

    let settled = cleanup_jobs(&rig, &delete_ref);
    let namespaced = [
        format!(
            "prune-{need}-r1-{}",
            need_branch::branch(&need).replace('/', "-")
        ),
        format!(
            "prune-{need}-r2-{}",
            need_branch::branch(&need).replace('/', "-")
        ),
    ];
    assert_eq!(
        settled
            .iter()
            .map(|job| job.id.as_str())
            .collect::<Vec<_>>(),
        namespaced.iter().map(String::as_str).collect::<Vec<_>>(),
        "the need-branch cleanup identity set is not one job per landing run"
    );

    // Both runs are done and the history is two distinct closed records.
    let store = open_store(&rig);
    for run in 1..=2 {
        assert_eq!(store.run_row(&need, run).unwrap().state, run_state::DONE);
    }
    let history = Mailspace::discover(Some(&rig.project))
        .unwrap()
        .pump_history(&need)
        .unwrap();
    assert_eq!(history.len(), 2, "{history:?}");
    let distinct: std::collections::BTreeSet<_> =
        history.iter().map(|record| &record.handle).collect();
    assert_eq!(distinct.len(), 2, "records are not distinct: {history:?}");
    for record in &history {
        assert_eq!(record.status, PumpState::Done, "{}", record.handle);
        assert_eq!(
            record.outcome.as_deref(),
            Some("landed"),
            "{}",
            record.handle
        );
    }

    // Each cleanup keeps its own identity and receipt, the run-1 job across
    // the restart, with every delete matching its own seeded tip.
    for (job, seeded_tip) in settled.iter().zip([&run1.need_tip, &run2.need_tip]) {
        let tip = seeded_tip.as_str();
        assert_eq!(job.status, JobStatus::Finished, "{}", job.id);
        assert!(job.applied_at.is_some(), "{} was never applied", job.id);
        assert_eq!(job.from_ref.as_deref(), Some(tip), "{}", job.id);
        assert_eq!(job.delete_expect.as_deref(), Some(tip), "{}", job.id);
        let receipt = job
            .outcome
            .as_ref()
            .unwrap()
            .runner_delete()
            .unwrap_or_else(|| panic!("{} has no runner delete receipt", job.id));
        assert_eq!(receipt.reference, delete_ref, "{}", job.id);
        assert_eq!(receipt.expected, tip, "{}", job.id);
        assert_eq!(receipt.after, "absent", "{}", job.id);
        assert!(receipt.succeeded(), "{}: {receipt:?}", job.id);
    }

    // Neither landing raised a run or cleanup failure diagnostic.
    let raised: Vec<_> = store
        .exceptions()
        .unwrap()
        .into_iter()
        .map(|(_, kind, _, _)| kind)
        .collect();
    assert!(
        !raised
            .iter()
            .any(|kind| kind == RUN_ERROR || kind == PRUNE_FAILED),
        "failure diagnostics were raised: {raised:?}"
    );
}

struct SeededMerge {
    need: String,
    step_task: String,
    need_tip: String,
}

/// A need with an open pump record and a live build run whose `merge` step
/// is pending, over a need branch one commit ahead of the bare remote's
/// current main. With `existing_need`, reuse that exact need (the caller has
/// reopened it through the real lifecycle); with `None`, send a fresh need.
/// Each seeded run opens a fresh pump record and numbers its sentinel, merge
/// step, run rows and branch row with `run`.
fn seed_merge_run(rig: &Rig, existing_need: Option<&str>, run: u32) -> SeededMerge {
    let mailspace = Mailspace::discover(Some(&rig.project)).unwrap();
    let need = match existing_need {
        Some(need) => String::from(need),
        None => {
            mailspace
                .send(SendRequest {
                    from: "operator".into(),
                    to: vec!["mind".into()],
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: "merge survival".into(),
                    body: render(&NeedFront::new(vec!["it works".into()]), "").unwrap(),
                    role: "needs".into(),
                    kind: Some("need".into()),
                    reply_to: None,
                })
                .unwrap()
                .delivered
                .remove(0)
                .handle
        }
    };
    let mut port = MailspacePort {
        mailspace,
        pump_identity: String::from("mind"),
    };
    MailWriter::open_record(&mut port, &need, "").unwrap();

    let branch = need_branch::branch(&need);
    // Base the candidate on the bare remote's current main, not on stale
    // local main: an earlier run may have landed merges there.
    git(
        &rig.seed,
        &[
            "fetch",
            "-q",
            path(&rig.bare),
            "main:refs/remotes/bare/main",
        ],
    );
    git(
        &rig.seed,
        &[
            "checkout",
            "-q",
            "-B",
            "need-work",
            "refs/remotes/bare/main",
        ],
    );
    let base = git_output(&rig.seed, &["rev-parse", "refs/remotes/bare/main"]);
    fs::write(
        rig.seed.join("need.txt"),
        format!("the finished work, run {run}\n"),
    )
    .unwrap();
    git(&rig.seed, &["add", "need.txt"]);
    git(
        &rig.seed,
        &["commit", "-q", "-m", &format!("finished work r{run}")],
    );
    let need_tip = git_output(&rig.seed, &["rev-parse", "HEAD"]);
    git(
        &rig.seed,
        &[
            "push",
            "-q",
            "--force",
            path(&rig.bare),
            &format!("HEAD:refs/heads/{branch}"),
        ],
    );
    git(&rig.seed, &["checkout", "-q", "main"]);
    // The pool resolves the tip in the pump's checkout.
    git(&rig.home.join("repo"), &["fetch", "-q", "origin"]);

    let store = PumpStore::open(&rig.home.join(DB_FILENAME)).unwrap();
    // A live build run: the sentinel, and the pending `merge` step where the
    // verified build would have routed. (Run start is crate-private.)
    let run_row = RunRow {
        need: need.clone(),
        run,
        flow: String::from("build"),
        flow_version: FLOW_VERSION_NUMBER,
        kind: String::from("build"),
        entry: String::from("rework1"),
        state: String::from(run_state::STARTING),
    };
    store.insert_run(&run_row).unwrap();
    let sentinel = port
        .send_step(
            "mind",
            "mind",
            &format!("{need} r{run} sentinel"),
            &render(&TaskFront::sentinel(&need, run), "").unwrap(),
            &[],
        )
        .unwrap();
    store
        .insert_run_step(
            &run_row,
            &run_row.step("sentinel", &sentinel, StepState::Running),
        )
        .unwrap();
    let step_task = port
        .send_step(
            "mind",
            "mind",
            &format!("{need} r{run} merge"),
            &render(&TaskFront::step(&need, run, "merge"), "").unwrap(),
            &[],
        )
        .unwrap();
    store
        .insert_run_step(
            &run_row,
            &run_row.step("merge", &step_task, StepState::Pending),
        )
        .unwrap();
    port.bind(&need, &[sentinel, step_task.clone()]).unwrap();
    store.set_run_state(&need, run, run_state::LIVE).unwrap();
    store
        .upsert_need_branch(&NeedBranchRow {
            need: need.clone(),
            branch,
            base: Some(base),
            status: String::from("live"),
            run,
            target: String::from("default"),
            priors: Vec::new(),
        })
        .unwrap();
    SeededMerge {
        need,
        step_task,
        need_tip,
    }
}

fn open_store(rig: &Rig) -> PumpStore {
    PumpStore::open_read_only(&rig.home.join(DB_FILENAME))
        .unwrap()
        .expect("pump.db")
}

fn merge_job(rig: &Rig) -> Option<jobs::PoolJob> {
    load_state(rig)
        .jobs
        .into_values()
        .find(|job| job.id.starts_with("merge-main-"))
}

fn step_state(rig: &Rig, task: &str) -> StepState {
    open_store(rig).step_by_task(task).unwrap().unwrap().state
}

/// The tip of one reference on the bare remote, or `None` when it is absent.
fn remote_tip(rig: &Rig, reference: &str) -> Option<String> {
    let listed = git_output(&rig.seed, &["ls-remote", path(&rig.bare), reference]);
    listed.split_whitespace().next().map(str::to_owned)
}

/// Whether one flow run row of `need` reached the done state.
fn run_done(rig: &Rig, need: &str, run: u32) -> bool {
    open_store(rig)
        .run_row(need, run)
        .is_ok_and(|row| row.state == run_state::DONE)
}

/// Every durable job deleting exactly `reference`, sorted by identity, so a
/// merge-artifact cleanup for another ref never masquerades as this one.
fn cleanup_jobs(rig: &Rig, reference: &str) -> Vec<jobs::PoolJob> {
    let mut found: Vec<_> = load_state(rig)
        .jobs
        .into_values()
        .filter(|job| job.delete_ref.as_deref() == Some(reference))
        .collect();
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}

/// Whether a cleanup job finished, was applied, and receipted the deletion.
fn cleanup_deleted(job: &jobs::PoolJob) -> bool {
    job.status == JobStatus::Finished
        && job.applied_at.is_some()
        && job
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.runner_delete())
            .is_some_and(|receipt| receipt.succeeded())
}

/// Completed tick ids from the pump home's monitor-cycles.jsonl.
fn completed_cycles(home: &Path) -> Vec<String> {
    let text = fs::read_to_string(home.join("monitor-cycles.jsonl")).unwrap_or_default();
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|cycle| cycle.get("ended_at").is_some_and(serde_json::Value::is_u64))
        .filter_map(|cycle| {
            cycle
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

fn scaffold(slots: usize) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let project = base.join("project");
    fs::create_dir(&project).unwrap();
    assert_success(&vivi(["mailspace", "init", "--project", path(&project)]));

    let bare = base.join("origin.git");
    let seed = base.join("seed");
    fs::create_dir(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    git(&seed, &["config", "user.name", "Survival Test"]);
    git(&seed, &["config", "user.email", "survival@localhost"]);
    fs::write(seed.join("README.md"), "survival fixture\n").unwrap();
    git(&seed, &["add", "README.md"]);
    git(&seed, &["commit", "-q", "-m", "fixture"]);
    git(&seed, &["clone", "-q", "--bare", ".", path(&bare)]);
    git(&seed, &["push", "-q", path(&bare), "main"]);
    git(&seed, &["push", "-q", path(&bare), "main:next"]);

    let home = project.join(".vivi/pump-home");
    fs::create_dir_all(&home).unwrap();
    let repo = home.join("repo");
    git(&base, &["clone", "-q", path(&bare), path(&repo)]);
    git(&repo, &["checkout", "-q", "-B", "next", "origin/next"]);
    let tip = git_output(&repo, &["rev-parse", "HEAD"]);
    let markers = base.join("markers");
    fs::create_dir(&markers).unwrap();
    let child_log = base.join("pump-child.log");
    let script = base.join("worker.sh");
    fs::write(&script, TEST_HARNESS).unwrap();

    let config_path = project.join(".vivi/mailspace.toml");
    let old = fs::read_to_string(&config_path).unwrap();
    let mut parsed: toml::Value = toml::from_str(&old).unwrap();
    // `mailspace init` writes a starter [pump]; this test supplies its own.
    parsed.as_table_mut().unwrap().remove("pump");
    let additions = format!(
        "\n[pump]\nhome = {}\nrepo = {}\nremote = \"origin\"\nremote_url = {}\nbranch = \"next\"\nmain = \"main\"\npromote = false\ntick = 1\ncommitter_name = \"Survival Test\"\ncommitter_email = \"survival@localhost\"\nfrom = \"mind\"\nto = \"hand\"\nreply_from = \"mind\"\n\n[pump.board]\nfrom = []\npull = false\n\n[pools.p]\nprovider = \"local\"\nslots = {}\n\n[pools.p.bands]\nP2 = [{{ model = \"standin\", reasoning = \"low\" }}]\n\n[pools.p.local]\nroot = {}\nworktree_of = {}\n\n[pools.p.harness]\nname = \"standin\"\nrun = \"bash '{}' {{brief}}\"\nmodels = [\"standin\"]\nenv = [\"VIVI_TEST_MARKERS\", \"VIVI_TEST_TIP\"]\n\n[pools.p.committer]\nname = \"Survival Test\"\nemail = \"survival@localhost\"\n",
        toml_string(&project.join(".vivi/pump-home")),
        toml_string(&repo),
        toml_string(&bare),
        slots,
        toml_string(&base.join("lanes")),
        toml_string(&repo),
        script.display(),
    );
    for (role, kind) in [
        ("mind", "mind"),
        ("hand", "hand"),
        ("pump-planner", "planner"),
        ("merge", "merge"),
        // Pump readiness requires an active auditor role on a pool.
        ("auditor", "auditor"),
        ("digest", "digest"),
    ] {
        ensure_role(&mut parsed, role, kind, "p", "P2");
    }
    let mut config = toml::to_string(&parsed).unwrap();
    config.push_str(&additions);
    fs::write(config_path, config).unwrap();
    let created = vivi(["pool", "create", "--pool", "p", "--project", path(&project)]);
    assert_success(&created);
    Rig {
        _dir: dir,
        project,
        home,
        marker_dir: markers,
        child_log,
        tip,
        bare,
        seed,
    }
}

fn ensure_role(config: &mut toml::Value, name: &str, kind: &str, pool: &str, model: &str) {
    let identities = config
        .get_mut("identities")
        .and_then(toml::Value::as_array_mut)
        .expect("mailspace init omitted identities array");
    let index = identities
        .iter()
        .position(|identity| identity.get("name").and_then(toml::Value::as_str) == Some(name));
    let index = if let Some(index) = index {
        index
    } else {
        identities.push(toml::Value::Table(toml::map::Map::new()));
        identities.len() - 1
    };
    let row = identities[index]
        .as_table_mut()
        .expect("identity row is not a table");
    row.insert("name".to_owned(), toml::Value::String(name.to_owned()));
    row.insert("kind".to_owned(), toml::Value::String(kind.to_owned()));
    row.insert("pool".to_owned(), toml::Value::String(pool.to_owned()));
    row.insert("model".to_owned(), toml::Value::String(model.to_owned()));
}

fn seed_state(rig: &Rig) {
    let mut st = State::default();
    for (id, kind, role) in WORKERS {
        let mut spec = JobSpec::new(
            *id,
            *kind,
            *role,
            format!("survival {kind}"),
            "survive SIGKILL",
        );
        spec.pool_candidates = vec![String::from("p")];
        spec.local_only = true;
        spec.target = Some(String::from("default"));
        spec.from_ref = Some(String::from("next"));
        spec.model = Some(String::from("standin"));
        spec.budget = 60;
        spec.read_only = matches!(*kind, "cadence" | "digest");
        if *kind == "hand" {
            spec.branch = Some(String::from("vivi/unit/hand-survival-a1"));
        }
        jobs::enqueue(&mut st, spec).unwrap();
    }
    st.extra.insert(
        String::from("cadence"),
        serde_json::json!({
            "runs": {
                "mind": {
                    "job": "cadence-survival",
                    "started_at": 1,
                    "read_only": true
                },
                "digest": {
                    "job": "digest-survival",
                    "started_at": 1,
                    "read_only": true
                }
            }
        }),
    );
    let store = PumpStore::open(&rig.home.join(DB_FILENAME)).unwrap();
    let mut store = store;
    store.save(&st).unwrap();
}

fn pump_child(rig: &Rig) -> PumpChild {
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rig.child_log)
        .unwrap();
    let stderr = stdout.try_clone().unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args([
            "pump",
            "run",
            "--service-child",
            "--project",
            path(&rig.project),
        ])
        .env("VIVI_TEST_MARKERS", &rig.marker_dir)
        .env("VIVI_TEST_TIP", &rig.tip)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    PumpChild {
        child,
        release_path: rig.marker_dir.join("release-workers"),
    }
}

fn stop_pump(rig: &Rig, child: &mut PumpChild) {
    let stopped = vivi(["pump", "stop", "--project", path(&rig.project)]);
    assert_success(&stopped);
    assert!(wait_until(Duration::from_secs(10), || child
        .try_wait()
        .unwrap()
        .is_some()));
}

fn dispatched(rig: &Rig) -> bool {
    let state = load_state(rig);
    WORKERS.iter().all(|(id, _, _)| {
        state
            .jobs
            .get(*id)
            .is_some_and(|job| job.status == JobStatus::Dispatched && job.task.is_some())
    })
}

/// Whether the bare pump still applies this kind's receipt. A hand job is a
/// flow slice job whose application to a flow step is proved tick-level on a
/// real mailspace by the e2e_gate_* tests; it still proves survival here:
/// dispatched, handle saved, receipt collected once.
fn pump_applies(kind: &str) -> bool {
    matches!(kind, "cadence" | "digest")
}

fn applied(rig: &Rig) -> bool {
    let state = load_state(rig);
    WORKERS.iter().all(|(id, _, _)| {
        state.jobs.get(*id).is_some_and(|job| {
            job.status == JobStatus::Finished
                && (!pump_applies(&job.kind) || job.applied_at.is_some())
        })
    })
}

fn load_state(rig: &Rig) -> State {
    PumpStore::open_read_only(&rig.home.join(DB_FILENAME))
        .unwrap()
        .expect("pump.db")
        .load()
        .unwrap()
}

fn startup_diagnostics(rig: &Rig) -> String {
    let state = load_state(rig);
    let jobs = state
        .jobs
        .values()
        .map(|job| {
            format!(
                "{} kind={} status={:?} task={:?} pool={:?}",
                job.id, job.kind, job.status, job.task, job.pool
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut tasks = Vec::new();
    for role in ["hand", "merge", "mind", "digest"] {
        tasks.push(command_report([
            "task",
            "list",
            "--for",
            role,
            "--status",
            "all",
            "--json",
            "--project",
            path(&rig.project),
        ]));
    }
    let receipts = state
        .jobs
        .values()
        .filter_map(|job| {
            job.task.as_deref().map(|task| {
                format!(
                    "{}:\n{}",
                    job.id,
                    command_report([
                        "task",
                        "show",
                        task,
                        "--latest",
                        "--project",
                        path(&rig.project)
                    ])
                )
            })
        })
        .collect::<Vec<_>>()
        .join("\n");
    [
        format!(
            "pump status:\n{}",
            fs::read_to_string(rig.home.join("status.txt")).unwrap_or_default()
        ),
        format!(
            "pool status:\n{}",
            command_report([
                "pool",
                "status",
                "--porcelain",
                "--project",
                path(&rig.project)
            ])
        ),
        format!("jobs:\n{jobs}"),
        format!("tasks:\n{}", tasks.join("\n")),
        format!("receipts:\n{receipts}"),
        format!(
            "mailspace config:\n{}",
            fs::read_to_string(rig.project.join(".vivi/mailspace.toml")).unwrap_or_default()
        ),
        format!(
            "state status:\n{}",
            fs::read_to_string(rig.home.join("status.txt")).unwrap_or_default()
        ),
        format!(
            "pump log:\n{}",
            fs::read_to_string(rig.home.join("log.txt")).unwrap_or_default()
        ),
        format!(
            "child log:\n{}",
            fs::read_to_string(&rig.child_log).unwrap_or_default()
        ),
    ]
    .join("\n\n")
}

fn command_report<const N: usize>(args: [&str; N]) -> String {
    let output = vivi(args);
    format!(
        "status={}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn task_occurrences(project: &Path, needle: &str, role: &str) -> usize {
    let output = vivi([
        "task",
        "list",
        "--for",
        role,
        "--status",
        "all",
        "--json",
        "--project",
        path(project),
    ]);
    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    count_task_subjects(&value, needle)
}

fn count_task_subjects(value: &serde_json::Value, needle: &str) -> usize {
    match value {
        serde_json::Value::Object(object) => {
            if object
                .get("handle")
                .and_then(serde_json::Value::as_str)
                .is_some()
                && object
                    .get("subject")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|subject| subject.contains(needle))
            {
                1
            } else {
                object
                    .values()
                    .map(|value| count_task_subjects(value, needle))
                    .sum()
            }
        }
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| count_task_subjects(value, needle))
            .sum(),
        _ => 0,
    }
}

fn role_for(id: &str) -> &'static str {
    WORKERS
        .iter()
        .find(|(job_id, _, _)| *job_id == id)
        .map_or("", |(_, _, role)| role)
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    condition()
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn vivi<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn toml_string(path: &Path) -> String {
    format!("{:?}", path.to_str().unwrap())
}
