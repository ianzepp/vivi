//! Offline validation behavior, independent of pump state and evidence.

use super::{Refusal, cto_needs_fact, failing_hand_checks, validate_reply};
use crate::pump::envelope::context::ReplyContract;
use vivi_mail::letter::LetterError;
use vivi_mail::letter::roles::RoleKind;

fn reply(front: &str) -> String {
    format!("+++\n{front}+++\nreport")
}

fn contract(role: RoleKind) -> ReplyContract {
    ReplyContract::for_role(role)
}

#[test]
fn every_role_uses_the_typed_role_parser() {
    let cases = [
        (RoleKind::Researcher, "answer = 'READY'\n"),
        (RoleKind::Planner, "answer = 'planned'\n"),
        (RoleKind::Admitter, "answer = 'admitted'\n"),
        (
            RoleKind::Slicer,
            "answer = 'sliced'\n[[slice]]\nid='S1'\ntitle='T'\nbrief='B'\ntarget='default'\n",
        ),
        (RoleKind::Auditor, "answer = 'clean_pass'\n"),
        (RoleKind::Judge, "answer = 'block'\nreason='why'\n"),
        (RoleKind::Cto, "answer = 'lock'\nreason='why'\n"),
        (RoleKind::Merge, "answer = 'merged'\n"),
        (RoleKind::Hand, "answer = 'built'\n"),
        (RoleKind::Repairer, "answer = 'built'\n"),
        (RoleKind::HeadCeo, "answer = 'reviewed'\n"),
    ];
    for (role, front) in cases {
        assert!(
            validate_reply(&contract(role), &reply(front)).is_ok(),
            "{role:?}"
        );
    }
}

#[test]
fn parser_refusals_keep_letter_error_variants_and_table_location() {
    assert!(matches!(
        validate_reply(&contract(RoleKind::Hand), "no envelope"),
        Err(Refusal::Envelope(LetterError::NoOpening))
    ));
    assert!(matches!(
        validate_reply(&contract(RoleKind::Hand), "+++\nanswer='built'"),
        Err(Refusal::Envelope(LetterError::Unclosed))
    ));
    let below = "+++\nanswer='built'\n+++\nbody\n[[check]]\ncmd='x'\n";
    assert!(
        matches!(validate_reply(&contract(RoleKind::Hand), below), Err(Refusal::Envelope(LetterError::TableBelowFence { line: 5, table })) if table == "[[check]]")
    );
}

#[test]
fn toml_type_and_unknown_key_failures_remain_typed_parser_errors() {
    for front in [
        "answer = [\"built\"]\n",
        "answer = 'built'\nproof = 1\n",
        "answer = 'built'\nchange = 'wrong type'\n",
    ] {
        assert!(matches!(
            validate_reply(&contract(RoleKind::Hand), &reply(front)),
            Err(Refusal::Envelope(LetterError::Toml { .. }))
        ));
    }
}

#[test]
fn preamble_uses_the_same_role_reply_parity() {
    let parsed = validate_reply(
        &contract(RoleKind::Hand),
        "Preamble.\n\n+++\nanswer='built'\n+++\nReport.",
    )
    .unwrap();
    assert_eq!(parsed.body, "Preamble.\n\nReport.");
}

#[test]
fn cto_run_three_rule_is_named_and_lock_is_exempt() {
    let mut c = contract(RoleKind::Cto);
    c.run = Some(3);
    let bad = validate_reply(&c, &reply("answer='plan'\nreason='next'\n")).unwrap_err();
    assert!(
        matches!(bad, Refusal::Issues(issues) if issues.iter().any(|issue| issue.name == "cto_new_fact_required"))
    );
    assert!(validate_reply(&c, &reply("answer='lock'\nreason='done'\n")).is_ok());
    assert!(!cto_needs_fact(
        "plan",
        &serde_json::json!({"new_fact":" fact "}),
        3
    ));
}

#[test]
fn built_with_failed_check_is_a_named_contradiction() {
    let bad = reply("answer='built'\n[[check]]\ncmd='cargo test'\nresult='fail'\nseconds=1\n");
    let refused = validate_reply(&contract(RoleKind::Hand), &bad).unwrap_err();
    assert!(
        matches!(refused, Refusal::Issues(issues) if issues.iter().any(|issue| issue.name == "built_with_failed_checks"))
    );
    assert_eq!(
        failing_hand_checks("built", [("x", true), ("y", false)].into_iter(),),
        ["x"]
    );
    assert!(failing_hand_checks("failed", [("x", true)]).is_empty());
}

#[test]
fn slicer_checks_keep_unavailable_context_distinct_from_empty_context() {
    let text = reply(
        "answer='sliced'\n[[slice]]\nid='S1'\ntitle='T'\nbrief='B'\ntarget='default'\nwrite_scope=['src/a']\ndeps=['OLD']\n",
    );
    let mut c = contract(RoleKind::Slicer);
    c.slicer = Some(crate::pump::envelope::context::SlicerRequirements {
        mode: None,
        configured_targets: Some(Vec::new()),
        check_targets: true,
        check_scope: true,
        earlier_slice_ids: Some(Vec::new()),
    });
    assert!(
        matches!(validate_reply(&c, &text), Err(Refusal::Issues(issues)) if issues.iter().any(|i| i.name == "slicer_target_unconfigured") && issues.iter().any(|i| i.name == "slicer_dependency_unknown"))
    );
    c.slicer.as_mut().unwrap().configured_targets = None;
    c.slicer.as_mut().unwrap().earlier_slice_ids = None;
    assert!(validate_reply(&c, &text).is_ok());
}
