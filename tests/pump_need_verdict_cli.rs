//! Frozen need-audit receipts route residual, block_range, and block_ship.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use vivi::pump::jobs::JobStatus;
use vivi::pump::need;
use vivi::pump::need_verdict::{self, EXTRA_KEY};
use vivi::pump::state::{NeedRecord, State, Status, Unit};
use vivi::pump::store::{DB_FILENAME, PumpStore};

const WORKER: &str = r#"#!/usr/bin/env python3
import json, os, pathlib, re, sys

body = pathlib.Path(sys.argv[1]).read_text()
kind = (re.search(r"(?m)^pump-job-kind: (.+)$", body) or type("m", (), {"group": lambda *_: ""})()).group(1).strip()
markers = pathlib.Path(os.environ["VIVI_TEST_MARKERS"])

def read_json_after(marker):
    return json.JSONDecoder().raw_decode(body.split(marker, 1)[1].lstrip())[0]

if kind != "need-audit":
    print(f"ignored {kind}")
    sys.exit(0)

identity = re.search(
    r"Dispatch identity: job_id=([^;]+); receipt_key=([^;]+); need=([^;]+); "
    r"target=([^;]+); revision=(\d+); frozen_binding=([0-9a-f]+)\.\.([0-9a-f]+); "
    r"source_hash=[^;]+; review_range=([0-9a-f]+)\.\.([0-9a-f]+)\. Round: (\d+)", body)
if not identity:
    raise RuntimeError("need audit brief has no frozen identity")
job, key, need, target, revision, base, tip, audit_base, _, round_no = identity.groups()
packet = read_json_after("Audit packet JSON:\n")
verdict = (markers / f"{need}.verdict").read_text().strip()
requirements = [{"id": item["id"], "text": item["text"], "status": "SATISFIED",
    "evidence": ["fixture"]} for item in packet["requirements"]]
findings = []
diagnoses = []
if verdict == "residual":
    findings = [{"id": "finding-residual", "content": "a leftover follow-up",
        "paths": ["left.txt"], "recheck_trigger": "the leftover is filed"}]
elif verdict == "block_range":
    findings = [{"id": "finding-range", "content": "a scoped defect remains",
        "paths": ["src/a.rs"], "recheck_trigger": "re-review the repair diff"}]
    diagnoses = ["blocked range"]
elif verdict == "block_ship":
    findings = [{"id": "finding-ship", "content": "cannot ship this range",
        "paths": ["src/a.rs"], "recheck_trigger": "operator disposition"}]
    diagnoses = ["ship is blocked"]
    requirements = [{"id": item["id"], "text": item["text"], "status": "UNVERIFIED",
        "evidence": ["fixture"]} for item in packet["requirements"]]
else:
    raise RuntimeError(f"unknown verdict {verdict}")
result = {"version": 1, "receipt_key": key, "job_id": job,
    "binding": {"need": need, "target": target, "revision": int(revision), "base": base, "tip": tip},
    "source_hash": packet["source_hash"], "audit_base": audit_base, "round": int(round_no),
    "verdict": verdict, "requirements": requirements, "findings": findings,
    "diagnoses": diagnoses, "blind_spots": [],
    "report": {"id": f"report-{job}", "reference": f"verdict-report/{job}"}}
print("PUMP_NEED_AUDIT_RESULT=" + json.dumps(result, separators=(",", ":")), flush=True)
"#;

struct Rig {
    _dir: tempfile::TempDir,
    project: PathBuf,
    home: PathBuf,
    repo: PathBuf,
    markers: PathBuf,
    child_log: PathBuf,
    tip: String,
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
fn frozen_need_audits_route_residual_block_range_and_block_ship() {
    let rig = scaffold();
    let residual = send_need(&rig, "residual need");
    let range = send_need(&rig, "block-range need");
    let ship = send_need(&rig, "block-ship need");
    fs::write(
        rig.markers.join(format!("{residual}.verdict")),
        "residual\n",
    )
    .unwrap();
    fs::write(
        rig.markers.join(format!("{range}.verdict")),
        "block_range\n",
    )
    .unwrap();
    fs::write(rig.markers.join(format!("{ship}.verdict")), "block_ship\n").unwrap();
    freeze_need(&rig, &residual, "u-residual");
    freeze_need(&rig, &range, "u-range");
    freeze_need(&rig, &ship, "u-ship");

    let mut pump = pump_child(&rig);
    assert!(
        wait_until(Duration::from_secs(90), || routed(
            &rig, &residual, "residual"
        ) && routed(
            &rig,
            &range,
            "block_range"
        ) && routed(
            &rig,
            &ship,
            "block_ship"
        )),
        "verdicts were not routed: {}",
        diagnostics(&rig)
    );
    stop_pump(&rig, &mut pump);

    let state = load_state(&rig);
    let residual_state = verdict(&state, &residual);
    assert!(residual_state["accepted"].is_object(), "{residual_state}");
    assert!(
        !residual_state["residual_needs"]
            .as_object()
            .unwrap()
            .is_empty(),
        "{residual_state}"
    );

    let range_state = verdict(&state, &range);
    assert!(range_state["accepted"].is_null());
    assert!(
        !range_state["repair_units"].as_object().unwrap().is_empty(),
        "{range_state}"
    );
    assert!(range_state["hold"].is_null(), "{range_state}");

    let ship_state = verdict(&state, &ship);
    assert!(ship_state["hold"].is_object(), "{ship_state}");
    assert!(ship_state["accepted"].is_null());
}

fn routed(rig: &Rig, need: &str, expected: &str) -> bool {
    let state = load_state(rig);
    state.jobs.values().any(|job| {
        job.kind == "need-audit"
            && job.status == JobStatus::Finished
            && job.applied_at.is_some()
            && job
                .payload
                .pointer("/binding/need")
                .and_then(|value| value.as_str())
                == Some(need)
            && verdict(&state, need)["receipts"]
                .as_object()
                .is_some_and(|receipts| {
                    receipts
                        .values()
                        .any(|receipt| receipt["verdict"] == expected)
                })
    })
}

fn verdict(state: &State, need: &str) -> serde_json::Value {
    need_verdict::state(state, "default", need)
        .ok()
        .and_then(|value| serde_json::to_value(value).ok())
        .unwrap_or(serde_json::json!({}))
}

fn freeze_need(rig: &Rig, need_id: &str, unit_id: &str) {
    git(
        &rig.repo,
        &[
            "push",
            "-q",
            "origin",
            &format!("{}:refs/heads/vivi/need/{need_id}", rig.tip),
        ],
    );
    let mut state = load_state(rig);
    state.needs.insert(
        need_id.to_owned(),
        NeedRecord {
            verdict: String::from("units"),
            units: vec![unit_id.to_owned()],
            revision: 1,
            thread_sha256: Some(source_hash_for(rig, need_id)),
            ..NeedRecord::default()
        },
    );
    state.units.insert(
        unit_id.to_owned(),
        Unit {
            need: need_id.to_owned(),
            target: String::from("default"),
            revision: 1,
            title: format!("complete {unit_id}"),
            brief: String::from("frozen verdict fixture"),
            write_scope: vec![String::from("src/a.rs")],
            status: Status::Merged,
            commit: Some(rig.tip.clone()),
            ..Unit::default()
        },
    );
    state.order.push(unit_id.to_owned());
    need::reserve(&mut state, "default", need_id, 1, &rig.tip).unwrap();
    need::record_create_job(&mut state, "default", need_id, "init").unwrap();
    need::record_created(&mut state, "default", need_id, "init", &rig.tip, &rig.tip).unwrap();
    need::freeze(&mut state, "default", need_id, 1, &rig.tip).unwrap();
    PumpStore::open(&rig.home.join(DB_FILENAME))
        .unwrap()
        .save(&state)
        .unwrap();
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
    git(&seed, &["config", "user.name", "Need Verdict Test"]);
    git(&seed, &["config", "user.email", "need-verdict@localhost"]);
    fs::write(seed.join("README.md"), "need verdict fixture\n").unwrap();
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
    let worker = base.join("worker.py");
    fs::write(&worker, WORKER).unwrap();
    write_config(&project, &home, &repo, &bare, &base.join("lanes"), &worker);
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
    PumpStore::open(&home.join(DB_FILENAME))
        .unwrap()
        .save(&State::default())
        .unwrap();
    Rig {
        _dir: dir,
        project,
        home,
        repo,
        markers,
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
) {
    let config_path = project.join(".vivi/mailspace.toml");
    let old = fs::read_to_string(&config_path).unwrap();
    let base_config = old.replace("identities = []\n", "");
    let additions = format!(
        "\n[pump]\nhome = {}\nrepo = {}\nremote = \"origin\"\nremote_url = {}\nbranch = \"next\"\nmain = \"main\"\npromote = false\ntick = 1\ncommitter_name = \"Need Verdict Test\"\ncommitter_email = \"need-verdict@localhost\"\nfrom = \"mind\"\nto = \"hand\"\nreply_from = \"mind\"\n\n[pump.board]\nmark = \"\"\nfrom = []\npull = false\n\n[pump.dispatch]\ndefault_kind = \"unit\"\nper_tick = 4\nlanes = [{{ model = \"standin\", cap = 8 }}]\n\n[pump.gate]\ncommand = \"true\"\npools = [\"p\"]\n\n[pump.merge]\nshared_checks = [\"true\"]\n\n[pump.need]\nrepair_rounds = 1\n\n[pools.p]\nprovider = \"local\"\nslots = 6\n\n[pools.p.local]\nroot = {}\nworktree_of = {}\n\n[pools.p.harness]\nname = \"need-verdict\"\nrun = \"python3 '{}' {{brief}}\"\nmodels = [\"standin\"]\nenv = [\"VIVI_TEST_MARKERS\"]\n\n[[pools.p.bands.P2]]\nmodel = \"standin\"\nreasoning = \"low\"\n\n[pools.p.committer]\nname = \"Need Verdict Test\"\nemail = \"need-verdict@localhost\"\n",
        toml_string(home),
        toml_string(repo),
        toml_string(bare),
        toml_string(lanes),
        toml_string(repo),
        worker.display(),
    );
    fs::write(config_path, format!("{base_config}{additions}")).unwrap();
}

fn source_hash_for(rig: &Rig, need: &str) -> String {
    let output = vivi([
        "need",
        "show",
        need,
        "--newest",
        &usize::MAX.to_string(),
        "--json",
        "--project",
        path(&rig.project),
    ]);
    assert_success(&output);
    let text = String::from_utf8_lossy(&output.stdout);
    let start = text.find('[').expect("need show JSON");
    let messages: Vec<serde_json::Value> = serde_json::from_str(&text[start..]).unwrap();
    let mut hasher = Sha256::new();
    for message in &messages {
        if message["event"].as_str().is_some() {
            continue;
        }
        let message_id = message["message_id"].as_str().unwrap_or("");
        for field in [
            message["handle"].as_str().unwrap_or(""),
            message_id,
            message["from"].as_str().unwrap_or(""),
            message["subject"].as_str().unwrap_or(""),
            message["body"].as_str().unwrap_or(""),
        ] {
            hasher.update((field.len() as u64).to_be_bytes());
            hasher.update(field.as_bytes());
        }
    }
    format!("v4:{}", hex::encode(hasher.finalize()))
}

fn send_need(rig: &Rig, subject: &str) -> String {
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
        "done_when: the frozen range is audited\n",
        "--project",
        path(&rig.project),
    ]);
    assert_success(&output);
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .find(|word| word.len() == 8 && word.chars().all(|ch| ch.is_ascii_hexdigit()))
        .unwrap_or_else(|| panic!("no handle in {text}"))
        .to_owned()
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
    assert_success(&vivi(["pump", "stop", "--project", path(&rig.project)]));
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

fn diagnostics(rig: &Rig) -> String {
    format!(
        "extra={}\nlog={}\nchild={}\njobs={:?}",
        load_state(rig)
            .extra
            .get(EXTRA_KEY)
            .cloned()
            .unwrap_or(serde_json::json!({})),
        fs::read_to_string(rig.home.join("log.txt")).unwrap_or_default(),
        fs::read_to_string(&rig.child_log).unwrap_or_default(),
        load_state(rig)
            .jobs
            .values()
            .map(|job| format!("{} {} {:?}", job.kind, job.id, job.status))
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
