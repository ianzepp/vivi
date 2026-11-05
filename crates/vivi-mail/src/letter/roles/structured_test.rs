//! Parity tests for fence-free role submissions.

use super::{RoleKind, parse_for, parse_structured_for};
use crate::letter::LetterError;
use serde_json::{Value, json};

fn structured_parity(kind: RoleKind, front: &str, fields: Value) {
    let text = format!("+++\n{front}+++\nreport\n");
    let fenced = parse_for(kind, &text).unwrap();
    let structured = parse_structured_for(kind, fields, "report").unwrap();
    assert_eq!(structured, fenced, "{}", kind.name());
}

#[test]
fn every_role_has_fence_free_parity_with_its_toml_vocabulary() {
    let cases = [
        (
            RoleKind::Researcher,
            "answer = \"READY\"\nopen_questions = [\"q\"]\n",
            json!({"answer":"READY", "open_questions":["q"]}),
        ),
        (
            RoleKind::Planner,
            "answer = \"planned\"\n",
            json!({"answer":"planned"}),
        ),
        (
            RoleKind::Admitter,
            "answer = \"admitted\"\nfindings = [\"ok\"]\n",
            json!({"answer":"admitted", "findings":["ok"]}),
        ),
        (
            RoleKind::Slicer,
            "answer = \"sliced\"\n\n[[slice]]\nid = \"S1\"\ntitle = \"t\"\nbrief = \"b\"\ntarget = \"src\"\n",
            json!({"answer":"sliced", "slice":[{"id":"S1", "title":"t", "brief":"b", "target":"src"}]}),
        ),
        (
            RoleKind::Auditor,
            "answer = \"clean_pass\"\n\n[[requirement]]\nid = \"R1\"\ntext = \"req\"\nstatus = \"SATISFIED\"\nevidence = [\"test\"]\n",
            json!({"answer":"clean_pass", "requirement":[{"id":"R1", "text":"req", "status":"SATISFIED", "evidence":["test"]}]}),
        ),
        (
            RoleKind::Judge,
            "answer = \"block\"\nreason = \"reason\"\n",
            json!({"answer":"block", "reason":"reason"}),
        ),
        (
            RoleKind::Cto,
            "answer = \"plan\"\nreason = \"reason\"\nnew_fact = \"fact\"\n",
            json!({"answer":"plan", "reason":"reason", "new_fact":"fact"}),
        ),
        (
            RoleKind::Merge,
            "answer = \"merged\"\nconflicts = []\ndesign_change = false\n\n[[check]]\ncmd = \"cargo test\"\nresult = \"pass\"\nseconds = 1.5\n",
            json!({"answer":"merged", "conflicts":[], "design_change":false, "check":[{"cmd":"cargo test", "result":"pass", "seconds":1.5}]}),
        ),
        (
            RoleKind::Hand,
            "answer = \"built\"\nchange = [\"src/a.rs\"]\n\n[[check]]\ncmd = \"cargo test\"\nresult = \"pass\"\nseconds = 2\n",
            json!({"answer":"built", "change":["src/a.rs"], "check":[{"cmd":"cargo test", "result":"pass", "seconds":2}]}),
        ),
        (
            RoleKind::Repairer,
            "answer = \"built\"\nchange = [\"src/a.rs\"]\n",
            json!({"answer":"built", "change":["src/a.rs"]}),
        ),
        (
            RoleKind::HeadCeo,
            "answer = \"reviewed\"\nserved = [\"N1\"]\nresearch = []\ndropped = []\n",
            json!({"answer":"reviewed", "served":["N1"], "research":[], "dropped":[]}),
        ),
    ];
    assert_eq!(
        cases.iter().map(|(kind, _, _)| *kind).collect::<Vec<_>>(),
        RoleKind::ALL.to_vec()
    );
    for (kind, front, fields) in cases {
        structured_parity(kind, front, fields);
    }
}

#[test]
fn structured_role_results_are_serializable_for_submission_storage() {
    let parsed =
        parse_structured_for(RoleKind::HeadCeo, json!({"answer":"no-op"}), "note").unwrap();
    let stored = serde_json::to_value(&parsed).unwrap();
    assert_eq!(
        serde_json::from_value::<super::Parsed>(stored).unwrap(),
        parsed
    );
}

#[test]
fn nested_unknown_fields_are_refused_by_structured_deserialization() {
    let hand_fields = json!({
        "answer":"built",
        "check":[{"cmd":"cargo test", "result":"pass", "seconds":1, "unknown":true}]
    });
    assert!(parse_structured_for(RoleKind::Hand, hand_fields, "body").is_err());

    let slicer_fields = json!({
        "answer":"sliced",
        "slice":[{"id":"S1", "title":"t", "brief":"b", "target":"src", "extra":true}]
    });
    assert!(parse_structured_for(RoleKind::Slicer, slicer_fields, "body").is_err());
}

#[test]
fn structured_validation_reuses_role_diagnostics() {
    let cases = [
        (
            RoleKind::Hand,
            "answer = \"blocked-scope\"\nscope_reason = \"needs a re-export\"\n",
            json!({"answer":"blocked-scope", "scope_reason":"needs a re-export"}),
        ),
        (
            RoleKind::Admitter,
            "answer = \"revise\"\n",
            json!({"answer":"revise"}),
        ),
        (
            RoleKind::Judge,
            "answer = \"block\"\n",
            json!({"answer":"block"}),
        ),
    ];
    for (kind, front, fields) in cases {
        let fenced = parse_for(kind, &format!("+++\n{front}+++\nbody")).unwrap_err();
        let structured = parse_structured_for(kind, fields, "body").unwrap_err();
        assert_eq!(structured, fenced, "{} diagnostic", kind.name());
        assert_eq!(structured.to_string(), fenced.to_string());
    }
}

#[test]
fn structured_auditor_rejects_an_unknown_requirement_status() {
    let fields = json!({"answer":"clean_pass", "requirement":[{
        "id":"R1", "text":"req", "status":"UNKNOWN", "evidence":[]
    }]});
    assert!(parse_structured_for(RoleKind::Auditor, fields, "body").is_err());
    let text = "+++\nanswer = \"clean_pass\"\n\n[[requirement]]\nid = \"R1\"\ntext = \"req\"\nstatus = \"UNKNOWN\"\n+++\nbody";
    assert!(parse_for(RoleKind::Auditor, text).is_err());
}

#[test]
fn structured_check_seconds_must_be_finite_and_non_negative() {
    for value in [json!(-1), json!("1"), json!(true)] {
        let fields = json!({"answer":"built", "check":[{
            "cmd":"cargo test", "result":"pass", "seconds":value
        }]});
        assert!(parse_structured_for(RoleKind::Hand, fields, "body").is_err());
    }
    for seconds in ["-1", "\"1\"", "true"] {
        let text = format!(
            "+++\nanswer = \"built\"\n\n[[check]]\ncmd = \"cargo test\"\nresult = \"pass\"\nseconds = {seconds}\n+++\nbody"
        );
        assert!(parse_for(RoleKind::Hand, &text).is_err());
    }
}

#[test]
fn structured_fields_report_normal_shared_validation_errors() {
    let got = parse_structured_for(RoleKind::Judge, json!({"answer":"block"}), "").unwrap_err();
    assert!(matches!(got, LetterError::Invalid(_)));
}
