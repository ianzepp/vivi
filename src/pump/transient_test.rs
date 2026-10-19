//! Tests for [`super::transient`].

use super::transient::{build_only_check, classify, jitter, provider_rate_limited};

/// A `merge-conflict` detail shaped exactly like `merge` writes it.
fn conflict(files: &[&str]) -> String {
    format!(
        "The branch no longer merges into the integration branch. Conflicting files: {}",
        files.join("\n")
    )
}

/// A `proof-failed` detail shaped exactly like `merge` writes it.
fn proof(exit: u32) -> String {
    format!(
        "After merging into the integration branch, this command failed (exit {exit}):\n\
         cargo test --lib\n\nLast output:\n…"
    )
}

#[test]
fn host_classes_from_collect() {
    assert_eq!(classify("error/host-down", ""), Some("host"));
    assert_eq!(classify("lost/the pool forgot the task", ""), Some("host"));
}

#[test]
fn fetch_failure_is_host() {
    assert_eq!(
        classify("fetch-failed", "fatal: unable to access remote"),
        Some("host")
    );
}

#[test]
fn proof_timeouts() {
    assert_eq!(classify("proof-failed", &proof(124)), Some("timeout"));
    assert_eq!(
        classify("proof-failed", "partial output\n[timeout after 1s]"),
        Some("timeout")
    );
    assert_eq!(classify("proof-failed", &proof(137)), Some("timeout"));
}

#[test]
fn lockfile_only_conflict_is_generated() {
    let detail = conflict(&["Cargo.lock", "crates/app/Cargo.lock"]);
    assert_eq!(
        classify("merge-conflict", &detail),
        Some("generated-conflict")
    );
}

#[test]
fn conflict_with_source_file_is_real() {
    let detail = conflict(&["Cargo.lock", "src/lib.rs"]);
    assert_eq!(classify("merge-conflict", &detail), None);
}

#[test]
fn empty_conflict_list_is_real() {
    assert_eq!(classify("merge-conflict", &conflict(&[])), None);
}

#[test]
fn provider_rate_limit_markers_are_transient() {
    assert!(provider_rate_limited(
        "429: {\"code\":\"1302\",\"message\":\"Rate limit reached for requests\"}"
    ));
    assert!(provider_rate_limited("Connection error."));
    assert!(provider_rate_limited("overloaded_error"));
    assert!(provider_rate_limited("Rate limit reached for requests"));
    assert!(!provider_rate_limited("planner exited 1: no plan written"));
    assert!(!provider_rate_limited("cargo test finished ok"));
    assert!(!provider_rate_limited(""));
}

#[test]
fn build_only_checks() {
    // The lane-timeout shape `merge` writes: command embedded after the exit.
    let killed = |cmd: &str| {
        format!(
            "After merging into the integration branch, this command failed (exit 124):\n\
             {cmd}\n\nWhat failed:\n[timeout after 480s]"
        )
    };
    assert!(build_only_check(&killed("cargo test --workspace --no-run --quiet")));
    assert!(build_only_check(&killed("cargo build --release")));
    assert!(build_only_check(&killed("cargo check --workspace")));
    assert!(build_only_check(&killed("cargo clippy --all-targets")));
    assert!(!build_only_check(&killed("cargo test --workspace --quiet")));
    // No embedded command at all: empty detail, and the bare timeout marker.
    assert!(!build_only_check(""));
    assert!(!build_only_check("partial output\n[timeout after 1s]"));
}

#[test]
fn jitter_is_deterministic_and_bounded() {
    for seed in ["", "a", "unit-55cd9711", "planner-429", "xxxxxxxxxxxxxxxx"] {
        assert_eq!(jitter(seed), jitter(seed), "same seed must give same value");
        assert!(jitter(seed) < 15, "jitter must stay in 0-14");
    }
}

#[test]
fn ordinary_failures_are_not_transient() {
    assert_eq!(classify("proof-failed", &proof(1)), None);
    assert_eq!(classify("finished/agent exit 1", ""), None);
    assert_eq!(classify("branch-missing", "no ref"), None);
}
