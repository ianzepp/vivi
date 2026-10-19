//! Tests for the proof-command validator, driven by the shapes the
//! planner actually emits and the cargo text the pump observed on the
//! burned attempts (need 6d9d254e, plan 38b4d91d).

use super::proof::{check, usage_failure};

/// An error result whose message contains `needle`.
fn err_containing(cmd: &str, needle: &str) {
    let err = check(cmd).expect_err(cmd);
    assert!(
        err.contains(needle),
        "check({cmd:?}) = {err:?}, want it to mention {needle:?}"
    );
}

#[test]
fn two_testname_filters_are_rejected() {
    err_containing(
        "cargo test --lib sync::watch an_event_applied_by_pull",
        "one TESTNAME filter",
    );
    err_containing("cargo test a b", "one TESTNAME filter");
    err_containing("cargo +nightly test a b", "one TESTNAME filter");
    err_containing(
        "cargo test --package vivi --lib pump::plan extra",
        "one TESTNAME filter",
    );
}

#[test]
fn single_filter_and_flags_are_accepted() {
    for cmd in [
        "cargo test --test cli proof_rule",
        "cargo test -p vivi --lib pump::plan",
        "cargo test -- a --nocapture",
        "cargo test",
        "cargo +nightly test --profile dev one_filter",
        "cargo build",
    ] {
        assert_eq!(check(cmd), Ok(()), "want {cmd:?} accepted");
    }
}

#[test]
fn unknown_binaries_are_rejected() {
    err_containing("pytest -x", "unknown binary");
    err_containing("/usr/bin/pytest -x", "unknown binary");
    assert_eq!(check("true"), Ok(()));
    assert_eq!(check("rg 'def parse' src/"), Ok(()));
}

#[test]
fn wrappers_are_validated_too() {
    err_containing("bash -c 'cargo test a b'", "one TESTNAME filter");
    err_containing("sh -c 'cargo test a b'", "one TESTNAME filter");
    err_containing("env cargo test a b", "one TESTNAME filter");
    err_containing("env RUST_BACKTRACE=1 cargo test a b", "one TESTNAME filter");
    err_containing("zsh -c 'pytest -x'", "unknown binary");
    assert_eq!(check("env cargo test one_filter"), Ok(()));
    assert_eq!(check("bash -c 'cargo test --lib sync::watch'"), Ok(()));
}

#[test]
fn empty_proof_is_rejected() {
    err_containing("   ", "empty proof command");
    err_containing("", "empty proof command");
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
