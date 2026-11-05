use super::*;
use crate::letter::roles::{Check, CheckResult, Hand, Slice};

#[test]
fn cto_missing_new_fact() {
    let empty = serde_json::json!({});
    assert!(super::cto_missing_new_fact(3, "no-op", &empty));
    assert!(!super::cto_missing_new_fact(3, "lock", &empty));
    assert!(!super::cto_missing_new_fact(2, "no-op", &empty));
}

#[test]
fn built_reports_failed_check() {
    let hand = Hand {
        answer: HandAnswer::Built,
        change: vec![],
        check: vec![Check {
            cmd: "test".into(),
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

fn slice(id: &str, deps: &[&str]) -> Slice {
    Slice {
        id: id.into(),
        title: "".into(),
        brief: "".into(),
        target: "vivi".into(),
        write_scope: vec!["src/a".into()],
        deps: deps.iter().map(|s| (*s).into()).collect(),
    }
}

#[test]
fn slicer_dependency_cycle() {
    assert!(
        super::topological_order(&[slice("a", &["b"]), slice("b", &["a"])], &[])
            .unwrap_err()
            .contains("cycle")
    );
}
#[test]
fn unknown_dependency() {
    assert!(
        super::topological_order(&[slice("a", &["missing"])], &[])
            .unwrap_err()
            .contains("unknown slice")
    );
}
#[test]
fn earlier_dependency_is_accepted() {
    assert!(super::topological_order(&[slice("a", &["old"])], &["old".into()]).is_ok());
}
#[test]
fn wrong_slicer_mode() {
    assert!(super::wrong_slicer_mode(
        SlicerMode::Split,
        SlicerMode::Rework
    ));
}
#[test]
fn validates_targets_and_scopes() {
    let errors = super::validate_slices(
        &[Slice {
            target: "unknown".into(),
            write_scope: vec![],
            ..slice("a", &[])
        }],
        &[],
        &[],
    );
    assert_eq!(errors.len(), 2);
}
