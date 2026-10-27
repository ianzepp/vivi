//! Tests for [`crate::pump::config::scope_policy`]: normalization, exact
//! files, the root entry, directory boundaries, symmetric overlap and the
//! serialized decision codes.

use super::{ScopeDecision, ScopePath};

/// A normalized path, panicking on a rejected entry.
fn p(raw: &str) -> ScopePath {
    ScopePath::normalize(raw).unwrap()
}

/// The rejection message for a refused entry.
fn refused(raw: &str) -> String {
    ScopePath::normalize(raw).unwrap_err().to_string()
}

#[test]
fn normalization_flattens_harmless_segments() {
    assert_eq!(p("src/a.rs").as_str(), "src/a.rs");
    assert_eq!(p("./src/a.rs").as_str(), "src/a.rs");
    assert_eq!(p("src//a.rs").as_str(), "src/a.rs");
    assert_eq!(p("src/./a.rs").as_str(), "src/a.rs");
    assert_eq!(p("  src/a.rs  ").as_str(), "src/a.rs");
    assert_eq!(p("src/./").as_str(), "src/");
    assert_eq!(p("./src/./").as_str(), "src/");
}

#[test]
fn normalization_keeps_exact_and_directory_forms_distinct() {
    assert!(!p("src/a.rs").is_directory());
    assert!(p("src/").is_directory());
    assert_eq!(p("src/a.rs").as_str(), "src/a.rs");
    assert_eq!(p("src/a.rs/").as_str(), "src/a.rs/");
}

#[test]
fn normalization_rejects_blank() {
    for raw in ["", "   ", "\t"] {
        assert!(refused(raw).contains("blank"), "`{raw}` not refused");
    }
}

#[test]
fn normalization_rejects_absolute() {
    for raw in ["/", "/etc", "/src/a.rs"] {
        assert!(refused(raw).contains("absolute"), "`{raw}` not refused");
    }
}

#[test]
fn normalization_rejects_parent_traversal() {
    for raw in ["..", "../x", "src/..", "src/../x", "./.."] {
        assert!(refused(raw).contains("traverses"), "`{raw}` not refused");
    }
}

#[test]
fn root_entry_grants_everything() {
    let root = p("./");
    assert!(root.is_root());
    assert!(root.is_directory());
    assert_eq!(root.as_str(), "./");
    assert_eq!(p(".").as_str(), "./");
    assert!(root.contains(&p("src/a.rs")));
    assert!(root.contains(&p("src/")));
    assert!(root.contains(&p("src-old/a.rs")));
    assert!(root.contains(&root));
    assert!(root.overlaps(&p("src/")));
}

#[test]
fn exact_file_grants_only_itself() {
    let file = p("src/a.rs");
    assert!(file.contains(&p("src/a.rs")));
    assert!(!file.contains(&p("src/b.rs")));
    assert!(!file.contains(&p("src/")));
    assert!(!file.contains(&p("src")));
    assert!(!file.contains(&p("src/a.rs.bak")));
}

#[test]
fn exact_file_does_not_grant_children() {
    let file = p("src/a.rs");
    assert!(!file.contains(&p("src/a.rs/child")));
    assert!(!file.contains(&p("src/a.rs/deep/kid")));
    assert!(!file.overlaps(&p("src/a.rs/child")));
    assert!(!p("src/a.rs/child").contains(&file));
}

#[test]
fn directory_grants_its_subtree() {
    let dir = p("src/");
    assert!(dir.contains(&p("src/")));
    assert!(dir.contains(&p("src/a.rs")));
    assert!(dir.contains(&p("src/a/")));
    assert!(dir.contains(&p("src/a.rs/child")));
    assert!(dir.overlaps(&p("src/a.rs")));
}

#[test]
fn directory_boundary_is_component_aware() {
    let dir = p("src/");
    assert!(!dir.contains(&p("src-old/a.rs")));
    assert!(!dir.contains(&p("srx/a.rs")));
    assert!(!dir.contains(&p("src-old/")));
    assert!(!dir.overlaps(&p("src-old/a.rs")));
    assert!(!p("src-old/a.rs").overlaps(&dir));
}

#[test]
fn overlap_is_symmetric() {
    let cases: [(&str, &str, bool); 6] = [
        ("src/", "src/a.rs", true),
        ("src/a.rs", "src/", true),
        ("src/a.rs", "src/b.rs", false),
        ("src/a.rs", "src-old/a.rs", false),
        ("src/", "src/", true),
        ("src/a.rs", "src/a.rs/child", false),
    ];
    for (left, right, expected) in cases {
        assert_eq!(p(left).overlaps(&p(right)), expected, "{left} vs {right}");
    }
}

#[test]
fn decision_unit_codes_serialize_as_strings() {
    assert_eq!(
        serde_json::to_string(&ScopeDecision::Granted).unwrap(),
        "\"granted\""
    );
    let round: ScopeDecision = serde_json::from_str("\"scope_denied\"").unwrap();
    assert_eq!(round, ScopeDecision::ScopeDenied);
    assert_eq!(ScopeDecision::Granted.code(), "granted");
    assert_eq!(ScopeDecision::OutsideAllow.code(), "outside_allow");
    assert_eq!(ScopeDecision::ScopeDenied.code(), "scope_denied");
    assert_eq!(ScopeDecision::UnsafePath.code(), "unsafe_path");
}

#[test]
fn unknown_target_is_typed_not_prose() {
    let decision = ScopeDecision::UnknownTarget {
        target: "web".to_owned(),
    };
    assert_eq!(decision.code(), "unknown_target");
    assert_eq!(decision.unknown_target(), Some("web"));
    assert_eq!(ScopeDecision::Granted.unknown_target(), None);
    let json = serde_json::to_string(&decision).unwrap();
    assert_eq!(json, "{\"unknown_target\":{\"target\":\"web\"}}");
    let round: ScopeDecision = serde_json::from_str(&json).unwrap();
    assert_eq!(round, decision);
}
