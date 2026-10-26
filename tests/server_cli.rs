//! The binary's command line. The server boots on any argument it does not
//! understand, so each case here runs the real binary under a deadline: a
//! binary that started serving would never exit and fails the test.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn run(args: &[&str]) -> Output {
    let data = std::env::temp_dir().join(format!("vivi-server-cli-{}", std::process::id()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_vivi-server"))
        .args(args)
        .env("PORT", "0")
        .env("DATA_DIR", &data)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn vivi-server");
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().expect("poll vivi-server").is_none() {
        if Instant::now() >= deadline {
            child.kill().expect("kill vivi-server");
            panic!("vivi-server {args:?} did not exit: it started serving");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = child
        .wait_with_output()
        .expect("collect vivi-server output");
    assert!(
        !data.exists(),
        "vivi-server {args:?} created its data directory"
    );
    output
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
