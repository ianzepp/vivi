//! Tests for [`super::scope`].

use super::{
    ScopeDecision, ScopePath, ScopePathError, ScopePolicy, ScopeRejection, WHOLE_REPOSITORY,
    first_overlap, set_covers,
};

/// Every entry parsed, panicking on a bad literal so tests stay terse.
fn paths(values: &[&str]) -> Vec<ScopePath> {
    values
        .iter()
        .map(|value| ScopePath::parse(value).expect("test declaration should parse"))
        .collect()
}

#[test]
fn normalization_drops_current_directory_components_and_keeps_directory_marks() {
    let file = ScopePath::parse("./src/./lib.rs").expect("should parse");
    assert_eq!(file.canonical(), "src/lib.rs");
    assert!(!file.is_directory());

    let dir = ScopePath::parse("docs/guides/").expect("should parse");
    assert_eq!(dir.canonical(), "docs/guides/");
    assert!(dir.is_directory());

    let squeezed = ScopePath::parse("src//lib.rs").expect("should parse");
    assert_eq!(squeezed.canonical(), "src/lib.rs");

    let again = ScopePath::parse(&dir.canonical()).expect("canonical form should re-parse");
    assert_eq!(again, dir);
}

#[test]
fn dot_and_dot_slash_are_the_repository_root() {
    for raw in [".", "./", "./."] {
        let root = ScopePath::parse(raw).expect("root should parse");
        assert!(root.is_root());
        assert!(root.is_directory());
        assert_eq!(root.canonical(), WHOLE_REPOSITORY);
        assert_eq!(root, ScopePath::root());
    }
}

#[test]
fn blank_absolute_and_traversal_declarations_are_rejected_without_the_filesystem() {
    assert_eq!(ScopePath::parse(""), Err(ScopePathError::Blank));
    assert_eq!(ScopePath::parse("   "), Err(ScopePathError::Blank));
    assert_eq!(
        ScopePath::parse("/etc/passwd"),
        Err(ScopePathError::Absolute)
    );
    assert_eq!(ScopePath::parse(".."), Err(ScopePathError::ParentTraversal));
    assert_eq!(
        ScopePath::parse("src/../.."),
        Err(ScopePathError::ParentTraversal)
    );
    assert_eq!(
        ScopePath::parse("a/b/../../c"),
        Err(ScopePathError::ParentTraversal)
    );
}

#[test]
fn files_are_exact_and_directories_cover_descendants_by_component_boundary() {
    let file = ScopePath::parse("src/lib.rs").expect("should parse");
    let dir = ScopePath::parse("src/").expect("should parse");

    assert!(file.covers(&ScopePath::parse("src/lib.rs").expect("should parse")));
    assert!(!file.covers(&ScopePath::parse("src/lib.rs.bak").expect("should parse")));
    assert!(!file.covers(&ScopePath::parse("src/lib.rs/inner").expect("should parse")));

    assert!(dir.covers(&file));
    assert!(dir.covers(&ScopePath::parse("src").expect("should parse")));
    assert!(dir.covers(&ScopePath::parse("src/letter/envelope.rs").expect("should parse")));
    assert!(!dir.covers(&ScopePath::parse("srcx/letter.rs").expect("should parse")));

    assert!(ScopePath::root().covers(&dir));
    assert!(!dir.covers(&ScopePath::root()));
    assert!(ScopePath::root().covers(&ScopePath::root()));
}

#[test]
fn intersections_find_shared_paths_between_declarations() {
    let src = ScopePath::parse("src/").expect("should parse");
    let lib = ScopePath::parse("src/lib.rs").expect("should parse");
    let docs = ScopePath::parse("docs/").expect("should parse");

    assert!(src.intersects(&lib));
    assert!(lib.intersects(&src));
    assert!(!src.intersects(&docs));
    assert!(ScopePath::root().intersects(&docs));
}

#[test]
fn set_coverage_and_first_overlap_work_over_declaration_lists() {
    let allow = paths(&["crates/vivi/", "docs"]);
    assert!(set_covers(
        &allow,
        &ScopePath::parse("crates/vivi/src/main.rs").expect("should parse")
    ));
    assert!(set_covers(
        &allow,
        &ScopePath::parse("docs").expect("should parse")
    ));
    assert!(!set_covers(
        &allow,
        &ScopePath::parse("crates/vivarium/src/lib.rs").expect("should parse")
    ));
    assert!(!set_covers(
        &allow,
        &ScopePath::parse("docsx").expect("should parse")
    ));

    let left = paths(&["src/lib.rs", "README.md"]);
    let right = paths(&["assets/", "src/lib.rs"]);
    let (a, b) = first_overlap(&left, &right).expect("sets should overlap");
    assert_eq!(a, &ScopePath::parse("src/lib.rs").expect("should parse"));
    assert_eq!(b, &ScopePath::parse("src/lib.rs").expect("should parse"));
    assert!(first_overlap(&paths(&["x"]), &paths(&["y"])).is_none());
}

#[test]
fn a_policy_snapshot_normalizes_entries_and_round_trips_through_json() {
    let policy = ScopePolicy::snapshot(
        "app",
        &[String::from("./src/"), String::from("docs")],
        &[String::from("src/generated/")],
    )
    .expect("declarations should parse");
    assert_eq!(policy.target, "app");
    assert_eq!(
        policy.allow,
        vec![String::from("src/"), String::from("docs")]
    );
    assert_eq!(policy.deny, vec![String::from("src/generated/")]);

    let text = serde_json::to_string(&policy).expect("snapshot should serialize");
    let again: ScopePolicy = serde_json::from_str(&text).expect("snapshot should deserialize");
    assert_eq!(again, policy);
}

#[test]
fn an_unusable_snapshot_entry_names_the_declaration() {
    let rejection = ScopePolicy::snapshot("app", &[String::from("/abs")], &[])
        .expect_err("absolute paths are rejected");
    assert_eq!(rejection.reason(), "invalid_path");
    assert!(
        matches!(rejection, ScopeRejection::InvalidPath { ref path, .. } if path == "/abs"),
        "{rejection}"
    );
    assert!(rejection.to_string().contains("/abs"));
}

#[test]
fn check_applies_deny_over_allow_and_empty_allow_allows_nothing() {
    let policy = ScopePolicy::snapshot(
        "app",
        &[String::from("src/")],
        &[String::from("src/generated/")],
    )
    .expect("declarations should parse");

    assert_eq!(policy.check("src/lib.rs"), ScopeDecision::Allow);
    assert_eq!(
        policy.check("src/generated/out.rs"),
        ScopeDecision::Reject(ScopeRejection::DeniedPath {
            path: String::from("src/generated/out.rs"),
            deny: String::from("src/generated/"),
        })
    );
    assert!(matches!(
        policy.check("docs/plan.md"),
        ScopeDecision::Reject(ScopeRejection::OutsideAllow { .. })
    ));
    assert!(matches!(
        policy.check("../escape"),
        ScopeDecision::Reject(ScopeRejection::InvalidPath { .. })
    ));

    let nothing = ScopePolicy::snapshot("app", &[], &[]).expect("empty declarations parse");
    assert!(matches!(
        nothing.check("./"),
        ScopeDecision::Reject(ScopeRejection::OutsideAllow { .. })
    ));
}

#[test]
fn the_ruling_default_is_the_whole_repository_except_deny() {
    let policy = ScopePolicy::snapshot(
        "app",
        &[String::from(WHOLE_REPOSITORY)],
        &[String::from("vendor/")],
    )
    .expect("declarations should parse");
    assert_eq!(policy.check("src/lib.rs"), ScopeDecision::Allow);
    assert_eq!(
        policy.check("vendor/bundle/x.rs"),
        ScopeDecision::Reject(ScopeRejection::DeniedPath {
            path: String::from("vendor/bundle/x.rs"),
            deny: String::from("vendor/"),
        })
    );
}

#[test]
fn rejections_serialize_with_reason_tags_and_display_sentences() {
    let rejections = [
        ScopeRejection::OutsideAllow {
            path: String::from("x"),
            allow: vec![String::from("./")],
        },
        ScopeRejection::DeniedPath {
            path: String::from("x"),
            deny: String::from("d/"),
        },
        ScopeRejection::InvalidPath {
            path: String::from("/x"),
            problem: String::from("absolute"),
        },
        ScopeRejection::UnknownTarget {
            target: String::from("nope"),
            known: vec![String::from("app")],
        },
        ScopeRejection::NeedTargetMismatch {
            unit_target: String::from("app"),
            need_target: String::from("docs"),
        },
    ];
    let tags = [
        "outside_allow",
        "denied_path",
        "invalid_path",
        "unknown_target",
        "need_target_mismatch",
    ];
    for (rejection, tag) in rejections.iter().zip(tags) {
        assert_eq!(rejection.reason(), tag);
        let text = serde_json::to_string(rejection).expect("rejection should serialize");
        assert!(text.contains(&format!("\"reason\":\"{tag}\"")), "{text}");
        assert!(!rejection.to_string().is_empty());
    }
}
