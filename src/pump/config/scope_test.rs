//! Tests for [`super::scope`].

use std::collections::BTreeMap;
use std::path::Path;

use super::{
    ScopeAuthorization, ScopeDecision, ScopePath, ScopePathError, ScopePolicy, ScopeRejection,
    WHOLE_REPOSITORY, authorize, first_overlap, set_covers,
};
use crate::pump::config::{PumpConfig, PumpTarget};

/// Every entry parsed, panicking on a bad literal so tests stay terse.
fn paths(values: &[&str]) -> Vec<ScopePath> {
    values
        .iter()
        .map(|value| ScopePath::parse(value).expect("test declaration should parse"))
        .collect()
}

/// A pump config parsed from mailspace TOML, for configuration findings.
fn pump_config(text: &str) -> PumpConfig {
    let mail: crate::mailspace::MailspaceConfig =
        toml::from_str(text).expect("mailspace text should parse");
    mail.pump.expect("pump table should be present")
}

/// Named targets for authorization tests: `app` allows `src/` and denies
/// `src/generated/`, `web` allows only `web/`.
fn target_configs() -> BTreeMap<String, PumpTarget> {
    let mut configs = BTreeMap::new();
    configs.insert(
        String::from("app"),
        PumpTarget {
            scope_allow: Some(vec![String::from("src/")]),
            scope_deny: vec![String::from("src/generated/")],
            ..PumpTarget::default()
        },
    );
    configs.insert(
        String::from("web"),
        PumpTarget {
            scope_allow: Some(vec![String::from("web/")]),
            ..PumpTarget::default()
        },
    );
    configs
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

#[test]
fn scope_policy_uses_component_boundaries() {
    let policy = ScopePolicy::snapshot("app", &[String::from("src/"), String::from("tests")], &[])
        .expect("declarations should parse");

    // A file under an allowed directory is eligible, leading `./` drops, and
    // the grant carries the normalized path. The file need not exist: paths
    // are declarations, never filesystem lookups.
    assert_eq!(
        policy.authorize(&[String::from("./src/helper.rs")]),
        ScopeAuthorization::Grant(vec![String::from("src/helper.rs")])
    );
    assert_eq!(
        policy.authorize(&[String::from("src/ghost.rs")]),
        ScopeAuthorization::Grant(vec![String::from("src/ghost.rs")])
    );
    // A file outside every allow entry, or across a component boundary, is
    // OutsideAllow.
    assert!(matches!(
        policy.authorize(&[String::from("deploy/service.toml")]),
        ScopeAuthorization::Reject(ScopeRejection::OutsideAllow { .. })
    ));
    assert!(matches!(
        policy.authorize(&[String::from("srcx/letter.rs")]),
        ScopeAuthorization::Reject(ScopeRejection::OutsideAllow { .. })
    ));
    // A file entry covers its exact self only: never a sibling, a parent
    // directory, or anything beneath it.
    let file_only = ScopePolicy::snapshot("app", &[String::from("src/lib.rs")], &[])
        .expect("declarations should parse");
    assert_eq!(
        file_only.authorize(&[String::from("src/lib.rs")]),
        ScopeAuthorization::Grant(vec![String::from("src/lib.rs")])
    );
    assert!(matches!(
        file_only.authorize(&[String::from("src/")]),
        ScopeAuthorization::Reject(ScopeRejection::OutsideAllow { .. })
    ));
    // The root request needs the root allow entry; an explicit empty allow
    // allows nothing.
    assert!(matches!(
        policy.authorize(&[String::from("./")]),
        ScopeAuthorization::Reject(ScopeRejection::OutsideAllow { .. })
    ));
    let nothing = ScopePolicy::snapshot("app", &[], &[]).expect("empty declarations parse");
    assert!(matches!(
        nothing.authorize(&[String::from("src/lib.rs")]),
        ScopeAuthorization::Reject(ScopeRejection::OutsideAllow { .. })
    ));
    // A mixed request is never granted as its allowed subset.
    assert!(matches!(
        policy.authorize(&[
            String::from("src/helper.rs"),
            String::from("deploy/service.toml")
        ]),
        ScopeAuthorization::Reject(ScopeRejection::OutsideAllow { .. })
    ));
    // With the root allow, the root request grants the whole repository.
    let everything = ScopePolicy::snapshot("app", &[String::from(WHOLE_REPOSITORY)], &[])
        .expect("declarations should parse");
    assert_eq!(
        everything.authorize(&[String::from("./")]),
        ScopeAuthorization::Grant(vec![String::from(WHOLE_REPOSITORY)])
    );
}

#[test]
fn directory_requests_intersecting_denies_require_operator() {
    let policy = ScopePolicy::snapshot(
        "app",
        &[String::from("src/"), String::from("tests/")],
        &[String::from("src/private/")],
    )
    .expect("declarations should parse");

    // Denying a directory inside a requested directory rejects the request,
    // even though most of it is allowed: no allowed subset is granted.
    assert_eq!(
        policy.authorize(&[String::from("src/")]),
        ScopeAuthorization::Reject(ScopeRejection::DeniedPath {
            path: String::from("src/"),
            deny: String::from("src/private/"),
        })
    );
    // So does denying a file beneath the requested directory.
    let file_deny = ScopePolicy::snapshot(
        "app",
        &[String::from("src/")],
        &[String::from("src/lib.rs")],
    )
    .expect("declarations should parse");
    assert_eq!(
        file_deny.authorize(&[String::from("src/")]),
        ScopeAuthorization::Reject(ScopeRejection::DeniedPath {
            path: String::from("src/"),
            deny: String::from("src/lib.rs"),
        })
    );
    // In the other direction, a deny covers a requested file beneath it.
    assert_eq!(
        policy.authorize(&[String::from("src/private/keys.toml")]),
        ScopeAuthorization::Reject(ScopeRejection::DeniedPath {
            path: String::from("src/private/keys.toml"),
            deny: String::from("src/private/"),
        })
    );
    // A mixed request intersecting a deny is rejected whole.
    assert!(matches!(
        policy.authorize(&[String::from("src/lib.rs"), String::from("src/private/x.rs")]),
        ScopeAuthorization::Reject(ScopeRejection::DeniedPath { .. })
    ));
    // Outside the deny, at the component boundary, the request stands.
    assert_eq!(
        policy.authorize(&[String::from("srcx/private/x.rs")]),
        ScopeAuthorization::Reject(ScopeRejection::OutsideAllow {
            path: String::from("srcx/private/x.rs"),
            allow: vec![String::from("src/"), String::from("tests/")],
        })
    );
}

#[test]
fn invalid_scope_paths_fail_closed() {
    let policy = ScopePolicy::snapshot("app", &[String::from(WHOLE_REPOSITORY)], &[])
        .expect("declarations should parse");

    // One unusable declaration rejects the whole request, even beside an
    // eligible one.
    assert_eq!(
        policy.authorize(&[String::from("src/lib.rs"), String::from("../escape")]),
        ScopeAuthorization::Reject(ScopeRejection::InvalidPath {
            path: String::from("../escape"),
            problem: ScopePathError::ParentTraversal.to_string(),
        })
    );
    assert!(matches!(
        policy.authorize(&[String::from("   ")]),
        ScopeAuthorization::Reject(ScopeRejection::InvalidPath { .. })
    ));
    // Unusable policy entries fail closed at the named-target entry point.
    let mut configs = BTreeMap::new();
    configs.insert(
        String::from("app"),
        PumpTarget {
            scope_allow: Some(vec![String::from("/abs")]),
            ..PumpTarget::default()
        },
    );
    assert!(matches!(
        authorize(&configs, "app", &[String::from("src/lib.rs")]),
        ScopeAuthorization::Reject(ScopeRejection::InvalidPath { .. })
    ));
}

#[test]
fn scope_policy_is_target_local() {
    let configs = target_configs();

    assert_eq!(
        authorize(&configs, "app", &[String::from("src/lib.rs")]),
        ScopeAuthorization::Grant(vec![String::from("src/lib.rs")])
    );
    // Another target's policy never leaks in: `web`'s allow does not cover
    // `src/`, and `app`'s deny does not fire for `web`.
    assert!(matches!(
        authorize(&configs, "web", &[String::from("src/lib.rs")]),
        ScopeAuthorization::Reject(ScopeRejection::OutsideAllow { .. })
    ));
    // An unknown name is refused outright, naming the known targets, and
    // never inherits another target or the legacy default.
    assert_eq!(
        authorize(&configs, "missing", &[String::from("src/lib.rs")]),
        ScopeAuthorization::Reject(ScopeRejection::UnknownTarget {
            target: String::from("missing"),
            known: vec![String::from("app"), String::from("web")],
        })
    );
    // An omitted allow on a resolved target is the whole repository, its
    // own deny still wins, and none of that leaks across targets.
    let mut legacy = target_configs();
    legacy.insert(
        String::from("loose"),
        PumpTarget {
            scope_deny: vec![String::from("vendor/")],
            ..PumpTarget::default()
        },
    );
    assert_eq!(
        authorize(&legacy, "loose", &[String::from("docs/plan.md")]),
        ScopeAuthorization::Grant(vec![String::from("docs/plan.md")])
    );
    assert_eq!(
        authorize(&legacy, "loose", &[String::from("vendor/bundle/x.rs")]),
        ScopeAuthorization::Reject(ScopeRejection::DeniedPath {
            path: String::from("vendor/bundle/x.rs"),
            deny: String::from("vendor/"),
        })
    );
    assert_eq!(
        authorize(&legacy, "app", &[String::from("src/lib.rs")]),
        ScopeAuthorization::Grant(vec![String::from("src/lib.rs")])
    );
}

#[test]
fn configuration_findings_name_invalid_scope_entries() {
    let config = pump_config(
        "name = \"t\"\n\
         [pump]\n\
         committer_name = \"T\"\n\
         committer_email = \"t@t\"\n\
         [pump.targets.app]\n\
         path = \".\"\n\
         scope_allow = [\"./\", \"src/\"]\n\
         scope_deny = [\"vendor/\", \"src/private/\", \"/abs\"]\n",
    );
    let found = config.findings_for(Path::new("."), &[]);
    let scope_errors: Vec<&String> = found
        .errors
        .iter()
        .filter(|error| error.contains("[pump.targets.app] scope_"))
        .collect();
    assert_eq!(scope_errors.len(), 3, "{scope_errors:?}");
    assert!(
        scope_errors
            .iter()
            .any(|error| error.contains("scope_allow entry `src/`")
                && error.contains("is not an operator decision")),
        "{scope_errors:?}"
    );
    assert!(
        scope_errors
            .iter()
            .any(|error| error.contains("scope_deny entry `src/private/`")
                && error.contains("is not an operator decision")),
        "{scope_errors:?}"
    );
    assert!(
        scope_errors
            .iter()
            .any(|error| error.contains("scope_deny entry `/abs` is unusable")),
        "{scope_errors:?}"
    );
    // Entries outside the reserved trees normalize cleanly and stay silent.
    let clean = pump_config(
        "name = \"t\"\n\
         [pump]\n\
         committer_name = \"T\"\n\
         committer_email = \"t@t\"\n\
         [pump.targets.app]\n\
         path = \".\"\n\
         scope_allow = [\"./deploy/\", \"vendor\"]\n\
         scope_deny = [\"deploy/credentials.toml\"]\n",
    );
    let found = clean.findings_for(Path::new("."), &[]);
    assert!(
        !found.errors.iter().any(|error| error.contains("scope_")),
        "{found:?}"
    );
}
