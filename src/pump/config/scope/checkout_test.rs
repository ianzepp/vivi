//! Tests for [`super::classify`] and the checked [`authorize`] entry point:
//! symlink traversal stays an operator decision, while new leaves beneath a
//! verifiable checkout and ambiguity the filesystem settles decide
//! mechanically.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;

use tempfile::tempdir;

use super::super::authorize;
use crate::pump::config::{PumpTarget, ScopeDecision};

/// One configured target named `web` with the given literal scope entries.
fn configs(allow: Option<&[&str]>, deny: &[&str]) -> BTreeMap<String, PumpTarget> {
    let target = PumpTarget {
        scope_allow: allow
            .map(|entries| entries.iter().map(|entry| String::from(*entry)).collect()),
        scope_deny: deny.iter().map(|entry| String::from(*entry)).collect(),
        ..PumpTarget::default()
    };
    BTreeMap::from([(String::from("web"), target)])
}

/// A whole request from literal paths.
fn request(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| String::from(*path)).collect()
}

/// The checked decision of `paths` for the `web` target against `checkout`.
fn decide(
    configs: &BTreeMap<String, PumpTarget>,
    checkout: &std::path::Path,
    paths: &[&str],
) -> ScopeDecision {
    authorize(configs, "web", Some(checkout), &request(paths))
}

/// A verifiable checkout with a real `src/` tree, a denied `src/secret/`
/// subtree, and three symlinks: an internal alias into the denied subtree,
/// an external alias out of the checkout, and a dangling alias.
fn populated_checkout() -> tempfile::TempDir {
    let dir = tempdir().expect("temp checkout should create");
    fs::create_dir_all(dir.path().join("src/secret")).expect("tree should create");
    fs::write(dir.path().join("src/lib.rs"), "fn main() {}").expect("file should write");
    fs::write(dir.path().join("src/secret/key.txt"), "hush").expect("file should write");
    symlink(dir.path().join("src/secret"), dir.path().join("src/door"))
        .expect("internal alias should create");
    symlink("/etc", dir.path().join("outside")).expect("external alias should create");
    symlink(
        dir.path().join("src/absent-target"),
        dir.path().join("dangling"),
    )
    .expect("dangling alias should create");
    dir
}

#[test]
fn symlink_traversal_requires_operator() {
    let checkout = populated_checkout();
    let policy = configs(Some(&["src/"]), &["src/secret/"]);

    // Plain decisions inside the real tree keep their typed verdicts.
    assert_eq!(
        decide(&policy, checkout.path(), &["src/lib.rs"]),
        ScopeDecision::Allowed
    );
    assert_eq!(
        decide(&policy, checkout.path(), &["src/secret/key.txt"]),
        ScopeDecision::Denied
    );
    assert_eq!(
        decide(&policy, checkout.path(), &["docs/notes.md"]),
        ScopeDecision::OutsideAllow
    );

    // An existing symlink is never silently authorized, final or not: the
    // internal alias reaches into the denied subtree...
    assert_eq!(
        decide(&policy, checkout.path(), &["src/door"]),
        ScopeDecision::UnverifiablePath
    );
    assert_eq!(
        decide(&policy, checkout.path(), &["src/door/key.txt"]),
        ScopeDecision::UnverifiablePath
    );
    // ...the external alias leaves the checkout entirely...
    assert_eq!(
        decide(&policy, checkout.path(), &["outside"]),
        ScopeDecision::UnverifiablePath
    );
    assert_eq!(
        decide(&policy, checkout.path(), &["outside/passwd"]),
        ScopeDecision::UnverifiablePath
    );
    // ...and a dangling alias cannot be verified either.
    assert_eq!(
        decide(&policy, checkout.path(), &["dangling"]),
        ScopeDecision::UnverifiablePath
    );

    // One unverifiable path refuses the whole atomic request.
    assert_eq!(
        decide(&policy, checkout.path(), &["src/lib.rs", "outside/passwd"]),
        ScopeDecision::UnverifiablePath
    );
}

#[test]
fn missing_and_unverifiable_checkout_roots_fail_safe() {
    let checkout = populated_checkout();
    let configs = configs(Some(&["src/"]), &[]);

    // A missing checkout root verifies nothing.
    assert_eq!(
        decide(&configs, &checkout.path().join("absent"), &["src/lib.rs"]),
        ScopeDecision::UnverifiablePath
    );

    // A checkout root that is itself a symlink is unverifiable.
    let alias = tempdir().expect("temp alias home should create");
    symlink(checkout.path(), alias.path().join("linked")).expect("alias should create");
    assert_eq!(
        decide(&configs, &alias.path().join("linked"), &["src/lib.rs"]),
        ScopeDecision::UnverifiablePath
    );

    // A checkout root that is a regular file is not a checkout.
    fs::write(checkout.path().join("plain-file"), "x").expect("file should write");
    assert_eq!(
        decide(
            &configs,
            &checkout.path().join("plain-file"),
            &["src/lib.rs"]
        ),
        ScopeDecision::UnverifiablePath
    );
}

#[test]
fn new_leaves_under_a_verifiable_checkout_are_valid() {
    let checkout = populated_checkout();
    let policy = configs(Some(&["src/"]), &["src/secret/"]);

    // New files and directories beneath verified directories decide
    // mechanically...
    assert_eq!(
        decide(&policy, checkout.path(), &["src/new.rs"]),
        ScopeDecision::Allowed
    );
    assert_eq!(
        decide(&policy, checkout.path(), &["src/newdir/nested.rs"]),
        ScopeDecision::Allowed
    );
    // ...still inside the denied subtree they are denied...
    assert_eq!(
        decide(&policy, checkout.path(), &["src/secret/new.rs"]),
        ScopeDecision::Denied
    );
    // ...and a deep new leaf under a missing middle is still one new leaf.
    assert_eq!(
        decide(&policy, checkout.path(), &["src/a/b/c.rs"]),
        ScopeDecision::Allowed
    );

    // A repository-wide default keeps the root request mechanical.
    let wide = configs(None, &[]);
    assert_eq!(
        decide(&wide, checkout.path(), &["."]),
        ScopeDecision::Allowed
    );

    // An unresolved checkout verifies nothing.
    assert_eq!(
        authorize(&policy, "web", None, &request(&["src/new.rs"])),
        ScopeDecision::UnverifiablePath
    );
    // So does a target the map does not name: the structural unknown-target
    // rejection belongs to the caller, and this fails safe behind it.
    assert_eq!(
        authorize(
            &policy,
            "other",
            Some(checkout.path()),
            &request(&["src/new.rs"])
        ),
        ScopeDecision::UnverifiablePath
    );
}

#[test]
fn the_filesystem_resolves_the_bare_file_ambiguity() {
    let checkout = populated_checkout();

    // The pure layer fails safe on bare `src` under an `src/` allow entry;
    // the checkout says `src` is a directory, so the entry applies.
    let allowed = configs(Some(&["src/"]), &[]);
    assert_eq!(
        decide(&allowed, checkout.path(), &["src"]),
        ScopeDecision::Allowed
    );
    // The same resolution means a bare request covers the denied descendant
    // beneath it, exactly as a trailing-slash directory request would.
    let denied = configs(None, &["src/secret/"]);
    assert_eq!(
        decide(&denied, checkout.path(), &["src"]),
        ScopeDecision::Denied
    );

    // A bare path that does not exist names a new file: no directory entry
    // applies, and the allow list decides literally.
    let guarded = configs(Some(&["src/"]), &[]);
    assert_eq!(
        decide(&guarded, checkout.path(), &["absent"]),
        ScopeDecision::OutsideAllow
    );
}
