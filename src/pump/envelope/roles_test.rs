//! Tests for the typed role envelopes.

use super::{RoleKind, parse_for};
use crate::pump::envelope::EnvelopeError;
use crate::pump::flow::def::NodeClass;

fn hand(front: &str) -> String {
    format!("+++\n{front}+++\nreport\n")
}

#[test]
fn from_class_maps_role_nodes_only() {
    assert_eq!(
        RoleKind::from_class(&NodeClass::Slicer),
        Some(RoleKind::Slicer)
    );
    assert_eq!(RoleKind::from_class(&NodeClass::Cto), Some(RoleKind::Cto));
    assert_eq!(
        RoleKind::from_class(&NodeClass::Merge),
        Some(RoleKind::Merge)
    );
    assert_eq!(RoleKind::from_class(&NodeClass::RungNeed), None);
    assert_eq!(RoleKind::from_class(&NodeClass::Fanout), None);
    assert_eq!(RoleKind::from_class(&NodeClass::Decision), None);
}

#[test]
fn a_valid_hand_envelope_parses() {
    let text = hand(
        "answer = \"built\"\nchange = [\"src/a.rs\"]\ndeviations = ['moved a call']\n\
         remaining = []\n\n[[check]]\ncmd = \"cargo test\"\nresult = \"pass\"\nseconds = 94\n\n\
         [[follow_up]]\npaths = [\"src/b.rs\"]\nneed = 'fix b'\n",
    );
    let parsed = parse_for(RoleKind::Hand, &text).unwrap();
    assert_eq!(parsed.answer, "built");
    assert_eq!(parsed.body, "report");
    assert_eq!(parsed.front["change"][0], "src/a.rs");
    assert_eq!(parsed.front["check"][0]["seconds"], 94);
    assert_eq!(parsed.front["check"][0]["result"], "pass");
    assert_eq!(parsed.front["follow_up"][0]["need"], "fix b");
}

#[test]
fn an_empty_change_list_is_a_valid_built() {
    let parsed = parse_for(RoleKind::Hand, &hand("answer = \"built\"\n")).unwrap();
    assert_eq!(parsed.front["change"], serde_json::json!([]));
    assert_eq!(parsed.front["scope_design"], false);
}

#[test]
fn a_hand_unknown_key_is_refused() {
    let err = parse_for(RoleKind::Hand, &hand("answer = \"built\"\nproof = 1\n")).unwrap_err();
    assert!(
        matches!(err, EnvelopeError::Toml { line: 3, .. }),
        "{err:?}"
    );
}

#[test]
fn a_hand_wrong_type_is_refused_with_its_line() {
    let err = parse_for(
        RoleKind::Hand,
        &hand("answer = \"built\"\nchange = \"src/a.rs\"\n"),
    )
    .unwrap_err();
    assert!(
        matches!(err, EnvelopeError::Toml { line: 3, .. }),
        "{err:?}"
    );
}

#[test]
fn a_bad_check_result_is_refused() {
    let text =
        hand("answer = \"built\"\n[[check]]\ncmd = \"x\"\nresult = \"timeout\"\nseconds = 1\n");
    assert!(matches!(
        parse_for(RoleKind::Hand, &text),
        Err(EnvelopeError::Toml { .. })
    ));
}

#[test]
fn blocked_scope_needs_paths_and_a_reason() {
    let no_paths = hand("answer = \"blocked-scope\"\nscope_reason = 'needs a re-export'\n");
    let err = parse_for(RoleKind::Hand, &no_paths).unwrap_err();
    assert!(
        matches!(err, EnvelopeError::Invalid(ref m) if m.contains("scope_paths")),
        "{err:?}"
    );

    let no_reason = hand("answer = \"blocked-scope\"\nscope_paths = [\"src/m.rs\"]\n");
    let err = parse_for(RoleKind::Hand, &no_reason).unwrap_err();
    assert!(
        matches!(err, EnvelopeError::Invalid(ref m) if m.contains("scope_reason")),
        "{err:?}"
    );

    let ok = hand(
        "answer = \"blocked-scope\"\nscope_paths = [\"src/m.rs\"]\n\
         scope_reason = 'needs a re-export'\nscope_design = true\n",
    );
    let parsed = parse_for(RoleKind::Hand, &ok).unwrap();
    assert_eq!(parsed.answer, "blocked-scope");
    assert_eq!(parsed.front["scope_design"], true);
}

#[test]
fn a_valid_planner_parses_and_reason_is_conditional() {
    let planned = parse_for(RoleKind::Planner, &hand("answer = \"planned\"\n")).unwrap();
    assert_eq!(planned.answer, "planned");

    let err = parse_for(RoleKind::Planner, &hand("answer = \"hold\"\n")).unwrap_err();
    assert!(matches!(err, EnvelopeError::Invalid(_)), "{err:?}");

    let held = parse_for(
        RoleKind::Planner,
        &hand("answer = \"hold\"\nreason = 'waiting'\n"),
    )
    .unwrap();
    assert_eq!(held.front["reason"], "waiting");

    let err = parse_for(RoleKind::Planner, &hand("answer = \"maybe\"\n")).unwrap_err();
    assert!(matches!(err, EnvelopeError::Toml { .. }), "{err:?}");
}

const SLICE: &str = "[[slice]]\nid = \"S1\"\ntitle = \"t\"\nbrief = 'b'\ntarget = \"src\"\n\
                     write_scope = [\"src/a.rs\"]\ndeps = []\n";

#[test]
fn a_valid_slicer_parses() {
    let parsed = parse_for(
        RoleKind::Slicer,
        &hand(&format!("answer = \"sliced\"\n{SLICE}")),
    )
    .unwrap();
    assert_eq!(parsed.answer, "sliced");
    assert_eq!(parsed.front["mode"], "slice");
    assert_eq!(parsed.front["slice"][0]["write_scope"][0], "src/a.rs");
}

#[test]
fn slicer_mode_rework_parses_and_repair_is_refused() {
    let with_mode = |mode: &str| hand(&format!("answer = \"sliced\"\nmode = \"{mode}\"\n{SLICE}"));
    let parsed = parse_for(RoleKind::Slicer, &with_mode("rework")).unwrap();
    assert_eq!(parsed.front["mode"], "rework");
    let err = parse_for(RoleKind::Slicer, &with_mode("repair")).unwrap_err();
    assert!(matches!(err, EnvelopeError::Toml { .. }), "{err:?}");
}

#[test]
fn a_slicer_proof_key_is_refused() {
    let text = hand(&format!(
        "answer = \"sliced\"\n{SLICE}proof = \"cargo test\"\n"
    ));
    let err = parse_for(RoleKind::Slicer, &text).unwrap_err();
    assert!(matches!(err, EnvelopeError::Toml { .. }), "{err:?}");
}

#[test]
fn slicer_rules_by_answer() {
    let err = parse_for(RoleKind::Slicer, &hand("answer = \"sliced\"\n")).unwrap_err();
    assert!(matches!(err, EnvelopeError::Invalid(_)), "{err:?}");

    let err = parse_for(RoleKind::Slicer, &hand("answer = \"escalate\"\n")).unwrap_err();
    assert!(matches!(err, EnvelopeError::Invalid(_)), "{err:?}");

    let err = parse_for(
        RoleKind::Slicer,
        &hand(&format!("answer = \"escalate\"\nreason = 'x'\n{SLICE}")),
    )
    .unwrap_err();
    assert!(matches!(err, EnvelopeError::Invalid(_)), "{err:?}");

    let ok = parse_for(
        RoleKind::Slicer,
        &hand("answer = \"escalate\"\nreason = 'too big'\n"),
    )
    .unwrap();
    assert_eq!(ok.answer, "escalate");
}

#[test]
fn split_mode_needs_two_to_four_numbered_ids() {
    let two = |a: &str, b: &str| {
        hand(&format!(
            "answer = \"sliced\"\nmode = \"split\"\n{}{}",
            SLICE.replace("S1", a),
            SLICE.replace("S1", b)
        ))
    };
    assert!(parse_for(RoleKind::Slicer, &two("S1.1", "S1.2")).is_ok());
    assert!(parse_for(RoleKind::Slicer, &two("S1.1", "S2")).is_err());

    let one = hand(&format!(
        "answer = \"sliced\"\nmode = \"split\"\n{}",
        SLICE.replace("S1", "S1.1")
    ));
    assert!(parse_for(RoleKind::Slicer, &one).is_err());
}

#[test]
fn a_follow_up_optional_flag_parses_and_defaults_to_false() {
    let text = hand(
        "answer = \"built\"\n\n[[follow_up]]\npaths = [\"src/a.rs\"]\nneed = 'a'\noptional = true\n\n\
         [[follow_up]]\npaths = [\"src/b.rs\"]\nneed = 'b'\n",
    );
    let parsed = parse_for(RoleKind::Hand, &text).unwrap();
    assert_eq!(parsed.front["follow_up"][0]["optional"], true);
    assert_eq!(parsed.front["follow_up"][1].get("optional"), None);
}

#[test]
fn a_non_optional_follow_up_serializes_without_the_optional_key() {
    let row = super::FollowUp {
        paths: vec!["src/a.rs".to_string()],
        need: "a".to_string(),
        optional: false,
    };
    let json = serde_json::to_value(&row).unwrap();
    assert_eq!(json.as_object().unwrap().len(), 2);
    let optional = super::FollowUp {
        optional: true,
        ..row
    };
    assert_eq!(serde_json::to_value(&optional).unwrap()["optional"], true);
}

const REPAIRER_BUILT: &str = "answer = \"built\"\nchange = [\"src/a.rs\"]\n\
    [[check]]\ncmd = \"cargo test\"\nresult = \"pass\"\nseconds = 12\n";

#[test]
fn from_class_maps_the_repairer() {
    assert_eq!(
        RoleKind::from_class(&NodeClass::Repairer),
        Some(RoleKind::Repairer)
    );
}

#[test]
fn a_valid_repairer_envelope_parses() {
    let parsed = parse_for(RoleKind::Repairer, &hand(REPAIRER_BUILT)).unwrap();
    assert_eq!(parsed.answer, "built");
    assert_eq!(parsed.front["change"][0], "src/a.rs");
    let failed = parse_for(RoleKind::Repairer, &hand("answer = \"failed\"\n")).unwrap();
    assert_eq!(failed.answer, "failed");
}

#[test]
fn a_repairer_refuses_overtime_blocked_scope_and_unknown_keys() {
    for word in ["overtime", "blocked-scope"] {
        let text = hand(&format!("answer = \"{word}\"\n"));
        assert!(parse_for(RoleKind::Repairer, &text).is_err(), "{word}");
    }
    let with_sha = hand(&format!("{REPAIRER_BUILT}sha = \"abc\"\n"));
    assert!(parse_for(RoleKind::Repairer, &with_sha).is_err());
}

#[test]
fn a_repairer_has_no_diff_cap() {
    let change: Vec<String> = (0..500).map(|n| format!("\"src/f{n}.rs\"")).collect();
    let text = hand(&format!(
        "answer = \"built\"\nchange = [{}]\n",
        change.join(", ")
    ));
    let parsed = parse_for(RoleKind::Repairer, &text).unwrap();
    assert_eq!(parsed.front["change"].as_array().map(Vec::len), Some(500));
}

#[test]
fn a_reply_with_a_prose_preamble_parses_and_keeps_the_preamble() {
    let text = format!(
        "All checks pass on the delivered tip. Final receipt:\n\n{}",
        hand("answer = \"built\"\nchange = [\"src/a.rs\"]\nremaining = []\n")
    );
    let parsed = parse_for(RoleKind::Hand, &text).unwrap();
    assert_eq!(parsed.answer, "built");
    assert_eq!(
        parsed.body,
        "All checks pass on the delivered tip. Final receipt:\n\nreport"
    );
}

#[test]
fn a_reply_with_no_fence_line_is_still_no_opening() {
    let err = parse_for(RoleKind::Hand, "done, everything passes\n").unwrap_err();
    assert_eq!(err, EnvelopeError::NoOpening);
}

#[test]
fn a_preamble_does_not_hide_a_bad_envelope() {
    let text = format!(
        "Here it is:\n{}",
        hand("[hand]\nanswer = \"built\"\nremaining = []\n")
    );
    let err = parse_for(RoleKind::Hand, &text).unwrap_err();
    assert!(matches!(err, EnvelopeError::Toml { .. }), "{err:?}");
}
