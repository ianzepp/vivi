//! Tests for the decision-seat resolver.

use super::recipient;
use crate::error::VivariumError;
use crate::mailspace::RoleUpdate;
use crate::pump::config::PumpConfig;
use crate::pump::fixture::project;

/// A config whose decision seat is `seat`.
fn config(seat: &str) -> PumpConfig {
    PumpConfig {
        decision_to: seat.into(),
        ..PumpConfig::default()
    }
}

fn err_of(result: Result<String, VivariumError>) -> String {
    result.err().expect("resolution should fail").to_string()
}

/// A role of kind `kind`.
fn role_of_kind(kind: &str) -> RoleUpdate {
    RoleUpdate {
        kind: Some(Some(kind.into())),
        ..RoleUpdate::default()
    }
}

#[test]
fn the_default_seat_resolves_the_mind_role() {
    let project = project();
    assert_eq!(
        recipient(&project.mailspace, &PumpConfig::default()).unwrap(),
        "mind",
        "the default seat is the mind role"
    );
}

#[test]
fn a_seat_may_name_an_alias_or_the_role_address() {
    let mut project = project();
    project
        .mailspace
        .add_role("chief", role_of_kind("mind"))
        .unwrap();
    project
        .mailspace
        .rename_identity("chief", "deputy")
        .unwrap();
    let mailspace = &project.mailspace;
    assert_eq!(
        recipient(mailspace, &config("chief")).unwrap(),
        "deputy",
        "an alias resolves to the canonical role name"
    );
    assert_eq!(
        recipient(mailspace, &config(&mailspace.address_for("deputy"))).unwrap(),
        "deputy",
        "the role's own address resolves too"
    );
}

#[test]
fn a_blank_seat_fails_without_falling_back() {
    let project = project();
    let mut pump = config("   ");
    pump.from = String::from("operator");
    pump.board.reply_to = Some(String::from("operator@bd371d96.local"));
    let err = err_of(recipient(&project.mailspace, &pump));
    assert!(err.contains("blank"), "{err}");
}

#[test]
fn an_unknown_seat_fails() {
    let project = project();
    let err = err_of(recipient(&project.mailspace, &config("nobody")));
    assert!(err.contains("unknown local role 'nobody'"), "{err}");
}

#[test]
fn a_wrong_kind_seat_fails_even_when_fallbacks_are_configured() {
    let mut project = project();
    project
        .mailspace
        .add_role("chief", role_of_kind("planner"))
        .unwrap();
    let mut pump = config("chief");
    pump.from = String::from("operator");
    pump.board.reply_to = Some(String::from("operator@bd371d96.local"));
    let err = err_of(recipient(&project.mailspace, &pump));
    assert!(err.contains("'chief'"), "{err}");
    assert!(err.contains("planner"), "{err}");
    assert!(err.contains("mind"), "{err}");
}

#[test]
fn a_kindless_seat_fails() {
    let project = project();
    let err = err_of(recipient(&project.mailspace, &config("elsewhere")));
    assert!(err.contains("'elsewhere'"), "{err}");
    assert!(err.contains("none"), "{err}");
}
