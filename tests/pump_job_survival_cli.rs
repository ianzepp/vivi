//! A real local pool outlives its pump. The pump is killed after all workers
//! started, then restarted to reattach and apply each role receipt once.

use std::fs::{self, OpenOptions};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use vivi::pump::audit::AuditRun;
use vivi::pump::jobs::{self, JobSpec, JobStatus};
use vivi::pump::state::{GateRun, State, Status, Unit};
use vivi::pump::store::{DB_FILENAME, PumpStore};

const WORKERS: &[(&str, &str, &str)] = &[
    ("planner-board", "planner:board", "pump-planner"),
    ("merge-default-1", "merge", "merge"),
    ("gate-survival", "gate", "gate"),
    ("audit-survival", "audit", "auditor"),
    ("promote-survival", "promote", "release"),
    ("cadence-survival", "cadence", "mind"),
    ("digest-survival", "digest", "digest"),
    ("reclaim-survival", "reclaim", "hand"),
    ("need-audit-survival", "need-audit", "auditor"),
    ("need-merge-survival", "need-merge", "merge"),
];
const UNIT: &str = "hand-survival";
const TEST_HARNESS: &str = r#"#!/bin/sh
set -eu
brief="$1"
id=$(sed -n 's/^pump-job-id: //p' "$brief" | head -n 1)
kind=$(sed -n 's/^pump-job-kind: //p' "$brief" | head -n 1)
if [ -z "$id" ]; then
  id="$VIVI_TEST_UNIT_ID"
  kind=hand
fi
printf '%s %s\n' "$kind" "$id" > "$VIVI_TEST_MARKERS/$id.started"
while [ ! -f "$VIVI_TEST_MARKERS/release-workers" ]; do sleep 0.1; done
case "$kind" in
  planner:board)
    printf '%s\n' 'PUMP-PLAN-BEGIN' '{"need":"planner-survival","verdict":"hold","reason":"recovered once"}' 'PUMP-PLAN-END'
    ;;
  merge)
    printf 'survived merge\n' > .survival-merge.txt
    git add .survival-merge.txt
    git -c user.name='Survival Test' -c user.email=survival@localhost commit -q -m 'survival merge candidate'
    candidate=$(git rev-parse HEAD)
    printf '%s\n' "PUMP_MERGE_RESULT={\"version\":1,\"target\":\"default\",\"tip\":\"$candidate\",\"sources\":[]}"
    ;;
  gate)
    printf '%s\n' 'FAILING-BEGIN' 'FAILING-END'
    ;;
  audit)
    printf '%s\n' 'AUDIT-BEGIN' '[{"root_cause":"survived audit","brief":"record the recovered audit","tests":["survival::audit"],"write_scope":["audit.txt"],"proof":["true"]}]' 'AUDIT-END'
    ;;
  promote)
    printf '%s\n' "PUMP_PROMOTE_RESULT={\"status\":\"ready\",\"tip\":\"$VIVI_TEST_TIP\"}"
    ;;
  cadence)
    printf 'cadence survived\n'
    ;;
  digest)
    printf 'digest survived\n'
    ;;
  reclaim)
    printf 'reclaim survived\n'
    ;;
  need-audit)
    printf '%s\n' 'PUMP_NEED_AUDIT_RESULT={"version":1,"verdict":"clean_pass"}'
    ;;
  need-merge)
    printf 'survived need landing\n' > .survival-landing.txt
    git add .survival-landing.txt
    git -c user.name='Survival Test' -c user.email=survival@localhost commit -q -m 'survival need landing'
    candidate=$(git rev-parse HEAD)
    printf '%s\n' "PUMP_MERGE_RESULT={\"version\":1,\"target\":\"default\",\"tip\":\"$candidate\",\"sources\":[]}"
    ;;
  hand)
    printf '%s\n' 'VERDICT: no-defect'
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
fn local_pool_jobs_and_a_hand_unit_survive_sigkill_and_apply_once() {
    let rig = scaffold();
    seed_state(&rig);

    let mut first = pump_child(&rig);
    let expected: Vec<String> = WORKERS
        .iter()
        .map(|(id, _, _)| (*id).to_owned())
        .chain(std::iter::once(UNIT.to_owned()))
        .collect();
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
    assert_eq!(stopped_state.units[UNIT].status, Status::Inflight);

    let mut restarted = pump_child(&rig);
    assert!(
        wait_until(Duration::from_secs(40), || applied(&rig)),
        "restart did not apply all receipts; pump status: {}",
        fs::read_to_string(rig.home.join("status.txt")).unwrap_or_default()
    );
    stop_pump(&rig, &mut restarted);

    let final_state = load_state(&rig);
    assert_eq!(final_state.units[UNIT].status, Status::NoDefect);
    assert!(
        final_state.units[UNIT]
            .verdict_text
            .as_deref()
            .is_some_and(|text| text.contains("VERDICT: no-defect"))
    );
    assert_eq!(final_state.gates.len(), 1, "gate receipt must apply once");
    assert_eq!(
        final_state.promotions.len(),
        1,
        "promotion receipt must apply once"
    );
    assert!(
        !final_state.extra.contains_key("audit"),
        "audit run remains open"
    );
    assert!(
        final_state.units.contains_key("audit-survival-1"),
        "audit report was not applied"
    );
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
            "merge" | "need-merge" => "PUMP_MERGE_RESULT=",
            "gate" => "FAILING-BEGIN",
            "audit" => "AUDIT-BEGIN",
            "promote" => "PUMP_PROMOTE_RESULT=",
            "cadence" => "cadence survived",
            "digest" => "digest survived",
            "reclaim" => "reclaim survived",
            "need-audit" => "PUMP_NEED_AUDIT_RESULT=",
            other => panic!("unexpected seeded job kind {other}"),
        };
        assert!(
            receipt.contains(marker),
            "{id} has the wrong saved receipt: {receipt}"
        );
        if job.kind != "need-audit" {
            assert!(job.applied_at.is_some(), "{id} receipt was not applied");
        }
        if matches!(job.kind.as_str(), "merge" | "promote") {
            let publication = job.outcome.as_ref().unwrap().runner_publish().unwrap();
            assert!(publication.succeeded(), "{id}: {publication:?}");
            assert_eq!(
                publication.reference,
                if job.kind == "merge" {
                    "refs/heads/next"
                } else {
                    "refs/heads/main"
                },
                "{id} published the wrong integration ref"
            );
            assert_eq!(publication.candidate, publication.remote_sha);
        }
        assert_eq!(
            task_occurrences(&rig.project, &format!("[pump-job:{id}]"), role_for(id)),
            1,
            "restart sent a duplicate task for {id}"
        );
    }
    assert_eq!(task_occurrences(&rig.project, UNIT, "hand"), 1);
}

fn scaffold() -> Rig {
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
    let additions = format!(
        "\n[pump]\nhome = {}\nrepo = {}\nremote = \"origin\"\nremote_url = {}\nbranch = \"next\"\nmain = \"main\"\npromote = false\ntick = 1\ncommitter_name = \"Survival Test\"\ncommitter_email = \"survival@localhost\"\nfrom = \"mind\"\nto = \"hand\"\nreply_from = \"mind\"\n\n[pump.board]\nmark = \"\"\nfrom = []\npull = false\n\n[pump.dispatch]\ndefault_kind = \"unit\"\nper_tick = 1\nlanes = [{{ model = \"standin\", cap = 1 }}]\n\n[pump.dispatch.kinds.unit]\npools = [\"p\"]\n\n[pump.gate]\ncommand = \"true\"\npools = [\"p\"]\n\n[pump.merge]\nshared_checks = [\"true\"]\n\n[pools.p]\nprovider = \"local\"\nslots = 16\n\n[pools.p.bands]\nP2 = [{{ model = \"standin\", reasoning = \"low\" }}]\n\n[pools.p.local]\nroot = {}\nworktree_of = {}\n\n[pools.p.harness]\nname = \"standin\"\nrun = \"bash '{}' {{brief}}\"\nmodels = [\"standin\"]\nenv = [\"VIVI_TEST_MARKERS\", \"VIVI_TEST_UNIT_ID\", \"VIVI_TEST_TIP\"]\n\n[pools.p.committer]\nname = \"Survival Test\"\nemail = \"survival@localhost\"\n",
        toml_string(&project.join(".vivi/pump-home")),
        toml_string(&repo),
        toml_string(&bare),
        toml_string(&base.join("lanes")),
        toml_string(&repo),
        script.display(),
    );
    for (role, kind) in [
        ("mind", "mind"),
        ("hand", "hand"),
        ("pump-planner", "planner"),
        ("merge", "merge"),
        ("gate", "auditor"),
        ("auditor", "auditor"),
        ("release", "merge"),
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
    st.baseline = Some(Vec::new());
    st.last_gate = GateRun {
        merges: 0,
        job_id: Some(String::from("gate-survival")),
        tip: Some(rig.tip.clone()),
        ..GateRun::default()
    };
    st.extra.insert(
        String::from("audit"),
        serde_json::to_value(AuditRun {
            need: String::from("audit-survival"),
            tip: rig.tip.clone(),
            job_id: Some(String::from("audit-survival")),
            ..AuditRun::default()
        })
        .unwrap(),
    );
    let unit = Unit {
        need: String::new(),
        title: String::from("survive pump restart"),
        brief: String::from("Return VERDICT: no-defect after the process restart."),
        write_scope: vec![String::from("survival.txt")],
        status: Status::Pending,
        ..Unit::default()
    };
    st.units.insert(UNIT.to_owned(), unit);
    st.order.push(UNIT.to_owned());

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
        spec.from_ref = Some(if *kind == "promote" {
            String::from("main")
        } else {
            String::from("next")
        });
        spec.model = Some(String::from("standin"));
        spec.budget = 60;
        spec.read_only = matches!(
            *kind,
            "planner:board" | "gate" | "audit" | "cadence" | "digest" | "reclaim" | "need-audit"
        );
        if *kind == "merge" {
            spec.branch = Some(String::from("pump/merge/default/survival"));
            spec.publish_ref = Some(String::from("refs/heads/next"));
        }
        if *kind == "promote" {
            spec.branch = Some(String::from("pump/promotion/default/survival"));
            spec.publish_ref = Some(String::from("refs/heads/main"));
            spec.publish_expect = Some(rig.tip.clone());
            spec.payload = serde_json::json!({
                "target": "default",
                "branch": "main",
                "remote": "origin",
                "tip": rig.tip.clone(),
                "failing": [],
            });
        }
        if *kind == "merge" {
            spec.payload = serde_json::json!({
                "version": 1,
                "target": "default",
                "integration_branch": "next",
            });
        }
        if *kind == "planner:board" {
            spec.payload = serde_json::json!({
                "planner_output": {
                    "out": "plans/survival.out",
                    "plan": "plans/survival-plan.json",
                }
            });
        }
        if *kind == "need-merge" {
            spec.branch = Some(String::from("pump/need-merge/default/survival"));
            spec.publish_ref = Some(String::from("refs/heads/vivi/need/survival"));
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
        .env("VIVI_TEST_UNIT_ID", UNIT)
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
    }) && state
        .units
        .get(UNIT)
        .is_some_and(|unit| unit.status == Status::Inflight && unit.task.is_some())
}

fn applied(rig: &Rig) -> bool {
    let state = load_state(rig);
    WORKERS.iter().all(|(id, kind, _)| {
        state.jobs.get(*id).is_some_and(|job| {
            job.status == JobStatus::Finished && (*kind == "need-audit" || job.applied_at.is_some())
        })
    }) && state
        .units
        .get(UNIT)
        .is_some_and(|unit| unit.status == Status::NoDefect)
        && state.gates.len() == 1
        && state
            .promotions
            .iter()
            .any(|promotion| promotion.tip == rig.tip)
        && !state.extra.contains_key("audit")
        && state.units.contains_key("audit-survival-1")
        && rig.home.join("plans/survival-plan.json").is_file()
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
    for role in [
        "hand",
        "pump-planner",
        "merge",
        "gate",
        "auditor",
        "release",
        "mind",
        "digest",
    ] {
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
