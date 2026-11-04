use super::*;
use std::fs;

#[test]
fn normalize_keeps_project_relative_posix_path() {
    let root = tempfile::tempdir().unwrap();
    let nested = root.path().join("docs/factory");
    fs::create_dir_all(&nested).unwrap();
    let file = nested.join("sample-goal.md");
    fs::write(&file, "# Goal\n").unwrap();

    let rel = normalize_goal_path(root.path(), Path::new("docs/factory/sample-goal.md")).unwrap();
    assert_eq!(rel, "docs/factory/sample-goal.md");

    let from_abs = normalize_goal_path(root.path(), &file).unwrap();
    assert_eq!(from_abs, "docs/factory/sample-goal.md");
}

#[test]
fn normalize_rejects_outside_project() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("other.md");
    fs::write(&file, "x").unwrap();
    let err = normalize_goal_path(root.path(), &file).unwrap_err();
    assert!(err.to_string().contains("inside project root"), "{err}");
}

#[test]
fn goal_add_list_drop_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let ms = Mailspace::init(Some(dir.path())).unwrap();
    let path = dir.path().join("docs/campaign-goal.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "# Goal: campaign\n").unwrap();

    let added = ms
        .goal_add(
            Path::new("docs/campaign-goal.md"),
            Some("campaign"),
            Some("mind"),
        )
        .unwrap();
    assert!(added.handle.starts_with("gol_"));
    assert_eq!(added.path, "docs/campaign-goal.md");
    assert_eq!(added.label.as_deref(), Some("campaign"));
    assert!(added.exists);

    let listed = ms.goal_list().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].handle, added.handle);

    let shown = ms.goal_show(&added.handle).unwrap();
    assert_eq!(shown.path, "docs/campaign-goal.md");

    let dropped = ms.goal_drop("docs/campaign-goal.md").unwrap();
    assert_eq!(dropped.handle, added.handle);
    assert!(ms.goal_list().unwrap().is_empty());
}
