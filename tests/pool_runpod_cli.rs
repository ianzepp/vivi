//! A `runpod` pool at the command line, with no `runpodctl` on PATH and no
//! network: what must be refused, said plainly, before anything is rented.
//!
//! Every command here runs with an empty PATH, so a call that reached for the
//! RunPod CLI could not make a pod even by accident.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

const POOL: &str = r#"
[pools.gpu]
provider = "runpod"
repo = "acme/widgets"
slots = 2
image = "runpod/pytorch:2.4.0-py3.11-cuda12.4.1-runtime-ubuntu22.04"
build_dir = "target"

[pools.gpu.runpod]
api_key_file = "KEY"
gpu_types = ["NVIDIA A100 80GB PCIe"]

[pools.gpu.github]
app_id = 1
private_key = "/nonexistent.pem"

[pools.gpu.harness]
name = "fake"
run = "fake --model {model} @{brief}"
models = ["m/ok"]

[pools.gpu.committer]
name = "bot"
email = "bot@example.com"
"#;

fn project(key_mode: u32) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap().to_string();
    let plain = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_vivi"))
            .args(args)
            .output()
            .unwrap();
    };
    plain(&["mailspace", "init", "--project", &root]);
    let key = dir.path().join("runpod.key");
    std::fs::write(&key, "rpa_not_a_real_key\n").unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(key_mode)).unwrap();
    let config = dir.path().join(".vivi/mailspace.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(&POOL.replace("KEY", key.to_str().unwrap()));
    std::fs::write(&config, text).unwrap();
    (dir, root)
}

/// A PATH holding `sh`, `cat` and a stand-in `runpodctl` that answers every call with
/// an empty list and logs its arguments. It cannot make anything.
fn stub_path() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for tool in ["sh", "cat"] {
        std::os::unix::fs::symlink(format!("/bin/{tool}"), dir.path().join(tool)).unwrap();
    }
    let cli = dir.path().join("runpodctl");
    let log = dir.path().join("calls.log");
    std::fs::write(
        &cli,
        format!("#!/bin/sh\necho \"$*\" >> {}\necho '[]'\n", log.display()),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn with_stub(root: &str, args: &[&str]) -> (Output, String) {
    let stub = stub_path();
    let out = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .env("PATH", stub.path())
        .env_remove("RUNPOD_API_KEY")
        .args(args)
        .args(["--project", root])
        .output()
        .unwrap();
    let log = std::fs::read_to_string(stub.path().join("calls.log")).unwrap_or_default();
    (out, log)
}

fn vivi(root: &str, args: &[&str]) -> Output {
    let empty = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .env("PATH", empty.path())
        .env_remove("RUNPOD_API_KEY")
        .args(args)
        .args(["--project", root])
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn the_live_test_refuses_without_the_spend_flag_and_says_what_it_would_rent() {
    let (_dir, root) = project(0o600);
    let out = vivi(&root, &["pool", "doctor", "--test", "--pool", "gpu"]);
    let said = text(&out);
    assert!(!out.status.success(), "{said}");
    assert!(said.contains("would rent a real GPU pod"), "{said}");
    assert!(said.contains("--allow-spend"), "{said}");
    assert!(said.contains("Nothing was rented"), "{said}");
    assert!(said.contains("NVIDIA A100 80GB PCIe"), "{said}");
    assert!(!said.contains("rpa_not_a_real_key"), "{said}");
}

#[test]
fn the_spend_flag_means_nothing_without_the_test() {
    let (_dir, root) = project(0o600);
    let out = vivi(&root, &["pool", "doctor", "--allow-spend", "--pool", "gpu"]);
    assert!(!out.status.success());
    let said = text(&out);
    assert!(
        said.contains("--test") && said.contains("--allow-spend"),
        "{said}"
    );
}

#[test]
fn create_says_there_are_no_resting_slots() {
    let (_dir, root) = project(0o600);
    let out = vivi(&root, &["pool", "create", "--pool", "gpu"]);
    let said = text(&out);
    assert!(!out.status.success());
    assert!(
        said.contains("made per dispatch") && said.contains("none to create"),
        "{said}"
    );
    assert!(
        said.contains("pool dispatch") && said.contains("`slots` is the most that may exist"),
        "{said}"
    );
}

#[test]
fn scaling_up_is_refused_and_asks_the_provider_only_to_list() {
    let (_dir, root) = project(0o600);
    let (out, log) = with_stub(&root, &["pool", "scale", "2", "--pool", "gpu"]);
    let said = text(&out);
    assert!(!out.status.success(), "{said}");
    assert!(
        said.contains("made per dispatch") || said.contains("no resting slots"),
        "{said}"
    );
    for line in log.lines() {
        assert!(
            line.starts_with("pod list"),
            "only a listing was asked: {line}"
        );
    }
}

#[test]
fn status_counts_pods_against_the_ceiling_and_asks_only_to_list() {
    let (_dir, root) = project(0o600);
    let (out, log) = with_stub(&root, &["pool", "status", "--pool", "gpu"]);
    let said = text(&out);
    assert!(out.status.success(), "{said}");
    assert!(
        said.contains("0 pod(s) of at most 2") && said.contains("only while a job runs"),
        "{said}"
    );
    assert!(log.lines().all(|l| l.starts_with("pod list")), "{log}");
}

#[test]
fn the_doctor_fails_a_key_file_open_to_others_and_a_missing_cli_naming_the_fixes() {
    let (dir, root) = project(0o644);
    let out = vivi(&root, &["pool", "doctor", "--pool", "gpu"]);
    let said = text(&out);
    assert!(!out.status.success());
    assert!(said.contains("Provider (RunPod)"), "{said}");
    assert!(
        said.contains("readable by group or others") && said.contains("chmod 600"),
        "{said}"
    );
    assert!(said.contains("install runpodctl"), "{said}");
    assert!(
        said.contains("skipped"),
        "later checks are skipped, not guessed: {said}"
    );
    assert!(!said.contains("rpa_not_a_real_key"));
    let _ = dir;
}

#[test]
fn a_runpod_pool_needs_its_key_file_named() {
    let (dir, root) = project(0o600);
    let config = dir.path().join(".vivi/mailspace.toml");
    let text_in = std::fs::read_to_string(&config).unwrap();
    let stripped: String = text_in
        .lines()
        .filter(|l| !l.starts_with("api_key_file"))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&config, stripped).unwrap();
    let out = vivi(&root, &["pool", "doctor", "--pool", "gpu"]);
    let said = text(&out);
    assert!(!out.status.success());
    assert!(said.contains("runpod.api_key_file is required"), "{said}");
    let _ = Path::new("");
}
