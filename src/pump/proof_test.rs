//! Tests for the proof-command validator, driven by the shapes the
//! planner actually emits and the cargo text the pump observed on the
//! burned attempts (need 6d9d254e, plan 38b4d91d).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::proof::{check, usage_failure};

/// Set the file mode of `path` to `mode`.
fn set_mode(path: &Path, mode: u32) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(mode);
    fs::set_permissions(path, perms).unwrap();
}

/// A fixture target repository with executable proof scripts at the paths
/// faberlang plans normally use, plus a non-executable file to reject.
fn fixture_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    for rel in [
        "scripta/check-corpus-suite",
        "scripta/check-grammar-tags",
        "target/debug/faber",
    ] {
        let path = repo.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "#!/bin/sh\n").unwrap();
        set_mode(&path, 0o755);
    }
    let notes = repo.path().join("scripta/notes.txt");
    fs::write(&notes, "notes\n").unwrap();
    set_mode(&notes, 0o644);
    repo
}

/// An error result whose message contains `needle`.
fn err_containing(cmd: &str, repo: &Path, needle: &str) {
    let err = check(cmd, repo).expect_err(cmd);
    assert!(
        err.contains(needle),
        "check({cmd:?}) = {err:?}, want it to mention {needle:?}"
    );
}

#[test]
fn two_testname_filters_are_rejected() {
    let repo = fixture_repo();
    err_containing(
        "cargo test --lib sync::watch an_event_applied_by_pull",
        repo.path(),
        "one TESTNAME filter",
    );
    err_containing("cargo test a b", repo.path(), "one TESTNAME filter");
    err_containing(
        "cargo +nightly test a b",
        repo.path(),
        "one TESTNAME filter",
    );
    err_containing(
        "cargo test --package vivi --lib pump::plan extra",
        repo.path(),
        "one TESTNAME filter",
    );
}

#[test]
fn single_filter_and_flags_are_accepted() {
    let repo = fixture_repo();
    for cmd in [
        "cargo test --test cli proof_rule",
        "cargo test -p vivi --lib pump::plan",
        "cargo test -- a --nocapture",
        "cargo test",
        "cargo +nightly test --profile dev one_filter",
        "cargo build",
    ] {
        assert_eq!(check(cmd, repo.path()), Ok(()), "want {cmd:?} accepted");
    }
}

#[test]
fn unknown_binaries_are_rejected() {
    let repo = fixture_repo();
    err_containing("pytest -x", repo.path(), "unknown binary");
    err_containing("/usr/bin/pytest -x", repo.path(), "unknown binary");
    assert_eq!(check("true", repo.path()), Ok(()));
    assert_eq!(check("rg 'def parse' src/", repo.path()), Ok(()));
}

#[test]
fn wrappers_are_validated_too() {
    let repo = fixture_repo();
    err_containing(
        "bash -c 'cargo test a b'",
        repo.path(),
        "one TESTNAME filter",
    );
    err_containing("sh -c 'cargo test a b'", repo.path(), "one TESTNAME filter");
    err_containing("env cargo test a b", repo.path(), "one TESTNAME filter");
    err_containing(
        "env RUST_BACKTRACE=1 cargo test a b",
        repo.path(),
        "one TESTNAME filter",
    );
    err_containing("zsh -c 'pytest -x'", repo.path(), "unknown binary");
    assert_eq!(check("env cargo test one_filter", repo.path()), Ok(()));
    assert_eq!(
        check("bash -c 'cargo test --lib sync::watch'", repo.path()),
        Ok(())
    );
}

#[test]
fn env_assignment_prefixes_are_skipped_like_env() {
    let repo = fixture_repo();
    for cmd in [
        "PATH=/opt/homebrew/bin:$PATH cargo test -p x --lib y",
        "A=1 B=2 cargo test one",
    ] {
        assert_eq!(check(cmd, repo.path()), Ok(()), "want {cmd:?} accepted");
    }
    err_containing("FOO=1 cargo test a b", repo.path(), "one TESTNAME filter");
    err_containing("FOO=1 pytest -x", repo.path(), "unknown binary");
    err_containing("FOO=1 BAR=2 pytest -x", repo.path(), "unknown binary");
}

#[test]
fn repo_relative_scripts_are_accepted_when_executable() {
    let repo = fixture_repo();
    for cmd in [
        "./scripta/check-corpus-suite",
        "scripta/check-grammar-tags",
        "target/debug/faber --help",
    ] {
        assert_eq!(check(cmd, repo.path()), Ok(()), "want {cmd:?} accepted");
    }
}

#[test]
fn repo_relative_paths_missing_or_not_executable_are_rejected() {
    let repo = fixture_repo();
    err_containing("./nope", repo.path(), "missing or not executable");
    err_containing(
        "scripta/notes.txt",
        repo.path(),
        "missing or not executable",
    );
}

#[test]
fn empty_proof_is_rejected() {
    let repo = fixture_repo();
    err_containing("   ", repo.path(), "empty proof command");
    err_containing("", repo.path(), "empty proof command");
}

#[test]
fn usage_failure_matches_the_observed_cargo_text() {
    let text = "error: unexpected argument 'an_event_applied_by_pull' found\n\n\
                Usage: cargo test [OPTIONS] [TESTNAME] [-- [ARGS]...]\n\n\
                For more information, try '--help'.\n";
    assert!(usage_failure(text));
    let argument_form = "error: the argument '--lib' cannot be used multiple times\n\
                         Usage: cargo test [OPTIONS] [TESTNAME] [-- [ARGS]...]\n";
    assert!(usage_failure(argument_form));
}

#[test]
fn usage_failure_ignores_compile_errors_and_test_failures() {
    let compile = "error[E0308]: mismatched types\n --> src/lib.rs:2:9\n  |\n2 |     x = 1;\n";
    assert!(!usage_failure(compile));
    let failed = "test pump::plan::tests::it_works ... FAILED\n\n\
                  failures:\n    pump::plan::tests::it_works\n\n\
                  test result: FAILED. 0 passed; 1 failed; 0 ignored\n";
    assert!(!usage_failure(failed));
    assert!(!usage_failure(
        "error: test failed, to rerun pass `--lib`\n"
    ));
    assert!(!usage_failure(""));
}
