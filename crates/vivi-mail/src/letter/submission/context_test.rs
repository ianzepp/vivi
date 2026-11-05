use super::*;
use crate::letter::roles::{Check, HandAnswer};

#[test]
fn cto_missing_new_fact() {
    let empty = serde_json::json!({"new_fact":"  "});
    assert!(super::cto_missing_new_fact(RoleKind::Cto, 3, "go", &empty));
    assert!(!super::cto_missing_new_fact(RoleKind::Cto, 2, "go", &empty));
}

#[test]
fn built_reports_failed_check() {
    let hand = Hand {
        answer: HandAnswer::Built,
        change: vec![],
        check: vec![Check {
            cmd: "cargo test".into(),
            result: CheckResult::Fail,
            seconds: 1.0,
            detail: None,
        }],
        deviations: vec![],
        follow_up: vec![],
        remaining: vec![],
        scope_paths: vec![],
        scope_reason: None,
        scope_design: false,
    };
    assert!(super::built_reports_failed_check(&hand));
}

#[test]
fn slicer_dependency_cycle_and_unknown_dependency() {
    let cycle = vec![
        ("A".into(), vec!["B".into()]),
        ("B".into(), vec!["A".into()]),
    ];
    assert!(
        super::slice_order(&cycle, &[])
            .unwrap_err()
            .contains("cycle")
    );
    let unknown = vec![("A".into(), vec!["Z".into()])];
    assert!(
        super::slice_order(&unknown, &[])
            .unwrap_err()
            .contains("unknown slice")
    );
    assert!(super::slice_order(&unknown, &["Z".into()]).is_ok());
}

#[test]
fn wrong_slicer_mode() {
    let slicer = Slicer {
        answer: crate::letter::roles::SlicerAnswer::Sliced,
        mode: SlicerMode::Split,
        slice: vec![],
        reason: None,
    };
    assert!(!super::slicer_mode_matches(&slicer, SlicerMode::Rework));
}

#[test]
fn nonempty_scope_and_known_target() {
    assert!(!super::scope_nonempty(&["  ".into()]));
    assert!(!super::target_known("missing", &[]));
}
