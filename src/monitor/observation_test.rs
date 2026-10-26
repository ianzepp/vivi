//! Unit tests for [`crate::monitor::observation`].

use super::*;
use std::path::Path;

#[test]
fn issue_kinds_serialize_as_kebab_case_identities() {
    assert_eq!(
        serde_json::to_value(IssueKind::ProjectConfig).unwrap(),
        "project-config"
    );
    assert_eq!(
        serde_json::to_value(IssueKind::MailStore).unwrap(),
        "mail-store"
    );
    assert_eq!(
        serde_json::to_value(IssueKind::PoolState).unwrap(),
        "pool-state"
    );
    assert_eq!(
        serde_json::to_value(IssueKind::PumpState).unwrap(),
        "pump-state"
    );
}

#[test]
fn read_issue_serializes_kind_identity_and_detail() {
    let issue = ReadIssue {
        kind: IssueKind::MailStore,
        detail: "display text only".to_string(),
    };
    let value = serde_json::to_value(&issue).unwrap();
    assert_eq!(value["kind"], "mail-store");
    assert_eq!(value["detail"], "display text only");
}

#[test]
fn issue_kinds_are_distinguishable_identities() {
    assert_ne!(IssueKind::ProjectConfig, IssueKind::MailStore);
    assert_ne!(IssueKind::PoolState, IssueKind::PumpState);
}

#[test]
fn store_presence_keeps_path_and_presence_identity() {
    let store = StorePresence {
        path: PathBuf::from("/proj/.vivi/mail.sqlite"),
        kind: IssueKind::MailStore,
        present: false,
    };
    assert_eq!(store.path, Path::new("/proj/.vivi/mail.sqlite"));
    assert!(!store.present);

    let value = serde_json::to_value(&store).unwrap();
    assert_eq!(value["path"], "/proj/.vivi/mail.sqlite");
    assert_eq!(value["kind"], "mail-store");
    assert_eq!(value["present"], false);
}

#[test]
fn warning_keeps_project_root_identity() {
    let warning = ProjectWarning {
        root: PathBuf::from("/srv/proj-a"),
        issues: vec![ReadIssue {
            kind: IssueKind::PoolState,
            detail: "unreadable".to_string(),
        }],
    };
    assert_eq!(warning.root, Path::new("/srv/proj-a"));

    let value = serde_json::to_value(&warning).unwrap();
    assert_eq!(value["root"], "/srv/proj-a");
    assert_eq!(value["issues"][0]["kind"], "pool-state");
}

#[test]
fn empty_observation_is_complete() {
    let observation = ProjectObservation::default();
    assert!(observation.is_complete());
    assert!(!observation.is_degraded());
    assert!(observation.issues.is_empty());
}

#[test]
fn observation_with_issue_is_degraded() {
    let mut observation = ProjectObservation::default();
    observation.issues.push(ReadIssue {
        kind: IssueKind::ProjectConfig,
        detail: "config parse failed".to_string(),
    });
    assert!(observation.is_degraded());
    assert!(!observation.is_complete());
}

#[test]
fn store_absence_alone_does_not_degrade_observation() {
    let observation = ProjectObservation {
        stores: vec![StorePresence {
            path: PathBuf::from("/proj/.vivi/pump.sqlite"),
            kind: IssueKind::PumpState,
            present: false,
        }],
        ..ProjectObservation::default()
    };
    assert!(observation.is_complete());
}
