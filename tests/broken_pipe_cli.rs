//! Broken-pipe pipelines: a reader that exits early must not fail vivi.
//!
//! Rust ignores `SIGPIPE`, so before the quiet-pipe panic hook a command
//! whose stdout reader closed early (`... | head -1`) panicked in `println!`
//! and exited 101 with a panic on stderr. These tests pin the contract:
//! exit 0, empty stderr — while real errors keep their message and exit
//! nonzero. The reader side is done in Rust (std::process), not by shelling
//! out to `head`.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use tempfile::tempdir;

/// Far larger than the 64 KiB pipe buffer, so the child is guaranteed to
/// still have output pending when the read end is dropped and must observe
/// EPIPE instead of finishing first.
const LARGE_BODY_BYTES: usize = 200 * 1024;

#[test]
fn skills_show_piped_to_early_reader_exits_zero_silently() {
    // Static, mailspace-free output (~107 KB), so no setup is needed.
    let (status, stderr) = drop_reader_after_first_line(&["skills", "show", "vivi"]);
    assert_exit_zero_silently(&status, &stderr);
}

#[test]
fn mailspace_need_show_piped_to_early_reader_exits_zero_silently() {
    let dir = tempdir().unwrap();
    let handle = mailspace_with_large_need(dir.path());
    let args = [
        "need",
        "show",
        &handle,
        "--project",
        dir.path().to_str().unwrap(),
    ];
    let (status, stderr) = drop_reader_after_first_line(&args);
    assert_exit_zero_silently(&status, &stderr);
}

#[test]
fn non_pipe_errors_keep_their_message_and_nonzero_exit() {
    let dir = tempdir().unwrap();
    setup_mailspace(dir.path());

    let output = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args([
            "need",
            "show",
            "nosuchhandle",
            "--project",
            dir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "unknown handle must fail; stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error:"), "{stderr}");
}

fn assert_exit_zero_silently(status: &ExitStatus, stderr: &str) {
    assert_eq!(
        status.code(),
        Some(0),
        "expected exit 0, got {status}; stderr:\n{stderr}"
    );
    assert!(stderr.is_empty(), "stderr must be empty:\n{stderr}");
}

/// Spawn vivi with piped stdout/stderr, read one line of its stdout, drop the
/// pipe read end, then drain stderr and wait. Returns the exit status and the
/// full stderr, which for a quiet broken pipe must be empty.
fn drop_reader_after_first_line(args: &[&str]) -> (ExitStatus, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut first_line = String::new();
    let read = reader.read_line(&mut first_line).unwrap();
    assert!(
        read > 0,
        "vivi closed stdout before writing a line: {args:?}"
    );
    drop(reader); // closes the read end; vivi's next stdout write hits EPIPE

    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let status = child.wait().unwrap();
    (status, stderr)
}

/// Initialize a mailspace with ceo/cto and send a need whose body exceeds the
/// pipe buffer. Returns the handle that `need show` accepts.
fn mailspace_with_large_need(project: &Path) -> String {
    setup_mailspace(project);

    let body = project.join("large-need-body.txt");
    fs::write(&body, "x".repeat(LARGE_BODY_BYTES)).unwrap();

    let sent = run_ok(&[
        "need",
        "send",
        "--project",
        project.to_str().unwrap(),
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "large body for the broken-pipe test",
        "--body-file",
        body.to_str().unwrap(),
    ]);
    let line = sent
        .lines()
        .find(|line| line.starts_with("sent "))
        .unwrap_or_else(|| panic!("need send printed no sent receipt:\n{sent}"));
    line.split_whitespace()
        .last()
        .unwrap_or_else(|| panic!("sent receipt without a handle:\n{sent}"))
        .to_string()
}

fn setup_mailspace(project: &Path) {
    run_ok(&["mailspace", "init", "--project", project.to_str().unwrap()]);
    for identity in ["ceo", "cto"] {
        run_ok(&[
            "mailspace",
            "identity",
            "add",
            identity,
            "--project",
            project.to_str().unwrap(),
        ]);
    }
}

fn run_ok(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "vivi {args:?} failed ({})\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}
