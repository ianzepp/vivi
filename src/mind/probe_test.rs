//! Tests for [`crate::mind::probe`].

use tempfile::tempdir;

use super::*;

#[test]
fn health_probe_on_a_folder_without_a_mailspace_is_unknown_not_a_panic() {
    let dir = tempdir().unwrap();
    let signals = HealthProbe.probe(dir.path());
    assert_eq!(signals.root, dir.path());
    assert_eq!(signals.verdict, "unknown");
    assert!(!signals.reasons.is_empty());
    assert!(!signals.alarming());
}

#[test]
fn fingerprint_changes_with_any_signal() {
    let base = ProjectSignals {
        root: "/a".into(),
        verdict: "ok".into(),
        ..ProjectSignals::default()
    };
    let held = ProjectSignals {
        held_steps: 1,
        ..base.clone()
    };
    assert_ne!(base.fingerprint(), held.fingerprint());
    let reasons_only = ProjectSignals {
        reasons: vec!["noise".into()],
        ..base.clone()
    };
    assert_eq!(base.fingerprint(), reasons_only.fingerprint());
}
