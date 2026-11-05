//! Tests for [`super`]: intake values are literal heads, never selectors.

use super::{IntakeIssue, IntakeRung, branch_issue};

#[test]
fn a_peer_slash_branch_is_a_literal_head() {
    assert_eq!(branch_issue("peer/main", "main"), None);
    assert_eq!(branch_issue("release", "main"), None);
    assert_eq!(branch_issue("feature/intake-2", "main"), None);
}

#[test]
fn selectors_and_special_names_are_rejected_by_category() {
    let cases = [
        ("", IntakeIssue::Empty),
        (" main", IntakeIssue::NotPlainRef),
        ("https://host/org/repo.git", IntakeIssue::Url),
        ("git@host:org/repo.git", IntakeIssue::Url),
        ("main:peer/main", IntakeIssue::Refspec),
        ("v1.*", IntakeIssue::Wildcard),
        ("refs/heads/main", IntakeIssue::FullRef),
        ("refs/remotes/origin/main", IntakeIssue::RemoteTracking),
        ("remotes/origin/main", IntakeIssue::RemoteTracking),
        ("vivi/need/abcd1234", IntakeIssue::ScratchNamespace),
        ("main", IntakeIssue::SelfIntake),
    ];
    for (branch, expected) in cases {
        assert_eq!(branch_issue(branch, "main"), Some(expected), "{branch:?}");
    }
}

#[test]
fn rungs_are_only_task_need_and_main() {
    assert_eq!(IntakeRung::default(), IntakeRung::Need);
    assert_eq!(IntakeRung::NAMES.map(|name| name), ["task", "need", "main"]);
    assert_eq!(IntakeRung::Main.to_string(), "main");
}
