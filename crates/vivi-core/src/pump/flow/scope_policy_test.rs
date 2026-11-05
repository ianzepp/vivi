use super::{covers, declared_problems, valid_path};
use vivi_mail::letter::roles::Slice;

#[test]
fn repository_paths_reject_traversal_absolute_and_private_roots() {
    for path in [
        "../x",
        "a/../../x",
        "/tmp/x",
        ".git",
        ".git/config",
        ".vivi/db",
    ] {
        assert!(!valid_path(path), "{path}");
    }
    for path in ["src/lib.rs", " src.rs", "src "] {
        assert!(valid_path(path), "{path}");
    }
    assert!(!valid_path("a/../b"));
}

#[test]
fn coverage_uses_components_without_trimming() {
    assert!(covers("src", "src/lib.rs"));
    assert!(covers("src/", "src/lib.rs"));
    assert!(!covers("src", "src2/lib.rs"));
    assert!(!covers(" src", "src/lib.rs"));
}

#[test]
fn declared_scopes_reject_unsafe_paths() {
    let slice = Slice {
        id: "a".into(),
        title: "A".into(),
        brief: "brief".into(),
        target: "src/a.rs".into(),
        write_scope: vec!["../secret".into()],
        deps: vec![],
    };
    assert!(!declared_problems(&[slice], &["src/a.rs".into()]).is_empty());
}
