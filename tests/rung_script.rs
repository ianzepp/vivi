//! Integration test for the actual `scripts/rung` shell script.
//!
//! A temporary executable `cargo` shim is placed first on the spawned
//! child's `PATH` (never the test process's own environment). The shim
//! records each invocation's argument list and fails selected invocation
//! indices so failure propagation can be observed without running any real
//! Cargo build.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn rung_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/rung")
}

fn shim_body() -> String {
    r#"#!/bin/sh
printf '%s\n' "$*" >> "$RUNG_SHIM_LOG"
n=0
if [ -f "$RUNG_SHIM_COUNT" ]; then
  read n < "$RUNG_SHIM_COUNT"
fi
n=$((n + 1))
printf '%s\n' "$n" > "$RUNG_SHIM_COUNT"
case ",${RUNG_SHIM_FAIL_ON:-}," in
  *",$n,"*) exit 1 ;;
esac
exit 0
"#
    .to_string()
}

/// Runs `scripts/rung <case>` with the shim directory first on the child's
/// PATH. `fail_on` is a comma-separated list of 1-based invocation indices
/// the shim fails. Returns the child's exit code and the recorded argument
/// lists, one per invocation.
fn run_rung(case: &str, fail_on: Option<&str>) -> (Option<i32>, Vec<String>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let shim = dir.path().join("cargo");
    fs::write(&shim, shim_body()).expect("write cargo shim");
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).expect("chmod shim");
    let log = dir.path().join("invocations.log");
    let count = dir.path().join("count");
    let mut cmd = Command::new(&rung_path());
    cmd.arg(case);
    // PATH applies to this child only; the shim dir shadows any real cargo
    // while /usr/bin and /bin keep bash and the shebang resolvable.
    cmd.env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()));
    cmd.env("RUNG_SHIM_LOG", &log);
    cmd.env("RUNG_SHIM_COUNT", &count);
    if let Some(fail_on) = fail_on {
        cmd.env("RUNG_SHIM_FAIL_ON", fail_on);
    }
    let output = cmd.output().expect("spawn scripts/rung");
    let invocations = fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    (output.status.code(), invocations)
}

#[test]
fn need_rung_selects_workspace_libraries() {
    let (code, invocations) = run_rung("need", None);
    assert_eq!(code, Some(0), "need rung should succeed, got {code:?}");
    assert_eq!(
        invocations,
        vec![
            "fmt --check".to_string(),
            "test --workspace --lib --quiet".to_string(),
            "test --test hygiene --quiet".to_string(),
        ],
        "need rung must run formatting, workspace library tests, then hygiene, in order"
    );
}

#[test]
fn need_rung_propagates_a_library_failure_without_retrying() {
    // Invocation 2 is the workspace library test; it fails once.
    let (code, invocations) = run_rung("need", Some("2"));
    assert_ne!(code, Some(0), "a library failure must exit nonzero");
    assert_eq!(
        invocations,
        vec![
            "fmt --check".to_string(),
            "test --workspace --lib --quiet".to_string(),
        ],
        "the failed library test must run once and then stop"
    );
}

#[test]
fn main_rung_propagates_a_test_failure_without_retrying() {
    let (code, invocations) = run_rung("main", Some("1"));
    assert_ne!(code, Some(0), "a main test failure must exit nonzero");
    assert_eq!(
        invocations,
        vec!["test --quiet".to_string()],
        "the failed main test must run once and then stop"
    );
}

#[test]
fn main_rung_keeps_its_existing_command() {
    let (code, invocations) = run_rung("main", None);
    assert_eq!(code, Some(0), "main rung should succeed, got {code:?}");
    assert_eq!(
        invocations,
        vec!["test --quiet".to_string()],
        "main rung must keep the unfiltered root cargo test"
    );
}
