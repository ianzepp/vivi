//! Tests for [`super::transient`].

use super::transient::classify;

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
fn ordinary_failures_are_not_transient() {
    assert_eq!(classify("proof-failed", &proof(1)), None);
    assert_eq!(classify("finished/agent exit 1", ""), None);
    assert_eq!(classify("branch-missing", "no ref"), None);
}
