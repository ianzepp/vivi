use std::path::{Path, PathBuf};

use crate::mailspace::Mailspace;

use super::project_rules_snapshot;

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(tmp.path())).unwrap();
    space.add_identity("mind").unwrap();
    (tmp, space)
}

/// Every file under `dir` whose bytes contain `needle`.
fn files_containing(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(files_containing(&path, needle));
        } else if let Ok(bytes) = std::fs::read(&path)
            && String::from_utf8_lossy(&bytes).contains(needle)
        {
            found.push(path);
        }
    }
    found
}

#[test]
fn the_snapshot_holds_only_active_project_rules() {
    let (_tmp, space) = space();
    space
        .save_rule("mind", "shared rule", "everyone follows this", false)
        .unwrap();
    space
        .save_rule("mind", "private rule", "only me", true)
        .unwrap();
    let retired = space
        .save_rule("mind", "old rule", "no longer applies", false)
        .unwrap();
    space.retire_rule("mind", &retired, None).unwrap();

    let snapshot = project_rules_snapshot(&space).unwrap();

    assert!(snapshot.contains("## shared rule"), "{snapshot}");
    assert!(snapshot.contains("everyone follows this"), "{snapshot}");
    assert!(!snapshot.contains("private rule"), "{snapshot}");
    assert!(!snapshot.contains("old rule"), "{snapshot}");
}

#[test]
fn an_unreadable_project_rule_is_skipped_and_the_others_remain() {
    let (tmp, space) = space();
    space
        .save_rule("mind", "good rule", "kept body", false)
        .unwrap();
    space
        .save_rule("mind", "broken rule", "lost-body-marker", false)
        .unwrap();
    // An empty blob is not a mail message: the list still sees a project rule
    // (the scope defaults to project) but its body cannot be read.
    let blobs = files_containing(tmp.path(), "lost-body-marker");
    assert_eq!(blobs.len(), 1, "{blobs:?}");
    std::fs::write(&blobs[0], b"").unwrap();

    let snapshot = project_rules_snapshot(&space).unwrap();

    assert!(snapshot.contains("## good rule"), "{snapshot}");
    assert!(snapshot.contains("kept body"), "{snapshot}");
    assert!(!snapshot.contains("broken rule"), "{snapshot}");
}
