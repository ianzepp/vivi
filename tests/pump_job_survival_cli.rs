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
use vivi::mailspace::{Mailspace, SendRequest};
use vivi::pump::flow::index::{FLOW_VERSION_NUMBER, RunRow, StepState, run_state};
use vivi::pump::flow::mail::{MailWriter, MailspacePort};
use vivi::pump::flow::need_branch;
use vivi::pump::jobs::{self, JobSpec, JobStatus};
use vivi::pump::state::State;
use vivi::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};

const WORKERS: &[(&str, &str, &str)] = &[
    ("planner-board", "planner:board", "pump-planner"),
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
id=$(sed -n 's/^pump-job-id: //p' "$brief" | head -n 1)
kind=$(sed -n 's/^pump-job-kind: //p' "$brief" | head -n 1)
printf '%s %s\n' "$kind" "$id" > "$VIVI_TEST_MARKERS/$id.started"
while [ ! -f "$VIVI_TEST_MARKERS/release-workers" ]; do sleep 0.1; done
case "$kind" in
  planner:board)
    printf '%s\n' 'PUMP-PLAN-BEGIN' '{"need":"planner-survival","verdict":"hold","reason":"recovered once"}' 'PUMP-PLAN-END'
    ;;
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
    assert_eq!(
        fs::read_to_string(rig.home.join("plans/survival-plan.json")).unwrap(),
        "{\"need\":\"planner-survival\",\"verdict\":\"hold\",\"reason\":\"recovered once\"}"
    );
    assert_eq!(
        fs::read_to_string(rig.home.join("plans/survival.out"))
            .unwrap()
            .matches("PUMP-PLAN-BEGIN")
            .count(),
        1,
        "planner receipt was materialized more than once"
    );
    for (id, _, _) in WORKERS {
        let job = &final_state.jobs[*id];
        assert_eq!(job.status, JobStatus::Finished, "{id}");
        let receipt = &job.outcome.as_ref().unwrap().receipt;
        let marker = match job.kind.as_str() {
            "planner:board" => "PUMP-PLAN-BEGIN",
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
    let seeded = seed_merge_run(&rig);
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

struct SeededMerge {
    need: String,
    step_task: String,
    need_tip: String,
}

/// A need with an open pump record and a live build run whose `merge` step
/// is pending, over a need branch one commit ahead of main on the remote.
fn seed_merge_run(rig: &Rig) -> SeededMerge {
    let mailspace = Mailspace::discover(Some(&rig.project)).unwrap();
    let need = mailspace
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
        .handle;
    let mut port = MailspacePort {
        mailspace,
        pump_identity: String::from("mind"),
    };
    MailWriter::open_record(&mut port, &need, "kind: build").unwrap();

    let branch = need_branch::branch(&need);
    git(&rig.seed, &["checkout", "-q", "-B", "need-work", "main"]);
    let base = git_output(&rig.seed, &["rev-parse", "HEAD"]);
    fs::write(rig.seed.join("need.txt"), "the finished work\n").unwrap();
    git(&rig.seed, &["add", "need.txt"]);
    git(&rig.seed, &["commit", "-q", "-m", "finished work"]);
    let need_tip = git_output(&rig.seed, &["rev-parse", "HEAD"]);
    git(
        &rig.seed,
        &[
            "push",
            "-q",
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
    let run = RunRow {
        need: need.clone(),
        run: 1,
        flow: String::from("build"),
        flow_version: FLOW_VERSION_NUMBER,
        kind: String::from("build"),
        entry: String::from("rework1"),
        state: String::from(run_state::STARTING),
    };
    store.insert_run(&run).unwrap();
    let sentinel = port
        .send_step(
            "mind",
            "mind",
            &format!("{need} r1 sentinel"),
            &render(&TaskFront::sentinel(&need, 1), "").unwrap(),
            &[],
        )
        .unwrap();
    store
        .insert_run_step(&run, &run.step("sentinel", &sentinel, StepState::Running))
        .unwrap();
    let step_task = port
        .send_step(
            "mind",
            "mind",
            &format!("{need} r1 merge"),
            &render(&TaskFront::step(&need, 1, "merge"), "").unwrap(),
            &[],
        )
        .unwrap();
    store
        .insert_run_step(&run, &run.step("merge", &step_task, StepState::Pending))
        .unwrap();
    port.bind(&need, &[sentinel, step_task.clone()]).unwrap();
    store.set_run_state(&need, 1, run_state::LIVE).unwrap();
    store
        .upsert_need_branch(&NeedBranchRow {
            need: need.clone(),
            branch,
            base: Some(base),
            status: String::from("live"),
            run: 1,
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
        spec.read_only = matches!(*kind, "planner:board" | "cadence" | "digest");
        if *kind == "hand" {
            spec.branch = Some(String::from("vivi/unit/hand-survival-a1"));
        }
        if *kind == "planner:board" {
            spec.payload = serde_json::json!({
                "planner_output": {
                    "out": "plans/survival.out",
                    "plan": "plans/survival-plan.json",
                }
            });
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
    matches!(kind, "planner:board" | "cadence" | "digest")
}

fn applied(rig: &Rig) -> bool {
    let state = load_state(rig);
    WORKERS.iter().all(|(id, _, _)| {
        state.jobs.get(*id).is_some_and(|job| {
            job.status == JobStatus::Finished
                && (!pump_applies(&job.kind) || job.applied_at.is_some())
        })
    }) && rig.home.join("plans/survival-plan.json").is_file()
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
    for role in ["hand", "pump-planner", "merge", "mind", "digest"] {
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
            command_report(["pump", "status", "--project", path(&rig.project)])
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
