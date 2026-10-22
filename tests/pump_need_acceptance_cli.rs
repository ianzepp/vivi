//! Managed need work stays private until its frozen range is accepted and landed.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::Digest;
use vivi::pump::jobs::JobStatus;
use vivi::pump::state::{GateRecord, GateRun, NeedRecord, State, Status, Unit};
use vivi::pump::store::{DB_FILENAME, PumpStore};

const LEGACY_UNIT: &str = "a1";
const LEGACY_BRANCH: &str = "unit/a1-a1";
const SECOND_A: &str = "a2";
const FIRST_B: &str = "b1";
const SECOND_B: &str = "b2";

const WORKER: &str = r#"#!/usr/bin/env python3
import json, os, pathlib, re, subprocess, sys, time

body = pathlib.Path(sys.argv[1]).read_text()
markers = pathlib.Path(os.environ["VIVI_TEST_MARKERS"])
match = re.search(r"(?m)^pump-job-kind: (.+)$", body)
kind = match.group(1).strip() if match else "hand"
match = re.search(r"(?m)^pump-job-id: (.+)$", body)
job_id = match.group(1).strip() if match else "manual-unit"

def run(*args, capture=False):
    return subprocess.run(["git", *args], check=True, text=True,
        stdout=subprocess.PIPE if capture else subprocess.DEVNULL)

def git_output(*args):
    return run(*args, capture=True).stdout.strip()

def read_json_after(marker):
    tail = body.split(marker, 1)[1].lstrip()
    return json.JSONDecoder().raw_decode(tail)[0]

def mark(name):
    (markers / name).write_text("started\n")

def wait_for(name):
    release = markers / name
    deadline = time.monotonic() + 180
    while time.monotonic() < deadline:
        if release.exists() or (markers / "release-all").exists():
            return
        time.sleep(0.05)
    raise RuntimeError(f"test did not release worker: {name}")

def commit(path, message):
    run("add", path)
    run("-c", "user.name=Need Acceptance Test", "-c",
        "user.email=need-acceptance@localhost", "commit", "-q", "-m", message)

if kind == "hand":
    match = re.search(r"(?m)^UNIT ([^:]+):", body)
    if not match:
        raise RuntimeError("unit brief has no UNIT header")
    unit = match.group(1).strip()
    mark(f"unit-{unit}.started")
    wait_for(f"unit-{unit}.go")
    path = f"unit-{unit}.txt"
    pathlib.Path(path).write_text(f"completed {unit}\n")
    commit(path, f"complete unit {unit}")
    print(f"Completed unit {unit}; proof true passed.")

elif kind == "need-branch-init":
    payload = read_json_after("Payload:\n")
    head = git_output("rev-parse", "HEAD")
    result = {"version": 1, "target": payload["target"], "need": payload["need"],
        "branch": payload["branch"], "status": "ready" if head == payload["base_sha"] else "blocked",
        "base_sha": payload["base_sha"], "tip_sha": head}
    print("PUMP_NEED_BRANCH_RESULT=" + json.dumps(result, separators=(",", ":")))

elif kind == "merge":
    payload = read_json_after("Input JSON:\n")
    if not payload.get("need_merge"):
        raise RuntimeError(f"unexpected generic merge job {job_id}")
    sources = payload["unit_sources"]
    for source in sources:
        for command in source.get("proof", []):
            subprocess.run(command, shell=True, check=True)
        run("merge", "--no-edit", source["source_sha"])
    for command in payload.get("shared_checks", []):
        subprocess.run(command, shell=True, check=True)
    result = {"version": 1, "target": payload["target"], "tip": git_output("rev-parse", "HEAD"),
        "sources": [{"kind": "unit", "id": source["id"], "status": "merged", "why": "",
            "detail": "merged into isolated need branch", "outside": [], "secs": 0} for source in sources],
        "shared_check_timeouts": 0, "shared_check_timeout_retries": 0,
        "shared_check_recoveries": []}
    print("PUMP_MERGE_RESULT=" + json.dumps(result, separators=(",", ":")))

elif kind == "need-audit":
    identity = re.search(
        r"Dispatch identity: job_id=([^;]+); receipt_key=([^;]+); need=([^;]+); "
        r"target=([^;]+); revision=(\d+); frozen_binding=([0-9a-f]+)\.\.([0-9a-f]+); "
        r"source_hash=[^;]+; review_range=([0-9a-f]+)\.\.([0-9a-f]+)\. Round: (\d+)", body)
    if not identity:
        raise RuntimeError("need audit brief has no frozen identity")
    job, key, need, target, revision, base, tip, audit_base, _, round_no = identity.groups()
    packet = read_json_after("Audit packet JSON:\n")
    source_hash = packet["source_hash"]
    if not source_hash.startswith("v4:"):
        raise RuntimeError("need audit job did not carry a source-aware v4 hash")
    if not packet["requirements"]:
        raise RuntimeError("acceptance audit packet omitted the need's done_when requirement")
    mark(f"audit-{need}.started")
    wait_for(f"audit-{need}.go")
    requirements = [{"id": item["id"], "text": item["text"], "status": "SATISFIED",
        "evidence": ["acceptance fixture verified the frozen branch"]} for item in packet["requirements"]]
    result = {"version": 1, "receipt_key": key, "job_id": job,
        "binding": {"need": need, "target": target, "revision": int(revision), "base": base, "tip": tip},
        "source_hash": source_hash,
        "audit_base": audit_base, "round": int(round_no), "verdict": "clean_pass",
        "requirements": requirements, "findings": [], "diagnoses": [], "blind_spots": [],
        "report": {"id": f"report-{job}", "reference": f"acceptance-report/{job}"}}
    print("PUMP_NEED_AUDIT_RESULT=" + json.dumps(result, separators=(",", ":")), flush=True)
    mark(f"audit-{need}.done")

elif kind == "need-merge":
    payload = read_json_after("Input JSON:\n")
    run("fetch", "-q", "origin", payload["target_ref"])
    run("merge", "--ff-only", "FETCH_HEAD")
    run("merge", "--no-edit", payload["binding"]["tip"])
    for check in payload["required_checks"]:
        subprocess.run(check["command"], shell=True, check=True)
    candidate = git_output("rev-parse", "HEAD")
    checks = [{"name": check["name"], "command": check["command"], "status": "passed"}
        for check in payload["required_checks"]]
    result = {"version": 1, "target": payload["binding"]["target"],
        "need": payload["binding"]["need"], "revision": payload["binding"]["revision"],
        "base": payload["binding"]["base"], "tip": payload["binding"]["tip"],
        "candidate": candidate, "status": "merged", "checks": checks,
        "sources": [{"kind": "job", "id": payload["source_job_id"], "status": "merged",
            "why": "", "detail": "accepted need branch merged"}]}
    print("PUMP_MERGE_RESULT=" + json.dumps(result, separators=(",", ":")))

elif kind == "need-cleanup":
    payload = read_json_after("Input JSON:\n")
    need = payload["binding"]["need"]
    mark(f"cleanup-{need}.started")
    wait_for(f"cleanup-{need}.go")
    print(f"Cleanup authorized for {payload['branch_ref']}.")

elif kind == "gate":
    mark(f"gate-{job_id}.started")
    wait_for(f"gate-{job_id}.go")
    subprocess.run("true", shell=True, check=True)
    print("FAILING-BEGIN\nFAILING-END")

elif kind == "promote":
    match = re.search(r"The green integration tip is `([0-9a-f]+)`", body)
    if not match:
        raise RuntimeError("promotion assignment omitted its exact green tip")
    tip = match.group(1)
    if git_output("rev-parse", "HEAD") != tip or git_output("status", "--porcelain"):
        raise RuntimeError("promotion checkout differs from its green tip or is dirty")
    print("PUMP_PROMOTE_RESULT=" + json.dumps({"status": "ready", "tip": tip},
        separators=(",", ":")))

elif kind == "reclaim":
    request = body.split("Request:\n```json\n", 1)[1].split("\n```", 1)[0]
    pathlib.Path("reclaim-request.json").write_text(request + "\n")
    command = [os.environ["VIVI_TEST_BIN"], "pool", "clean", "--after-merged-request",
        "reclaim-request.json", "--porcelain"]
    result = subprocess.run(command, text=True, capture_output=True)
    print(result.stdout)
    print(result.stderr, file=sys.stderr)
    if result.returncode:
        raise RuntimeError(f"pool clean exited {result.returncode}")

else:
    raise RuntimeError(f"unexpected worker kind {kind!r} for {job_id}")
"#;

struct Rig {
    _dir: tempfile::TempDir,
    project: PathBuf,
    home: PathBuf,
    markers: PathBuf,
    bare: PathBuf,
    repo: PathBuf,
    child_log: PathBuf,
    base: String,
}

struct PumpChild {
    child: Child,
    markers: PathBuf,
}

impl Drop for PumpChild {
    fn drop(&mut self) {
        let _ = fs::write(self.markers.join("release-all"), "release\n");
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn independent_multi_unit_needs_survive_restart_and_land_only_after_acceptance() {
    let rig = scaffold();
    let need_a = send_need(
        &rig,
        "Need A",
        "all A unit files are on the accepted integration branch",
    );
    let need_b = send_need(
        &rig,
        "Need B",
        "all B unit files are on the accepted integration branch",
    );
    let legacy_need = send_need(
        &rig,
        "Legacy need",
        "the already-merged revision-zero unit remains complete",
    );
    let legacy_task = send_task(
        &rig,
        "Legacy unit A1",
        "UNIT a1: preserve this stored legacy attempt branch\ndone_when: the a1 unit file exists\n",
    );
    let legacy_dispatch = vivi_with_env(
        &rig,
        [
            "pool",
            "dispatch",
            "--pool",
            "p",
            &legacy_task,
            "--target",
            "default",
            "--from",
            "next",
            "--branch",
            LEGACY_BRANCH,
            "--model",
            "standin",
            "--budget",
            "120",
            "--project",
            path(&rig.project),
        ],
    );
    assert_success(&legacy_dispatch);
    assert!(
        wait_until(Duration::from_secs(30), || rig
            .markers
            .join("unit-a1.started")
            .is_file()),
        "stored legacy unit task did not start: {}",
        startup_diagnostics(&rig)
    );
    seed_state(&rig, &need_a, &need_b, &legacy_need, &legacy_task);

    // Legacy revision-zero units retain the existing closeout path.
    assert!(closeout_handles(&rig).contains(&legacy_need));
    let closed_legacy = vivi([
        "pump",
        "closeout",
        "--close",
        &legacy_need,
        "--for",
        "mind",
        "--project",
        path(&rig.project),
    ]);
    assert_success(&closed_legacy);

    let mut first = pump_child(&rig);
    let all_started = wait_until(Duration::from_secs(90), || {
        ["a1", "a2", "b1", "b2"]
            .iter()
            .all(|unit| rig.markers.join(format!("unit-{unit}.started")).is_file())
            && need_record(&load_state(&rig), &need_a).is_some()
            && need_record(&load_state(&rig), &need_b).is_some()
    });
    assert!(
        all_started,
        "pump did not dispatch both needs independently: {}",
        startup_diagnostics(&rig)
    );
    let initial = load_state(&rig);
    assert_eq!(unit_branch(&initial, LEGACY_UNIT), Some(LEGACY_BRANCH));
    for need in [&need_a, &need_b] {
        let branch = need_record(&initial, need).unwrap();
        assert_eq!(branch["branch"], format!("vivi/need/{need}"));
        assert_eq!(branch["status"], "active");
        assert_eq!(branch["base_sha"], rig.base);
    }

    release_unit(&rig, LEGACY_UNIT);
    release_unit(&rig, FIRST_B);
    assert!(
        wait_until(Duration::from_secs(90), || {
            let state = load_state(&rig);
            state.units[LEGACY_UNIT].status == Status::Merged
                && state.units[FIRST_B].status == Status::Merged
        }),
        "first units did not merge into their need branches: {}",
        startup_diagnostics(&rig)
    );
    assert_eq!(
        remote_sha(&rig, "refs/heads/next").as_deref(),
        Some(rig.base.as_str())
    );
    assert!(!remote_has_path(&rig, "next", &unit_path(LEGACY_UNIT)));
    assert!(!remote_has_path(&rig, "next", &unit_path(FIRST_B)));
    assert_eq!(
        unit_branch(&load_state(&rig), LEGACY_UNIT),
        Some(LEGACY_BRANCH)
    );

    release_unit(&rig, SECOND_A);
    assert!(
        wait_until(Duration::from_secs(90), || {
            need_record(&load_state(&rig), &need_a)
                .is_some_and(|record| record["status"] == "frozen")
                && rig
                    .markers
                    .join(format!("audit-{need_a}.started"))
                    .is_file()
        }),
        "need A did not freeze and start acceptance: {}",
        startup_diagnostics(&rig)
    );
    let frozen_a = load_state(&rig);
    let branch_a = need_record(&frozen_a, &need_a).unwrap();
    let binding_a = serde_json::json!({"need": need_a, "target": "default", "revision": 1,
        "base": rig.base, "tip": branch_a["frozen_tip_sha"]});
    assert_eq!(branch_a["audit_base_sha"], rig.base);
    assert_eq!(need_record(&frozen_a, &need_b).unwrap()["status"], "active");
    assert_eq!(
        remote_sha(&rig, "refs/heads/next").as_deref(),
        Some(rig.base.as_str())
    );
    for id in [LEGACY_UNIT, SECOND_A, FIRST_B, SECOND_B] {
        assert!(
            !remote_has_path(&rig, "next", &unit_path(id)),
            "partial unit {id} leaked to next"
        );
    }
    assert!(
        !closeout_handles(&rig).contains(&need_a),
        "all-merged managed need appeared complete before accepted landing and cleanup"
    );
    let refused_close = vivi([
        "pump",
        "closeout",
        "--close",
        &need_a,
        "--for",
        "mind",
        "--project",
        path(&rig.project),
    ]);
    assert!(
        !refused_close.status.success(),
        "unlanded managed need closed from the CLI"
    );
    assert!(
        String::from_utf8_lossy(&refused_close.stderr).contains("not a complete-but-open need"),
        "{}",
        String::from_utf8_lossy(&refused_close.stderr)
    );

    // The pool owns the audit task. Kill only the controller; let the Auditor
    // finish, then require startup recovery to apply that exact receipt once.
    first.child.kill().unwrap();
    first.child.wait().unwrap();
    let audit_a = job_for_need(&load_state(&rig), "need-audit", &need_a)
        .expect("frozen need A has no durable audit job")
        .id
        .clone();
    release_audit(&rig, &need_a);
    assert!(
        wait_until(Duration::from_secs(30), || rig
            .markers
            .join(format!("audit-{need_a}.done"))
            .is_file()),
        "Auditor did not finish while the pump was stopped"
    );

    let mut restarted = pump_child(&rig);
    assert!(
        wait_until(Duration::from_secs(90), || {
            need_landing_receipt(&load_state(&rig), &need_a).is_some()
                && rig
                    .markers
                    .join(format!("cleanup-{need_a}.started"))
                    .is_file()
        }),
        "restart did not recover the accepted audit and publish need A: {}",
        startup_diagnostics(&rig)
    );
    let state_a = load_state(&rig);
    let branch_a = need_record(&state_a, &need_a).unwrap();
    let landing_a = need_landing_receipt(&state_a, &need_a).unwrap();
    assert_eq!(branch_a["status"], "frozen", "cleanup is deliberately held");
    assert_eq!(landing_a["binding"], binding_a);
    assert_eq!(landing_a["target_ref"], "refs/heads/next");
    assert_eq!(
        landing_a["candidate_sha"],
        landing_a["runner_published_sha"]
    );
    assert_eq!(
        landing_a["runner_published_sha"],
        landing_a["observed_target_sha"]
    );
    assert_eq!(landing_a["accepted_tip_in_candidate"], true);
    assert_eq!(landing_a["candidate_in_observed_target"], true);
    assert!(
        !closeout_handles(&rig).contains(&need_a),
        "closeout listed an accepted need before cleanup was verified"
    );
    let refused_during_cleanup = vivi([
        "pump",
        "closeout",
        "--close",
        &need_a,
        "--for",
        "mind",
        "--project",
        path(&rig.project),
    ]);
    assert!(!refused_during_cleanup.status.success());
    assert_eq!(
        remote_sha(&rig, "refs/heads/main").as_deref(),
        Some(rig.base.as_str())
    );
    assert!(remote_has_path(&rig, "next", &unit_path(LEGACY_UNIT)));
    assert!(remote_has_path(&rig, "next", &unit_path(SECOND_A)));
    assert!(!remote_has_path(&rig, "next", &unit_path(FIRST_B)));
    assert!(!remote_has_path(&rig, "next", &unit_path(SECOND_B)));
    assert!(remote_sha(&rig, &format!("refs/heads/vivi/need/{need_a}")).is_some());
    let gate_a = gate_job(&state_a)
        .expect("need A landing did not queue its integration gate")
        .id
        .clone();
    assert!(rig.markers.join(format!("gate-{gate_a}.started")).is_file());
    assert!(
        !state_a.jobs.values().any(|job| job.kind == "promote"),
        "acceptance bypassed the pending gate"
    );
    let verdict_a = need_verdict_state(&state_a, &need_a);
    assert_eq!(verdict_a["receipts"].as_object().unwrap().len(), 1);
    assert_eq!(verdict_a["accepted"]["receipt"]["binding"], binding_a);
    let source_hash_a = state_a.jobs[&audit_a].payload["source_hash"]
        .as_str()
        .expect("audit job omitted source_hash");
    assert!(source_hash_a.starts_with("v4:"));
    assert_eq!(
        verdict_a["accepted"]["receipt"]["source_hash"],
        source_hash_a
    );
    assert!(state_a.jobs[&audit_a].applied_at.is_some());
    assert_eq!(task_occurrences(&rig, &audit_a, "auditor"), 1);

    release_cleanup(&rig, &need_a);
    assert!(
        wait_until(Duration::from_secs(60), || {
            need_record(&load_state(&rig), &need_a)
                .is_some_and(|record| record["status"] == "landed")
                && remote_sha(&rig, &format!("refs/heads/vivi/need/{need_a}")).is_none()
        }),
        "verified need A cleanup did not complete: {}",
        startup_diagnostics(&rig)
    );
    assert!(closeout_handles(&rig).contains(&need_a));
    release_gate(&rig, &gate_a);
    assert!(
        wait_until(Duration::from_secs(90), || {
            let state = load_state(&rig);
            state
                .promotions
                .iter()
                .any(|promotion| promotion.tip == landing_a["observed_target_sha"])
        }),
        "passing gate did not enable regular promotion: {}",
        startup_diagnostics(&rig)
    );
    let tip_a = landing_a["observed_target_sha"].as_str().unwrap();
    assert_eq!(remote_sha(&rig, "refs/heads/main").as_deref(), Some(tip_a));
    assert!(!remote_has_path(&rig, "main", &unit_path(FIRST_B)));

    // Need B made real progress while A awaited its audit, but its two unit
    // commits remain private until B freezes and receives its own acceptance.
    release_unit(&rig, SECOND_B);
    assert!(
        wait_until(Duration::from_secs(90), || {
            let state = load_state(&rig);
            state.units[FIRST_B].status == Status::Merged
                && state.units[SECOND_B].status == Status::Merged
                && need_record(&state, &need_b).is_some_and(|record| record["status"] == "frozen")
                && rig
                    .markers
                    .join(format!("audit-{need_b}.started"))
                    .is_file()
        }),
        "need B did not complete and start its own audit: {}",
        startup_diagnostics(&rig)
    );
    assert_eq!(remote_sha(&rig, "refs/heads/main").as_deref(), Some(tip_a));
    assert!(!remote_has_path(&rig, "next", &unit_path(FIRST_B)));
    assert!(!remote_has_path(&rig, "next", &unit_path(SECOND_B)));
    release_audit(&rig, &need_b);
    assert!(
        wait_until(Duration::from_secs(90), || {
            need_landing_receipt(&load_state(&rig), &need_b).is_some()
                && rig
                    .markers
                    .join(format!("cleanup-{need_b}.started"))
                    .is_file()
        }),
        "need B acceptance did not queue its landing: {}",
        startup_diagnostics(&rig)
    );
    let state_b = load_state(&rig);
    let landing_b = need_landing_receipt(&state_b, &need_b).unwrap();
    let tip_b = landing_b["observed_target_sha"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        landing_b["candidate_sha"],
        landing_b["runner_published_sha"]
    );
    assert_eq!(
        landing_b["runner_published_sha"],
        landing_b["observed_target_sha"]
    );
    assert_eq!(landing_b["accepted_tip_in_candidate"], true);
    assert_eq!(landing_b["candidate_in_observed_target"], true);
    assert_ne!(landing_b["candidate_sha"], landing_b["binding"]["tip"]);
    assert!(remote_has_path(&rig, "next", &unit_path(FIRST_B)));
    assert!(remote_has_path(&rig, "next", &unit_path(SECOND_B)));
    assert_eq!(remote_sha(&rig, "refs/heads/main").as_deref(), Some(tip_a));
    let gate_b = gate_job(&state_b)
        .expect("need B landing did not queue its integration gate")
        .id
        .clone();
    assert!(rig.markers.join(format!("gate-{gate_b}.started")).is_file());
    assert!(
        !state_b
            .promotions
            .iter()
            .any(|promotion| promotion.tip == tip_b)
    );

    release_cleanup(&rig, &need_b);
    assert!(
        wait_until(Duration::from_secs(60), || {
            need_record(&load_state(&rig), &need_b)
                .is_some_and(|record| record["status"] == "landed")
                && remote_sha(&rig, &format!("refs/heads/vivi/need/{need_b}")).is_none()
        }),
        "verified need B cleanup did not complete: {}",
        startup_diagnostics(&rig)
    );
    release_gate(&rig, &gate_b);
    assert!(
        wait_until(Duration::from_secs(90), || {
            load_state(&rig)
                .promotions
                .iter()
                .any(|promotion| promotion.tip == tip_b)
                && remote_sha(&rig, "refs/heads/main").as_deref() == Some(tip_b.as_str())
        }),
        "passing gate did not promote need B's accepted landing: {}",
        startup_diagnostics(&rig)
    );

    let final_state = load_state(&rig);
    assert_eq!(
        need_record(&final_state, &need_a).unwrap()["status"],
        "landed"
    );
    assert_eq!(
        need_record(&final_state, &need_b).unwrap()["status"],
        "landed"
    );
    let audit_b = job_for_need(&final_state, "need-audit", &need_b).unwrap();
    assert_eq!(
        need_verdict_state(&final_state, &need_b)["accepted"]["receipt"]["source_hash"],
        audit_b.payload["source_hash"]
    );
    assert_eq!(unit_branch(&final_state, LEGACY_UNIT), Some(LEGACY_BRANCH));
    assert_eq!(
        final_state.gates.len(),
        3,
        "baseline plus one gate per accepted landing"
    );
    assert_eq!(task_occurrences(&rig, &audit_a, "auditor"), 1);
    for job in final_state.jobs.values().filter(|job| job.task.is_some()) {
        assert_eq!(
            task_occurrences(&rig, &job.id, &job.role),
            1,
            "restart or receipt replay duplicated job {}",
            job.id
        );
    }
    stop_pump(&rig, &mut restarted);
}

fn scaffold() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let base_path = dir.path().canonicalize().unwrap();
    let project = base_path.join("project");
    fs::create_dir(&project).unwrap();
    assert_success(&vivi(["mailspace", "init", "--project", path(&project)]));

    let bare = base_path.join("origin.git");
    let seed = base_path.join("seed");
    fs::create_dir(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    git(&seed, &["config", "user.name", "Need Acceptance Test"]);
    git(
        &seed,
        &["config", "user.email", "need-acceptance@localhost"],
    );
    fs::write(seed.join("README.md"), "need acceptance fixture\n").unwrap();
    git(&seed, &["add", "README.md"]);
    git(&seed, &["commit", "-q", "-m", "fixture"]);
    git(&seed, &["clone", "-q", "--bare", ".", path(&bare)]);
    git(&seed, &["push", "-q", path(&bare), "main"]);
    git(&seed, &["push", "-q", path(&bare), "main:next"]);

    let home = project.join(".vivi/pump-home");
    fs::create_dir_all(&home).unwrap();
    let repo = home.join("repo");
    git(&base_path, &["clone", "-q", path(&bare), path(&repo)]);
    git(&repo, &["checkout", "-q", "-B", "next", "origin/next"]);
    let base = git_output(&repo, &["rev-parse", "HEAD"]);

    let markers = base_path.join("markers");
    fs::create_dir(&markers).unwrap();
    let worker = base_path.join("worker.py");
    fs::write(&worker, WORKER).unwrap();
    write_config(
        &project,
        &home,
        &repo,
        &bare,
        &base_path.join("lanes"),
        &worker,
    );
    assert_success(&vivi([
        "pool",
        "create",
        "--pool",
        "p",
        "--project",
        path(&project),
    ]));
    for (name, kind) in [
        ("mind", "mind"),
        ("hand", "hand"),
        ("auditor", "auditor"),
        ("gate", "gate"),
        ("release", "release"),
        ("reclaim", "reclaim"),
        ("head-ceo", "head"),
        ("head-cto", "head"),
    ] {
        assert_success(&vivi([
            "role",
            "add",
            name,
            "--kind",
            kind,
            "--model",
            "P2",
            "--pool",
            "p",
            "--project",
            path(&project),
        ]));
    }
    assert_success(&vivi([
        "role",
        "pool",
        "merge",
        "p",
        "--project",
        path(&project),
    ]));
    assert_success(&vivi([
        "role",
        "set",
        "merge",
        "--model",
        "P2",
        "--project",
        path(&project),
    ]));
    // `mailspace init` provisioned the pump planner; it only needs a pool.
    assert_success(&vivi([
        "role",
        "pool",
        "pump-planner",
        "p",
        "--project",
        path(&project),
    ]));
    assert_success(&vivi([
        "role",
        "set",
        "pump-planner",
        "--model",
        "P2",
        "--project",
        path(&project),
    ]));
    Rig {
        _dir: dir,
        project,
        home,
        markers,
        bare,
        repo,
        child_log: base_path.join("pump-child.log"),
        base,
    }
}

fn write_config(
    project: &Path,
    home: &Path,
    repo: &Path,
    bare: &Path,
    lanes: &Path,
    worker: &Path,
) {
    let config_path = project.join(".vivi/mailspace.toml");
    let old = fs::read_to_string(&config_path).unwrap();
    // `mailspace init` writes a starter [pump]; this test supplies its own.
    let base_config = old
        .replace("identities = []\n", "")
        .replace(vivi::mailspace::STARTER_PUMP, "");
    let additions = format!(
        "\n[pump]\nhome = {}\nrepo = {}\nremote = \"origin\"\nremote_url = {}\nbranch = \"next\"\nmain = \"main\"\npromote = true\ntick = 1\ncommitter_name = \"Need Acceptance Test\"\ncommitter_email = \"need-acceptance@localhost\"\nfrom = \"mind\"\nto = \"hand\"\nreply_from = \"mind\"\n\n[pump.board]\nmark = \"\"\nfrom = []\npull = false\n\n[pump.dispatch]\ndefault_kind = \"unit\"\nper_tick = 4\nlanes = [{{ model = \"standin\", cap = 8 }}]\n\n[pump.dispatch.kinds.unit]\npools = [\"p\"]\n\n[pump.gate]\ncommand = \"true\"\npools = [\"p\"]\n\n[pump.merge]\nshared_checks = [\"true\"]\n\n[pools.p]\nprovider = \"local\"\nslots = 12\n\n[pools.p.local]\nroot = {}\nworktree_of = {}\n\n[pools.p.harness]\nname = \"need-acceptance\"\nrun = \"python3 '{}' {{brief}}\"\nmodels = [\"standin\"]\nenv = [\"VIVI_TEST_MARKERS\", \"VIVI_TEST_BIN\"]\n\n[[pools.p.bands.P2]]\nmodel = \"standin\"\nreasoning = \"low\"\n\n[pools.p.committer]\nname = \"Need Acceptance Test\"\nemail = \"need-acceptance@localhost\"\n",
        toml_string(home),
        toml_string(repo),
        toml_string(bare),
        toml_string(lanes),
        toml_string(repo),
        worker.display(),
    );
    fs::write(config_path, format!("{base_config}{additions}")).unwrap();
}

fn send_need(rig: &Rig, subject: &str, criterion: &str) -> String {
    let body = format!("done_when: {criterion}\n\nThis is a two-unit acceptance fixture.\n");
    let output = vivi([
        "need",
        "send",
        "--from",
        "mind",
        "--to",
        "hand",
        "--subject",
        subject,
        "--body",
        &body,
        "--project",
        path(&rig.project),
    ]);
    assert_success(&output);
    created_handle(&output)
}

fn send_task(rig: &Rig, subject: &str, body: &str) -> String {
    let output = vivi([
        "task",
        "send",
        "--from",
        "mind",
        "--to",
        "hand",
        "--subject",
        subject,
        "--body",
        body,
        "--project",
        path(&rig.project),
    ]);
    assert_success(&output);
    created_handle(&output)
}

fn seed_state(rig: &Rig, need_a: &str, need_b: &str, legacy_need: &str, legacy_task: &str) {
    let mut state = State::default();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs();
    state.merges = 1;
    state.baseline = Some(Vec::new());
    state.last_gate = GateRun {
        at: now,
        merges: 1,
        tip: Some(rig.base.clone()),
        ..GateRun::default()
    };
    state.gates.push(GateRecord {
        tip: rig.base.clone(),
        at: now,
        merges: 1,
        names: Some(Vec::new()),
        new: Some(Vec::new()),
    });
    let thread_sha = format!("v2:{}", hex::encode(sha2::Sha256::digest([])));
    for (need, units) in [
        (need_a, vec![LEGACY_UNIT, SECOND_A]),
        (need_b, vec![FIRST_B, SECOND_B]),
    ] {
        state.needs.insert(
            need.to_owned(),
            NeedRecord {
                verdict: String::from("units"),
                reason: String::from("acceptance integration fixture"),
                units: units.iter().map(|unit| (*unit).to_owned()).collect(),
                revision: 1,
                thread_sha256: Some(thread_sha.clone()),
                ..NeedRecord::default()
            },
        );
    }
    state.needs.insert(
        legacy_need.to_owned(),
        NeedRecord {
            verdict: String::from("units"),
            reason: String::from("legacy closeout compatibility fixture"),
            units: vec![String::from("legacy-old")],
            ..NeedRecord::default()
        },
    );
    let mut legacy_old = unit(legacy_need, "legacy-old");
    legacy_old.revision = 0;
    legacy_old.status = Status::Merged;
    state.units.insert(String::from("legacy-old"), legacy_old);
    state.order.push(String::from("legacy-old"));
    let mut legacy = unit(need_a, LEGACY_UNIT);
    legacy.status = Status::Inflight;
    legacy.attempts = 1;
    legacy.since = now;
    legacy.since_dispatch = Some(now);
    legacy.branch = Some(LEGACY_BRANCH.to_owned());
    legacy.task = Some(legacy_task.to_owned());
    legacy.pool = Some(String::from("p"));
    legacy.model = Some(String::from("standin"));
    state.units.insert(LEGACY_UNIT.to_owned(), legacy);
    state.order.push(LEGACY_UNIT.to_owned());
    for (need, id) in [(need_a, SECOND_A), (need_b, FIRST_B), (need_b, SECOND_B)] {
        state.units.insert(id.to_owned(), unit(need, id));
        state.order.push(id.to_owned());
    }
    let store = PumpStore::open(&rig.home.join(DB_FILENAME)).unwrap();
    let mut store = store;
    store.save(&state).unwrap();
}

fn unit(need: &str, id: &str) -> Unit {
    Unit {
        need: need.to_owned(),
        target: String::from("default"),
        revision: 1,
        title: format!("complete {id}"),
        brief: format!("Create the fixture file for unit {id}."),
        write_scope: vec![unit_path(id)],
        proof: vec![String::from("true")],
        status: Status::Pending,
        ..Unit::default()
    }
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
        .env("VIVI_TEST_MARKERS", &rig.markers)
        .env("VIVI_TEST_BIN", env!("CARGO_BIN_EXE_vivi"))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    PumpChild {
        child,
        markers: rig.markers.clone(),
    }
}

fn stop_pump(rig: &Rig, child: &mut PumpChild) {
    fs::write(rig.markers.join("release-all"), "release\n").unwrap();
    assert_success(&vivi(["pump", "stop", "--project", path(&rig.project)]));
    assert!(wait_until(Duration::from_secs(15), || child
        .child
        .try_wait()
        .unwrap()
        .is_some()));
}

fn release_unit(rig: &Rig, id: &str) {
    fs::write(rig.markers.join(format!("unit-{id}.go")), "go\n").unwrap();
}

fn release_audit(rig: &Rig, need: &str) {
    fs::write(rig.markers.join(format!("audit-{need}.go")), "go\n").unwrap();
}

fn release_cleanup(rig: &Rig, need: &str) {
    fs::write(rig.markers.join(format!("cleanup-{need}.go")), "go\n").unwrap();
}

fn release_gate(rig: &Rig, job: &str) {
    fs::write(rig.markers.join(format!("gate-{job}.go")), "go\n").unwrap();
}

fn load_state(rig: &Rig) -> State {
    PumpStore::open_read_only(&rig.home.join(DB_FILENAME))
        .unwrap()
        .expect("pump.db")
        .load()
        .unwrap()
}

fn need_record(state: &State, need: &str) -> Option<serde_json::Value> {
    state
        .extra
        .get("need_branches")?
        .get("default")?
        .get(need)
        .cloned()
}

fn need_landing_receipt(state: &State, need: &str) -> Option<serde_json::Value> {
    need_record(state, need)?
        .get("landing_receipt")
        .filter(|value| !value.is_null())
        .cloned()
}

fn need_verdict_state(state: &State, need: &str) -> serde_json::Value {
    state
        .extra
        .get("need_verdicts")
        .and_then(|root| root.get("default"))
        .and_then(|target| target.get(need))
        .cloned()
        .unwrap_or_default()
}

fn unit_branch<'a>(state: &'a State, id: &str) -> Option<&'a str> {
    state.units.get(id)?.branch.as_deref()
}

fn gate_job(state: &State) -> Option<&vivi::pump::jobs::PoolJob> {
    state
        .jobs
        .values()
        .find(|job| job.kind == "gate" && job.status == JobStatus::Dispatched)
}

fn job_for_need<'a>(
    state: &'a State,
    kind: &str,
    need: &str,
) -> Option<&'a vivi::pump::jobs::PoolJob> {
    state.jobs.values().find(|job| {
        job.kind == kind
            && (job.payload.get("need").and_then(serde_json::Value::as_str) == Some(need)
                || job
                    .payload
                    .get("binding")
                    .and_then(|binding| binding.get("need"))
                    .and_then(serde_json::Value::as_str)
                    == Some(need))
    })
}

fn remote_sha(rig: &Rig, reference: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(&rig.bare)
        .args(["rev-parse", "--verify", reference])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn remote_has_path(rig: &Rig, branch: &str, file: &str) -> bool {
    let reference = format!("refs/heads/{branch}:{file}");
    Command::new("git")
        .arg("--git-dir")
        .arg(&rig.bare)
        .args(["show", &reference])
        .output()
        .is_ok_and(|out| out.status.success())
}

fn unit_path(id: &str) -> String {
    format!("unit-{id}.txt")
}

fn task_occurrences(rig: &Rig, id: &str, role: &str) -> usize {
    let output = vivi([
        "task",
        "list",
        "--for",
        role,
        "--status",
        "all",
        "--json",
        "--project",
        path(&rig.project),
    ]);
    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    count_subjects(&value, &format!("[pump-job:{id}]"))
}

fn closeout_handles(rig: &Rig) -> Vec<String> {
    let output = vivi([
        "pump",
        "closeout",
        "--json",
        "--project",
        path(&rig.project),
    ]);
    assert_success(&output);
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    rows.as_array()
        .expect("closeout JSON is not a list")
        .iter()
        .filter_map(|row| row.get("handle").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect()
}

fn count_subjects(value: &serde_json::Value, needle: &str) -> usize {
    match value {
        serde_json::Value::Object(object) => {
            let found = object
                .get("handle")
                .and_then(serde_json::Value::as_str)
                .is_some()
                && object
                    .get("subject")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|subject| subject.contains(needle));
            if found {
                1
            } else {
                object
                    .values()
                    .map(|value| count_subjects(value, needle))
                    .sum()
            }
        }
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| count_subjects(value, needle))
            .sum(),
        _ => 0,
    }
}

fn startup_diagnostics(rig: &Rig) -> String {
    let state = PumpStore::open_read_only(&rig.home.join(DB_FILENAME))
        .ok()
        .flatten()
        .and_then(|store| store.load().ok());
    let jobs = state
        .map(|state| {
            state
                .jobs
                .values()
                .map(|job| {
                    format!(
                        "{} kind={} status={:?} task={:?} applied={:?}",
                        job.id, job.kind, job.status, job.task, job.applied_at
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    format!(
        "jobs:\n{jobs}\nstatus:\n{}\npump log:\n{}",
        fs::read_to_string(rig.home.join("status.txt")).unwrap_or_default(),
        fs::read_to_string(rig.home.join("log.txt")).unwrap_or_default()
    )
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

fn vivi_with_env<I, S>(rig: &Rig, args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .env("VIVI_TEST_MARKERS", &rig.markers)
        .env("VIVI_TEST_BIN", env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

fn created_handle(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .find_map(|line| {
            let words = line.split_whitespace().collect::<Vec<_>>();
            if !matches!(words.first().copied(), Some("created" | "node")) {
                return None;
            }
            words
                .last()
                .copied()
                .filter(|handle| {
                    handle.len() == 8 && handle.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
                .map(str::to_owned)
        })
        .unwrap_or_else(|| panic!("created handle missing:\n{stdout}"))
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
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
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
