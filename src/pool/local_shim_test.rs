//! The stand-in `timeout`, `sha256sum` and `nproc` a local slot carries, run
//! on this machine with its own bash.

use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

fn shim(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/pool/local-bin")
        .join(name)
}

fn run(name: &str, args: &[&str]) -> (i32, String) {
    let out = Command::new("bash")
        .arg(shim(name))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn timeout_passes_the_exit_code_of_a_command_that_ends_in_time() {
    assert_eq!(run("timeout", &["5", "true"]).0, 0);
    assert_eq!(
        run("timeout", &["5", "bash", "-c", "echo hi; exit 7"]),
        (7, "hi\n".to_string())
    );
    assert_eq!(run("timeout", &["-k", "1", "5", "true"]).0, 0);
    assert_eq!(run("timeout", &["5"]).0, 125, "no command is a usage error");
}

#[test]
fn timeout_ends_a_command_and_what_it_started_and_says_124() {
    let started = Instant::now();
    let (code, _) = run("timeout", &["1", "bash", "-c", "sleep 30 & sleep 31"]);
    assert_eq!(code, 124);
    assert!(
        started.elapsed().as_secs() < 10,
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn timeout_escalates_to_kill_for_a_command_that_ignores_term() {
    let (code, _) = run(
        "timeout",
        &[
            "-k",
            "1",
            "1",
            "bash",
            "-c",
            "trap '' TERM; while :; do sleep 1; done",
        ],
    );
    assert_eq!(code, 137);
}

#[test]
fn sha256sum_and_nproc_answer_like_the_tools_they_stand_in_for() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f");
    std::fs::write(&file, "abc").unwrap();
    let (code, out) = run("sha256sum", &[file.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(
        out.starts_with("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  "),
        "{out}"
    );
    let (code, out) = run("nproc", &[]);
    assert_eq!(code, 0);
    assert!(out.trim().parse::<u32>().unwrap() >= 1);
}
