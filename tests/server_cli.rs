//! The binary's command line. The server boots on any argument it does not
//! understand, so each case here runs the real binary under a deadline: a
//! binary that started serving would never exit and fails the test.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Run the binary with exactly `vars` added (and no `DATA_DIR` unless one is
/// given), under a deadline.
fn wait(args: &[&str], vars: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vivi-server"));
    command
        .args(args)
        .env_remove("DATA_DIR")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in vars {
        command.env(name, value);
    }
    let mut child = command.spawn().expect("spawn vivi-server");
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().expect("poll vivi-server").is_none() {
        if Instant::now() >= deadline {
            child.kill().expect("kill vivi-server");
            panic!("vivi-server {args:?} did not exit: it started serving");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child
        .wait_with_output()
        .expect("collect vivi-server output")
}

fn scratch_data_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("vivi-server-cli-{tag}-{}", std::process::id()))
}

fn run(args: &[&str]) -> Output {
    let data = scratch_data_dir("args");
    let out = wait(
        args,
        &[
            ("PORT", "0"),
            ("DATA_DIR", data.to_str().expect("utf-8 path")),
        ],
    );
    assert!(
        !data.exists(),
        "vivi-server {args:?} created its data directory"
    );
    out
}

#[test]
fn version_prints_name_and_crate_version() {
    for flag in ["--version", "-V"] {
        let out = run(&[flag]);
        assert!(out.status.success(), "{flag}: {:?}", out.status);
        let line = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            line.trim_end(),
            format!("vivi-server {}", env!("CARGO_PKG_VERSION"))
        );
    }
}

#[test]
fn help_prints_usage_and_environment() {
    for flag in ["--help", "-h"] {
        let out = run(&[flag]);
        assert!(out.status.success(), "{flag}: {:?}", out.status);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("Usage: vivi-server"), "{text}");
        assert!(text.contains("STORE_ENCRYPTION_KEY"), "{text}");
    }
}

#[test]
fn an_unknown_argument_exits_2_without_serving() {
    let out = run(&["--halp"]);
    assert_eq!(out.status.code(), Some(2));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("unrecognized argument: --halp"), "{text}");
    assert!(text.contains("--help"), "{text}");
    assert!(out.stdout.is_empty());
}

#[test]
fn serving_without_a_data_dir_exits_2() {
    let out = wait(&[], &[("PORT", "0")]);
    assert_eq!(out.status.code(), Some(2));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("DATA_DIR is required"), "{text}");
}

#[test]
fn a_relative_data_dir_exits_2_and_is_not_created() {
    let out = wait(&[], &[("PORT", "0"), ("DATA_DIR", "relative-data")]);
    assert_eq!(out.status.code(), Some(2));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("DATA_DIR must be an absolute path"), "{text}");
    assert!(!std::path::Path::new("relative-data").exists());
}

#[test]
fn an_address_already_in_use_exits_1_before_the_data_dir_is_touched() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port");
    let port = taken.local_addr().expect("local addr").port().to_string();
    let data = scratch_data_dir("taken");
    let out = wait(
        &[],
        &[
            ("BIND", "127.0.0.1"),
            ("PORT", &port),
            ("DATA_DIR", data.to_str().expect("utf-8 path")),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("cannot bind 127.0.0.1"), "{text}");
    assert!(
        !text.contains("panicked"),
        "a bind failure must be an error exit, not a panic: {text}"
    );
    assert!(!data.exists(), "the data directory was created before bind");
}
