use super::{RoleFacts, validate_publish};
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
fn intake_publication_authority() {
    let role = |name: &str, kind: &str| RoleFacts {
        name: name.to_owned(),
        kind: Some(kind.to_owned()),
        band: None,
        thinking: None,
        charter: String::new(),
        read_only: None,
    };
    let merge = role("merge", "merge");
    let hand = role("hand", "hand");
    let repairer = role("repairer", "repairer");
    let unknown = role("unknown", "unknown");
    let mains = vec!["trunk".to_owned()];

    assert!(
        validate_publish(
            Some(&merge),
            Some("refs/heads/vivi/intake/item-1"),
            None,
            &mains
        )
        .is_ok()
    );
    assert!(validate_publish(Some(&merge), Some("refs/heads/trunk"), None, &mains).is_ok());
    for denied in [&hand, &repairer, &unknown] {
        assert!(
            validate_publish(
                Some(denied),
                Some("refs/heads/vivi/intake/item-1"),
                None,
                &mains
            )
            .is_err()
        );
    }
    for destination in [
        "refs/heads/vivi/intake/",
        "refs/heads/vivi/intake/item-1/nested",
        "refs/heads/vivi/intake/item-1/",
        "refs/heads/vivi/intake-item-1",
        "refs/heads/vivi/arbitrary/item-1",
    ] {
        assert!(validate_publish(Some(&merge), Some(destination), None, &mains).is_err());
    }
    assert!(validate_publish(Some(&merge), Some("refs/heads/unconfigured"), None, &mains).is_err());
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
