use super::{covers, declared_problems, invalid_path};
use vivi_mail::letter::roles::Slice;

fn slice(scope: &[&str]) -> Slice {
    Slice {
        id: "a".into(),
        title: "a".into(),
        brief: "b".into(),
        target: "local".into(),
        write_scope: scope.iter().map(|path| (*path).into()).collect(),
        deps: Vec::new(),
    }
}

#[test]
fn validates_repository_tree_paths_and_keeps_whitespace() {
    for path in [
        "../secret",
        "src/../secret",
        "/absolute",
        ".git/config",
        ".vivi/data",
    ] {
        assert!(invalid_path(path).is_some(), "{path}");
    }
    for path in ["src/a.rs", "src/test.rs", " src/file ", "src/.gitignore"] {
        assert_eq!(invalid_path(path), None, "{path}");
    }
    assert!(covers("src", "src/a.rs"));
    assert!(!covers("src", "src2/a.rs"));
    assert!(covers(" src ", " src /a"));
}

#[test]
fn declared_scope_rejects_protected_and_traversal_paths() {
    let issues = declared_problems(&[slice(&["safe/file", "../escape"])], &["local".into()]);
    assert_eq!(issues.len(), 1);
}
