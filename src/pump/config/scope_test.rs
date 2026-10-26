//! Tests for [`crate::pump::config::scope`].

use super::{ScopeDecision, ScopePath, ScopePathError, ScopePathKind, ScopePolicy};
use crate::pump::config::PumpTarget;

/// A policy from literal entries, both of which must parse.
fn policy(allow: Option<&[&str]>, deny: &[&str]) -> ScopePolicy {
    ScopePolicy::new(
        allow.map(|entries| entries.iter().map(|entry| String::from(*entry)).collect()),
        deny.iter().map(|entry| String::from(*entry)).collect(),
    )
    .expect("test policy entries should parse")
}

/// A whole request from literal paths.
fn request(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| String::from(*path)).collect()
}

#[test]
fn scope_policy_defaults_to_repository() {
    let wide = ScopePolicy::new(None, Vec::new()).expect("defaults should parse");
    for path in [
        "src/lib.rs",
        "tests/cli.rs",
        "docs/pump.md",
        "skills/vivi/SKILL.md",
        "Cargo.toml",
        "src/",
    ] {
        assert_eq!(wide.decide_path(path), ScopeDecision::Allowed, "{path}");
    }
    assert_eq!(
        wide.decide(&request(&["src/pump/config.rs", "docs/", "tests/x.rs"])),
        ScopeDecision::Allowed
    );
    // Source, tests, docs and skills are mechanical under the default: no
    // operator is ever pulled in, and no directory list is hard-coded.
    assert!(!wide.decide_path("src/lib.rs").requires_operator());
}

#[test]
fn empty_scope_allow_authorizes_nothing() {
    let none = policy(Some(&[]), &[]);
    for path in ["src/lib.rs", "docs/", ".", "README.md"] {
        assert_eq!(
            none.decide_path(path),
            ScopeDecision::OutsideAllow,
            "{path}"
        );
    }
    assert_eq!(
        none.decide(&request(&["src/lib.rs", "docs/"])),
        ScopeDecision::OutsideAllow
    );
    assert!(!none.decide_path("src/lib.rs").is_grant());
}

#[test]
fn scope_deny_overrides_allow() {
    let mixed = policy(Some(&["src/", "docs/"]), &["src/vivi/"]);
    assert_eq!(mixed.decide_path("src/lib.rs"), ScopeDecision::Allowed);
    assert_eq!(mixed.decide_path("docs/notes.md"), ScopeDecision::Allowed);
    assert_eq!(mixed.decide_path("src/vivi/main.rs"), ScopeDecision::Denied);
    assert_eq!(mixed.decide_path("src/vivi/"), ScopeDecision::Denied);
    // Deny also cuts into the repository-wide default.
    let wide = policy(None, &["docs/drafts/"]);
    assert_eq!(wide.decide_path("docs/drafts/x.md"), ScopeDecision::Denied);
    assert_eq!(wide.decide_path("docs/pub.md"), ScopeDecision::Allowed);
}

#[test]
fn scope_policy_uses_component_boundaries() {
    let allow_src = policy(Some(&["src/"]), &[]);
    assert_eq!(allow_src.decide_path("src/a/b.rs"), ScopeDecision::Allowed);
    assert_eq!(
        allow_src.decide_path("srcx/a.rs"),
        ScopeDecision::OutsideAllow
    );
    assert_eq!(
        allow_src.decide_path("src-less/x.rs"),
        ScopeDecision::OutsideAllow
    );
    let deny_src = policy(None, &["src/"]);
    assert_eq!(deny_src.decide_path("srcx/a.rs"), ScopeDecision::Allowed);
    // A file entry matches exactly, never by text prefix.
    let exact = policy(Some(&["src/lib.rs"]), &[]);
    assert_eq!(
        exact.decide_path("src/lib.rs.bak"),
        ScopeDecision::OutsideAllow
    );
    // The parsed-path containment agrees at the same boundaries.
    let src = ScopePath::parse("src/").expect("should parse");
    assert!(src.covers(&ScopePath::parse("./src/a/b.rs").expect("should parse")));
    assert!(!src.covers(&ScopePath::parse("srcology/x").expect("should parse")));
    assert!(src.overlaps(&ScopePath::parse("src/a/").expect("should parse")));
    assert!(!src.overlaps(&ScopePath::parse("srcx/").expect("should parse")));
    assert!(
        ScopePath::parse("a/b.rs")
            .expect("should parse")
            .overlaps(&ScopePath::parse("a/").expect("should parse"))
    );
}

#[test]
fn directory_request_cannot_cover_denied_descendant() {
    let p = policy(Some(&["src/"]), &["src/generated.rs"]);
    assert_eq!(p.decide_path("src/"), ScopeDecision::Denied);
    assert_eq!(
        p.decide(&request(&["src/", "src/lib.rs"])),
        ScopeDecision::Denied
    );
    // A deeper denied subtree is covered too, at any ancestor directory.
    let deep = policy(None, &["src/gen/out/"]);
    assert_eq!(deep.decide_path("src/gen/"), ScopeDecision::Denied);
    assert_eq!(deep.decide_path("src/"), ScopeDecision::Denied);
    // A root request covers every deny entry.
    let root = policy(None, &["anything/"]);
    assert_eq!(root.decide_path("."), ScopeDecision::Denied);
    // A directory entry that covers nothing denied stays allowed.
    let clean = policy(Some(&["src/"]), &["docs/"]);
    assert_eq!(clean.decide_path("src/"), ScopeDecision::Allowed);
}

#[test]
fn mixed_scope_request_is_atomic() {
    let p = policy(Some(&["src/", "docs/"]), &["src/vivi/"]);
    // One allowed path plus one outside: the whole request is refused.
    assert_eq!(
        p.decide(&request(&["src/lib.rs", "tests/x.rs"])),
        ScopeDecision::OutsideAllow
    );
    // One allowed path plus one denied: refused as denied.
    assert_eq!(
        p.decide(&request(&["docs/a.md", "src/vivi/b.rs"])),
        ScopeDecision::Denied
    );
    assert!(!p.decide(&request(&["src/lib.rs", "tests/x.rs"])).is_grant());
    // Requests fully inside the policy still grant.
    assert_eq!(
        p.decide(&request(&["src/lib.rs", "docs/a.md"])),
        ScopeDecision::Allowed
    );
}

#[test]
fn unsafe_scope_paths_require_operator() {
    let wide = ScopePolicy::new(None, Vec::new()).expect("defaults should parse");
    for path in [
        "",
        "   ",
        "/etc/passwd",
        "../out",
        "a/../b",
        "C:/repo",
        "a*b",
        "src/?",
        "x[0]",
    ] {
        assert_eq!(wide.decide_path(path), ScopeDecision::InvalidPath, "{path}");
        assert!(wide.decide_path(path).requires_operator(), "{path}");
    }
    // One unsafe path poisons the whole atomic request.
    assert_eq!(
        wide.decide(&request(&["src/lib.rs", "/abs"])),
        ScopeDecision::InvalidPath
    );
    // A bare path a configured directory could also name waits for the
    // filesystem and needs an operator until then.
    let guarded = policy(Some(&["src/"]), &["src/vivi/"]);
    assert_eq!(guarded.decide_path("src"), ScopeDecision::UnverifiablePath);
    assert!(guarded.decide_path("src").requires_operator());
    assert_eq!(
        guarded.decide(&request(&["src/lib.rs", "src"])),
        ScopeDecision::UnverifiablePath
    );
}

#[test]
fn normalization_keeps_directory_semantics() {
    let parse = |raw| ScopePath::parse(raw).expect("path should parse");
    assert_eq!(parse("./src//lib.rs").as_str(), "src/lib.rs");
    assert_eq!(parse("src/./lib.rs").as_str(), "src/lib.rs");
    assert_eq!(parse("  src/lib.rs  ").as_str(), "src/lib.rs");
    assert_eq!(parse("src/").as_str(), "src/");
    assert_eq!(parse("src/./").as_str(), "src/");
    assert_eq!(parse("./").as_str(), ".");
    assert_eq!(parse(".").kind(), ScopePathKind::Root);
    assert_eq!(parse("src/").kind(), ScopePathKind::Directory);
    assert_eq!(parse("src").kind(), ScopePathKind::File);
    assert_eq!(parse("src/lib.rs").first_segment(), Some("src"));
    assert_eq!(parse("src/").first_segment(), Some("src"));
    assert_eq!(parse(".").first_segment(), None);
}

#[test]
fn malformed_paths_name_their_class() {
    assert_eq!(ScopePath::parse("  "), Err(ScopePathError::Blank));
    assert_eq!(ScopePath::parse(""), Err(ScopePathError::Blank));
    assert_eq!(ScopePath::parse("/abs"), Err(ScopePathError::Absolute));
    assert_eq!(ScopePath::parse("a/../b"), Err(ScopePathError::ParentDir));
    assert_eq!(
        ScopePath::parse("Z:\\repo"),
        Err(ScopePathError::DriveQualified)
    );
    assert_eq!(ScopePath::parse("C:"), Err(ScopePathError::DriveQualified));
    assert_eq!(ScopePath::parse("src/**"), Err(ScopePathError::Wildcard));
}

#[test]
fn policy_construction_rejects_malformed_entries() {
    assert!(
        ScopePolicy::new(
            Some(vec![String::from("ok/"), String::from("/abs")]),
            Vec::new()
        )
        .is_err()
    );
    assert!(ScopePolicy::new(None, vec![String::from("a*b")]).is_err());
}

#[test]
fn resolve_takes_a_targets_own_policy() {
    let target = PumpTarget {
        scope_allow: Some(vec![String::from("src/")]),
        scope_deny: vec![String::from("src/secret.rs")],
        ..PumpTarget::default()
    };
    let resolved = ScopePolicy::resolve(&target).expect("entries should parse");
    assert_eq!(resolved.decide_path("src/lib.rs"), ScopeDecision::Allowed);
    assert_eq!(resolved.decide_path("src/secret.rs"), ScopeDecision::Denied);
    // A target that declares nothing resolves to the repository-wide default.
    let plain = ScopePolicy::resolve(&PumpTarget::default()).expect("defaults should parse");
    assert_eq!(plain.allow(), None);
    assert!(plain.deny().is_empty());
    assert_eq!(plain.decide_path("anything.rs"), ScopeDecision::Allowed);
}
