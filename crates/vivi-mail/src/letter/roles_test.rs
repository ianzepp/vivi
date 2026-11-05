//! Tests for the typed role envelopes.

use super::{RoleKind, parse_for};
use crate::letter::LetterError as EnvelopeError;

#[test]
fn structured_fields_validate_each_role_and_keep_body_separate() {
    let fixtures = [
        (RoleKind::Hand, serde_json::json!({"answer":"built"})),
        (RoleKind::Repairer, serde_json::json!({"answer":"built"})),
        (
            RoleKind::Slicer,
            serde_json::json!({"answer":"escalate","reason":"large"}),
        ),
        (RoleKind::Planner, serde_json::json!({"answer":"planned"})),
        (RoleKind::Researcher, serde_json::json!({"answer":"READY"})),
        (
            RoleKind::Admitter,
            serde_json::json!({"answer":"revise","required_changes":["fix"]}),
        ),
        (
            RoleKind::Auditor,
            serde_json::json!({"answer":"clean_pass"}),
        ),
        (
            RoleKind::Judge,
            serde_json::json!({"answer":"block","reason":"evidence"}),
        ),
        (
            RoleKind::Cto,
            serde_json::json!({"answer":"lock","reason":"done"}),
        ),
        (RoleKind::Merge, serde_json::json!({"answer":"merged"})),
        (RoleKind::HeadCeo, serde_json::json!({"answer":"no-op"})),
    ];
    for (kind, fields) in fixtures {
        let structured = super::parse_fields(kind, &fields, "body prose").unwrap();
        let printed = parse_for(kind, &hand(&toml::to_string(&fields).unwrap())).unwrap();
        assert_eq!(structured.answer, printed.answer, "{kind:?}");
        assert_eq!(structured.front, printed.front, "{kind:?}");
        assert_eq!(structured.body, "body prose");
    }
}

#[test]
fn structured_fields_reject_unknown_nested_fields_and_share_validation() {
    let unknown = serde_json::json!({"answer":"sliced", "slice":[{"id":"S1","title":"t","brief":"b","target":"x","proof":1}]});
    let structured = super::parse_fields(RoleKind::Slicer, &unknown, "").unwrap_err();
    let printed = parse_for(
        RoleKind::Slicer,
        &hand("answer = 'sliced'\n[[slice]]\nid='S1'\ntitle='t'\nbrief='b'\ntarget='x'\nproof=1\n"),
    )
    .unwrap_err();
    assert!(matches!(structured, EnvelopeError::Invalid(_)));
    assert!(matches!(printed, EnvelopeError::Toml { .. }));
    let bad = super::parse_fields(
        RoleKind::Hand,
        &serde_json::json!({"answer":"blocked-scope"}),
        "",
    )
    .unwrap_err();
    let printed = parse_for(RoleKind::Hand, &hand("answer = 'blocked-scope'\n")).unwrap_err();
    assert_eq!(bad.to_string(), printed.to_string());
    let non_object = super::parse_fields(RoleKind::Hand, &serde_json::json!([]), "").unwrap_err();
    assert!(matches!(non_object, EnvelopeError::Invalid(_)));
}

#[test]
fn role_kind_has_stable_kebab_case_spelling() {
    assert_eq!(
        serde_json::to_string(&RoleKind::HeadCeo).unwrap(),
        "\"head-ceo\""
    );
    assert_eq!(
        serde_json::from_str::<RoleKind>("\"head-ceo\"").unwrap(),
        RoleKind::HeadCeo
    );
}

fn hand(front: &str) -> String {
    format!("+++\n{front}+++\nreport\n")
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
    assert_eq!(parsed.front["check"][0]["seconds"], 94.0);
    assert_eq!(parsed.front["check"][0]["result"], "pass");
    assert_eq!(parsed.front["follow_up"][0]["need"], "fix b");
}

#[test]
fn check_seconds_accept_integer_and_decimal_numbers() {
    let check_row = |seconds: &str| {
        format!(
            "answer = \"built\"\n\n[[check]]\ncmd = \"cargo test\"\nresult = \"pass\"\
             \nseconds = {seconds}\n"
        )
    };
    for (front, expected) in [
        (check_row("0"), 0.0),
        (check_row("94"), 94.0),
        (check_row("0.0"), 0.0),
        (check_row("12.5"), 12.5),
        (check_row("1.25e1"), 12.5),
        (check_row("-0.0"), 0.0),
    ] {
        let parsed = parse_for(RoleKind::Hand, &hand(&front)).unwrap();
        assert_eq!(parsed.answer, "built");
        assert_eq!(parsed.body, "report");
        assert_eq!(parsed.front["check"][0]["seconds"].as_f64(), Some(expected));
    }

    let merge = check_row("12.5").replace("built", "merged");
    let parsed = parse_for(RoleKind::Merge, &hand(&merge)).unwrap();
    assert_eq!(parsed.answer, "merged");
    assert_eq!(parsed.body, "report");
    assert_eq!(parsed.front["check"][0]["seconds"].as_f64(), Some(12.5));

    let repairer = check_row("12").replace("built", "failed");
    let parsed = parse_for(RoleKind::Repairer, &hand(&repairer)).unwrap();
    assert_eq!(parsed.answer, "failed");
    assert_eq!(parsed.body, "report");
    assert_eq!(parsed.front["check"][0]["seconds"].as_f64(), Some(12.0));
}

/// Hand, Repairer and Merge all carry `[[check]]` rows, so each role's parser
/// must refuse the same bad durations. The check row starts on reply line 4
/// and its `seconds` line lands on reply line 7.
fn a_check_row_per_role() -> impl Iterator<Item = (RoleKind, &'static str)> {
    [
        (RoleKind::Hand, "built"),
        (RoleKind::Repairer, "built"),
        (RoleKind::Merge, "merged"),
    ]
    .into_iter()
}

#[test]
fn check_seconds_refuse_negative_and_non_numeric_values() {
    for (kind, answer) in a_check_row_per_role() {
        let row = |seconds: &str| {
            hand(&format!(
                "answer = \"{answer}\"\n\n[[check]]\ncmd = \"cargo test\"\n\
                 result = \"pass\"\nseconds = {seconds}\n"
            ))
        };
        for seconds in [
            "-1",
            "-1.5",
            "\"12\"",
            "\"fast\"",
            "true",
            "[12]",
            "{ value = 12 }",
        ] {
            let err = parse_for(kind, &row(seconds)).unwrap_err();
            assert!(
                matches!(err, EnvelopeError::Toml { line: 7, .. }),
                "{kind:?} seconds = {seconds}: {err:?}"
            );
        }

        let missing = hand(&format!(
            "answer = \"{answer}\"\n\n[[check]]\ncmd = \"cargo test\"\nresult = \"pass\"\n"
        ));
        let err = parse_for(kind, &missing).unwrap_err();
        assert!(
            matches!(err, EnvelopeError::Toml { .. }),
            "{kind:?}: {err:?}"
        );
    }
}

#[test]
fn check_seconds_refuse_non_finite_numbers() {
    for (kind, answer) in a_check_row_per_role() {
        let row = |seconds: &str| {
            hand(&format!(
                "answer = \"{answer}\"\n\n[[check]]\ncmd = \"cargo test\"\n\
                 result = \"pass\"\nseconds = {seconds}\n"
            ))
        };
        for seconds in ["nan", "inf", "-inf"] {
            let err = parse_for(kind, &row(seconds)).unwrap_err();
            assert!(
                matches!(err, EnvelopeError::Toml { line: 7, .. }),
                "{kind:?} seconds = {seconds}: {err:?}"
            );
        }
    }
}

#[test]
fn decimal_check_seconds_survive_serialization() {
    let text = hand(
        "answer = \"built\"\n\n[[check]]\ncmd = \"cargo test\"\nresult = \"pass\"\
         \nseconds = 12.5\n\n[[check]]\ncmd = \"cargo doc\"\nresult = \"skipped\"\
         \nseconds = 1.0e20\n",
    );
    let envelope = super::parse_role_reply::<super::Hand>(&text).unwrap();
    assert_eq!(envelope.front.check[0].seconds, 12.5);
    assert_eq!(envelope.front.check[1].seconds, 1.0e20);

    let json = serde_json::to_value(&envelope.front).unwrap();
    let round_tripped: super::Hand = serde_json::from_value(json).unwrap();
    assert_eq!(round_tripped.check[0].seconds, 12.5);
    assert_eq!(round_tripped.check[1].seconds, 1.0e20);

    let rendered = crate::letter::render(&envelope.front, "fractional receipt").unwrap();
    let reparsed = super::parse_role_reply::<super::Hand>(&rendered).unwrap();
    assert_eq!(reparsed.front.check[0].seconds, 12.5);
    assert_eq!(reparsed.front.check[1].seconds, 1.0e20);
    assert_eq!(reparsed.body, "fractional receipt");
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
fn a_valid_repairer_envelope_parses() {
    let parsed = parse_for(RoleKind::Repairer, &hand(REPAIRER_BUILT)).unwrap();
    assert_eq!(parsed.answer, "built");
    assert_eq!(parsed.front["change"][0], "src/a.rs");
    assert_eq!(parsed.front["check"][0]["seconds"].as_f64(), Some(12.0));
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
