//! Merge-role workers handle real git conflicts and failing checks.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use vivi::pump::jobs::JobStatus;
use vivi::pump::state::{GateRun, State, Status, Unit};
use vivi::pump::store::{DB_FILENAME, PumpStore};

const UNIT: &str = "merge-agent-u";
const BRANCH: &str = "unit/merge-agent-u";
const WORKER: &str = r#"#!/usr/bin/env python3
import json, os, pathlib, re, subprocess, sys

body = pathlib.Path(sys.argv[1]).read_text()
kind = (re.search(r"(?m)^pump-job-kind: (.+)$", body) or type("m", (), {"group": lambda *_: "hand"})()).group(1).strip()

def run(*args, check=True):
    return subprocess.run(["git", *args], check=check, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE)

def git_output(*args):
    return run(*args).stdout.strip()

def read_json_after(marker):
    return json.JSONDecoder().raw_decode(body.split(marker, 1)[1].lstrip())[0]

def merge_ref(ref):
    last = None
    for candidate in (ref, f"origin/{ref}", f"refs/heads/{ref}"):
        proc = run("merge", "--no-edit", candidate, check=False)
        text = proc.stdout + proc.stderr
        if proc.returncode == 0:
            return proc, text, False
        if "CONFLICT" in text or "Automatic merge failed" in text:
            return proc, text, True
        last = (proc, text)
    proc, text = last
    return proc, text, False

def unmerged():
    proc = run("diff", "--name-only", "--diff-filter=U", check=False)
    return [line for line in proc.stdout.splitlines() if line.strip()]

def commit(message):
    run("-c", "user.name=Merge Agent Test", "-c",
        "user.email=merge-agent@localhost", "commit", "--no-edit", "-q", "-m", message)

if kind != "merge":
    raise RuntimeError(f"unexpected worker kind {kind!r}")

payload = read_json_after("Input JSON:\n")
sources = []
for source in payload.get("unit_sources", []):
    ref = source.get("branch") or source["id"]
    scope = set(source.get("write_scope") or [])
    proc, text, conflicted = merge_ref(ref)
    if conflicted:
        paths = unmerged()
        outside = [path for path in paths if path not in scope]
        if outside:
            run("merge", "--abort", check=False)
            sources.append({"kind": "unit", "id": source["id"], "status": "dropped",
                "why": "conflict", "detail": "conflict outside write scope: " + ",".join(outside),
                "outside": outside, "secs": 0})
            continue
        for path in paths:
            run("checkout", "--theirs", "--", path)
            run("add", "--", path)
        commit("resolve merge conflict in write scope")
        sources.append({"kind": "unit", "id": source["id"], "status": "merged", "why": "",
            "detail": "diagnosed and resolved conflict on " + ",".join(paths), "outside": [], "secs": 0})
    elif proc.returncode != 0:
        sources.append({"kind": "unit", "id": source["id"], "status": "dropped",
            "why": "conflict", "detail": text.strip()[:400], "outside": [], "secs": 0})
    else:
        sources.append({"kind": "unit", "id": source["id"], "status": "merged", "why": "",
            "detail": "merged cleanly", "outside": [], "secs": 0})

dropped = False
recoveries = []
for command in payload.get("shared_checks", []):
    first = subprocess.run(command, shell=True, text=True, capture_output=True)
    if first.returncode == 0:
        continue
    retry = subprocess.run(command, shell=True, text=True, capture_output=True)
    if retry.returncode == 0:
        recoveries.append({"name": "", "command": command,
            "detail": (first.stderr or first.stdout or "first run failed").strip()[:200]})
        continue
    detail = (first.stderr or first.stdout or command).strip()[:400]
    for source in sources:
        source["status"] = "dropped"
        source["why"] = "check-failed"
        source["detail"] = f"diagnosed failing check {command!r}: {detail}"
    dropped = True
    break

if not dropped:
    for source in payload.get("unit_sources", []):
        for command in source.get("proof") or []:
            subprocess.run(command, shell=True, check=True)

result = {"version": 1, "target": payload["target"], "tip": git_output("rev-parse", "HEAD"),
    "sources": sources, "shared_check_timeouts": 0, "shared_check_timeout_retries": 0,
    "shared_check_recoveries": recoveries}
print("PUMP_MERGE_RESULT=" + json.dumps(result, separators=(",", ":")))
"#;

struct Rig {
    _dir: tempfile::TempDir,
    project: PathBuf,
    home: PathBuf,
    repo: PathBuf,
    child_log: PathBuf,
    tip: String,
}

struct PumpChild {
    child: Child,
}

impl Drop for PumpChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn merge_agent_resolves_a_real_conflict_inside_write_scope() {
    let rig = scaffold("true");
    push_conflicting_unit(&rig);
    seed_delivered(&rig);
    let mut pump = pump_child(&rig);
    assert!(
        wait_until(Duration::from_secs(90), || {
            let state = load_state(&rig);
            state.units[UNIT].status == Status::Merged
        }),
        "conflict was not resolved through the merge agent: {}",
        diagnostics(&rig)
    );
    stop_pump(&rig, &mut pump);
    let state = load_state(&rig);
    let job = state
        .jobs
        .values()
        .find(|job| job.kind == "merge")
        .expect("merge job");
    let receipt = job.outcome.as_ref().expect("merge outcome").receipt.clone();
    assert!(
        receipt.contains("diagnosed and resolved conflict"),
        "missing conflict diagnosis: {receipt}"
    );
    assert!(
        remote_has_path(&rig, "next", "conflict.txt"),
        "resolved tree was not published"
    );
}

#[test]
fn merge_agent_diagnoses_a_failing_shared_check_on_real_branches() {
    let rig = scaffold("sh -c 'echo boom-from-shared-check >&2; exit 1'");
    push_clean_unit(&rig);
    seed_delivered(&rig);
    let mut pump = pump_child(&rig);
    assert!(
        wait_until(Duration::from_secs(90), || {
            load_state(&rig).jobs.values().any(|job| {
                job.kind == "merge"
                    && job.status == JobStatus::Finished
                    && job.outcome.as_ref().is_some_and(|outcome| {
                        outcome.receipt.contains("check-failed")
                            && outcome.receipt.contains("boom-from-shared-check")
                    })
            })
        }),
        "failing check was not collected: {}",
        diagnostics(&rig)
    );
    stop_pump(&rig, &mut pump);
    let state = load_state(&rig);
    assert_ne!(state.units[UNIT].status, Status::Merged);
    let job = state
        .jobs
        .values()
        .find(|job| job.kind == "merge")
        .expect("merge job");
    let receipt = job.outcome.as_ref().expect("merge outcome").receipt.clone();
    assert!(
        receipt.contains("check-failed") && receipt.contains("boom-from-shared-check"),
        "missing failing-check diagnosis: {receipt}"
    );
}

fn scaffold(shared_check: &str) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let project = base.join("project");
    fs::create_dir(&project).unwrap();
    assert_success(&vivi(["mailspace", "init", "--project", path(&project)]));

    let bare = base.join("origin.git");
    let seed = base.join("seed");
    fs::create_dir(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    git(&seed, &["config", "user.name", "Merge Agent Test"]);
    git(&seed, &["config", "user.email", "merge-agent@localhost"]);
    fs::write(seed.join("README.md"), "merge agent fixture\n").unwrap();
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
    let worker = base.join("worker.py");
    fs::write(&worker, WORKER).unwrap();
    write_config(
        &project,
        &home,
        &repo,
        &bare,
        &base.join("lanes"),
        &worker,
        shared_check,
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
        repo,
        child_log: base.join("pump-child.log"),
        tip,
    }
}

fn write_config(
    project: &Path,
    home: &Path,
    repo: &Path,
    bare: &Path,
    lanes: &Path,
    worker: &Path,
    shared_check: &str,
) {
    let config_path = project.join(".vivi/mailspace.toml");
    let old = fs::read_to_string(&config_path).unwrap();
    // `mailspace init` writes a starter [pump]; this test supplies its own.
    let base_config = old
        .replace("identities = []\n", "")
        .replace(vivi::mailspace::STARTER_PUMP, "");
    let additions = format!(
        "\n[pump]\nhome = {}\nrepo = {}\nremote = \"origin\"\nremote_url = {}\nbranch = \"next\"\nmain = \"main\"\npromote = false\ntick = 1\ncommitter_name = \"Merge Agent Test\"\ncommitter_email = \"merge-agent@localhost\"\nfrom = \"mind\"\nto = \"hand\"\nreply_from = \"mind\"\n\n[pump.board]\nfrom = []\npull = false\n\n[pump.dispatch]\ndefault_kind = \"unit\"\nper_tick = 4\nlanes = [{{ model = \"standin\", cap = 8 }}]\n\n[pump.dispatch.kinds.unit]\npools = [\"p\"]\n\n[pump.gate]\ncommand = \"true\"\npools = [\"p\"]\n\n[pump.merge]\nshared_checks = [{shared_check:?}]\n\n[pools.p]\nprovider = \"local\"\nslots = 4\n\n[pools.p.local]\nroot = {}\nworktree_of = {}\n\n[pools.p.harness]\nname = \"merge-agent\"\nrun = \"python3 '{}' {{brief}}\"\nmodels = [\"standin\"]\n\n[[pools.p.bands.P2]]\nmodel = \"standin\"\nreasoning = \"low\"\n\n[pools.p.committer]\nname = \"Merge Agent Test\"\nemail = \"merge-agent@localhost\"\n",
        toml_string(home),
        toml_string(repo),
        toml_string(bare),
        toml_string(lanes),
        toml_string(repo),
        worker.display(),
    );
    fs::write(config_path, format!("{base_config}{additions}")).unwrap();
}

fn push_conflicting_unit(rig: &Rig) {
    git(&rig.repo, &["checkout", "-q", "next"]);
    fs::write(rig.repo.join("conflict.txt"), "original\n").unwrap();
    git(&rig.repo, &["add", "conflict.txt"]);
    git(&rig.repo, &["commit", "-q", "-m", "shared conflict base"]);
    git(&rig.repo, &["push", "-q", "origin", "next"]);
    git(&rig.repo, &["checkout", "-q", "-B", BRANCH]);
    fs::write(rig.repo.join("conflict.txt"), "unit side\n").unwrap();
    git(&rig.repo, &["add", "conflict.txt"]);
    git(&rig.repo, &["commit", "-q", "-m", "unit conflict side"]);
    git(&rig.repo, &["push", "-q", "-u", "origin", BRANCH]);
    git(&rig.repo, &["checkout", "-q", "next"]);
    fs::write(rig.repo.join("conflict.txt"), "target side\n").unwrap();
    git(&rig.repo, &["add", "conflict.txt"]);
    git(&rig.repo, &["commit", "-q", "-m", "target conflict side"]);
    git(&rig.repo, &["push", "-q", "origin", "next"]);
}

fn push_clean_unit(rig: &Rig) {
    git(&rig.repo, &["checkout", "-q", "-B", BRANCH, "origin/next"]);
    fs::write(rig.repo.join("ok.txt"), "unit ok\n").unwrap();
    git(&rig.repo, &["add", "ok.txt"]);
    git(&rig.repo, &["commit", "-q", "-m", "clean unit"]);
    git(&rig.repo, &["push", "-q", "-u", "origin", BRANCH]);
    git(&rig.repo, &["checkout", "-q", "next"]);
}

fn seed_delivered(rig: &Rig) {
    let mut state = State::default();
    state.baseline = Some(Vec::new());
    state.last_gate = GateRun {
        merges: 0,
        tip: Some(rig.tip.clone()),
        ..GateRun::default()
    };
    state.units.insert(
        UNIT.to_owned(),
        Unit {
            need: String::new(),
            title: String::from("merge agent unit"),
            brief: String::from("exercise the merge agent"),
            write_scope: vec![String::from("conflict.txt"), String::from("ok.txt")],
            status: Status::Delivered,
            branch: Some(BRANCH.to_owned()),
            proof: vec![String::from("true")],
            ..Unit::default()
        },
    );
    state.order.push(UNIT.to_owned());
    PumpStore::open(&rig.home.join(DB_FILENAME))
        .unwrap()
        .save(&state)
        .unwrap();
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
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    PumpChild { child }
}

fn stop_pump(rig: &Rig, child: &mut PumpChild) {
    let stopped = vivi(["pump", "stop", "--project", path(&rig.project)]);
    assert_success(&stopped);
    assert!(wait_until(Duration::from_secs(10), || child
        .child
        .try_wait()
        .unwrap()
        .is_some()));
}

fn load_state(rig: &Rig) -> State {
    PumpStore::open_read_only(&rig.home.join(DB_FILENAME))
        .unwrap()
        .expect("pump.db")
        .load()
        .unwrap()
}

fn remote_has_path(rig: &Rig, branch: &str, path: &str) -> bool {
    Command::new("git")
        .args([
            "-C",
            &rig.repo.to_string_lossy(),
            "cat-file",
            "-e",
            &format!("origin/{branch}:{path}"),
        ])
        .status()
        .unwrap()
        .success()
}

fn diagnostics(rig: &Rig) -> String {
    format!(
        "status={}\nlog={}\nchild={}\nstate units={:?} jobs={:?}",
        fs::read_to_string(rig.home.join("status.txt")).unwrap_or_default(),
        fs::read_to_string(rig.home.join("log.txt")).unwrap_or_default(),
        fs::read_to_string(&rig.child_log).unwrap_or_default(),
        load_state(rig)
            .units
            .iter()
            .map(|(id, unit)| format!("{id}={:?}", unit.status))
            .collect::<Vec<_>>(),
        load_state(rig)
            .jobs
            .values()
            .map(|job| format!("{} {:?} {:?}", job.id, job.status, job.task))
            .collect::<Vec<_>>(),
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
