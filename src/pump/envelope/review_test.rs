//! Tests for the review-side role envelopes.

use super::{
    ADMITTER_ANSWERS, AUDITOR_ANSWERS, CTO_ANSWERS, HEAD_CEO_ANSWERS, JUDGE_ANSWERS, MERGE_ANSWERS,
    RESEARCHER_ANSWERS,
};
use crate::pump::envelope::EnvelopeError;
use crate::pump::envelope::roles::{RoleKind, parse_for};
use crate::pump::flow::def::NodeClass;

fn env(front: &str) -> String {
    format!("+++\n{front}+++\nreport\n")
}

fn answer(kind: RoleKind, front: &str) -> String {
    parse_for(kind, &env(front)).unwrap().answer
}

fn is_err(kind: RoleKind, front: &str) -> bool {
    parse_for(kind, &env(front)).is_err()
}

const ROLES: [RoleKind; 10] = [
    RoleKind::Researcher,
    RoleKind::Planner,
    RoleKind::Admitter,
    RoleKind::Slicer,
    RoleKind::Auditor,
    RoleKind::Judge,
    RoleKind::Cto,
    RoleKind::Merge,
    RoleKind::Hand,
    RoleKind::HeadCeo,
];

#[test]
fn a_valid_researcher_envelope_parses() {
    let front = "answer = \"NEEDS FURTHER REVIEW\"\nopen_questions = ['Per step or per run?']\n";
    let parsed = parse_for(RoleKind::Researcher, &env(front)).unwrap();
    assert_eq!(parsed.answer, "NEEDS FURTHER REVIEW");
    assert_eq!(parsed.front["open_questions"][0], "Per step or per run?");
    assert_eq!(parsed.body, "report");
    assert_eq!(
        answer(RoleKind::Researcher, "answer = \"NOT READY\"\n"),
        "NOT READY"
    );
    assert!(is_err(RoleKind::Researcher, "answer = \"ready\"\n"));
}

#[test]
fn admitter_conditional_rules() {
    let revise = "answer = \"revise\"\nrequired_changes = ['name the file']\nfindings = ['dup']\n";
    let parsed = parse_for(RoleKind::Admitter, &env(revise)).unwrap();
    assert_eq!(parsed.front["required_changes"][0], "name the file");
    assert!(is_err(RoleKind::Admitter, "answer = \"revise\"\n"));
    assert!(is_err(RoleKind::Admitter, "answer = \"escalate\"\n"));
    assert!(is_err(
        RoleKind::Admitter,
        "answer = \"escalate\"\nreason = '  '\n"
    ));
    assert_eq!(
        answer(
            RoleKind::Admitter,
            "answer = \"escalate\"\nreason = 'needs a design'\n"
        ),
        "escalate"
    );
    assert_eq!(
        answer(RoleKind::Admitter, "answer = \"admitted\"\n"),
        "admitted"
    );
}

const AUDIT: &str = "answer = \"residual\"\nraise_rung = \"main\"\n\n\
    [[requirement]]\nid = \"R1\"\ntext = 'A closed run stops.'\nstatus = \"NOT_APPLICABLE\"\n\
    evidence = [\"src/run.rs:210\"]\n\n\
    [[finding]]\nid = \"F1\"\ncontent = 'Counted twice.'\npaths = [\"src/board.rs\"]\n\
    recheck_trigger = 'Any change to the count query.'\n";

#[test]
fn a_valid_auditor_envelope_parses() {
    let parsed = parse_for(RoleKind::Auditor, &env(AUDIT)).unwrap();
    assert_eq!(parsed.answer, "residual");
    assert_eq!(parsed.front["raise_rung"], "main");
    assert_eq!(parsed.front["requirement"][0]["status"], "NOT_APPLICABLE");
    assert_eq!(parsed.front["finding"][0]["paths"][0], "src/board.rs");
    assert_eq!(
        answer(RoleKind::Auditor, "answer = \"clean_pass\"\n"),
        "clean_pass"
    );
}

#[test]
fn auditor_refuses_bad_rows() {
    assert!(is_err(
        RoleKind::Auditor,
        "answer = \"residual\"\nraise_rung = \"all\"\n"
    ));
    let bad_status = AUDIT.replace("NOT_APPLICABLE", "DONE");
    assert!(is_err(RoleKind::Auditor, &bad_status));
    let no_trigger = "answer = \"residual\"\n[[finding]]\nid = \"F1\"\ncontent = 'x'\n";
    assert!(is_err(RoleKind::Auditor, no_trigger));
    let extra = format!(
        "{AUDIT}[[requirement]]\nid = \"R2\"\ntext = 'x'\nstatus = \"MISSING\"\nproof = 1\n"
    );
    assert!(is_err(RoleKind::Auditor, &extra));
}

#[test]
fn judge_and_cto_require_a_reason() {
    assert_eq!(
        answer(
            RoleKind::Judge,
            "answer = \"block\"\nreason = 'guard dropped'\n"
        ),
        "block"
    );
    assert!(is_err(RoleKind::Judge, "answer = \"block\"\n"));
    assert!(is_err(
        RoleKind::Judge,
        "answer = \"residual\"\nreason = ''\n"
    ));

    let rework = "answer = \"rework\"\nreason = 'guard missing'\nnew_fact = 'traced to slice 3'\n";
    let parsed = parse_for(RoleKind::Cto, &env(rework)).unwrap();
    assert_eq!(parsed.front["new_fact"], "traced to slice 3");
    assert_eq!(
        answer(RoleKind::Cto, "answer = \"lock\"\nreason = 'done'\n"),
        "lock"
    );
    assert!(is_err(
        RoleKind::Cto,
        "answer = \"repair\"\nreason = 'x'\nnew_fact = 'y'\n"
    ));
    assert!(is_err(RoleKind::Cto, "answer = \"lock\"\n"));
    assert!(is_err(RoleKind::Cto, "answer = \"done\"\nreason = 'x'\n"));
}

#[test]
fn a_merge_envelope_carries_judgment_only() {
    let front = "answer = \"conflict\"\nconflicts = [\"src/tick.rs\"]\ndesign_change = true\n\n\
        [[check]]\ncmd = \"cargo check\"\nresult = \"pass\"\nseconds = 41\n";
    let parsed = parse_for(RoleKind::Merge, &env(front)).unwrap();
    assert_eq!(parsed.answer, "conflict");
    assert_eq!(parsed.front["conflicts"][0], "src/tick.rs");
    assert_eq!(parsed.front["design_change"], true);
    assert_eq!(parsed.front["check"][0]["seconds"], 41);

    let plain = parse_for(RoleKind::Merge, &env("answer = \"merged\"\n")).unwrap();
    assert_eq!(plain.front["design_change"], false);
    assert_eq!(plain.front["conflicts"], serde_json::json!([]));
    assert!(is_err(RoleKind::Merge, "answer = \"failed\"\n"));
    assert!(is_err(
        RoleKind::Merge,
        "answer = \"merged\"\n[[check]]\ncmd = \"c\"\nresult = \"pass\"\n"
    ));
}

#[test]
fn a_head_ceo_envelope_parses() {
    let front =
        "answer = \"reviewed\"\npromoted = [\"need-12\"]\nresearch = []\ndropped = [\"want-3\"]\n";
    let parsed = parse_for(RoleKind::HeadCeo, &env(front)).unwrap();
    assert_eq!(parsed.front["promoted"][0], "need-12");
    assert_eq!(parsed.front["dropped"][0], "want-3");
    assert_eq!(answer(RoleKind::HeadCeo, "answer = \"no-op\"\n"), "no-op");
    assert!(is_err(RoleKind::HeadCeo, "answer = \"noop\"\n"));
}

/// A minimal valid front matter for any word of any role.
fn minimal(kind: RoleKind, word: &str) -> String {
    let mut front = format!("answer = \"{word}\"\n");
    match (kind, word) {
        (RoleKind::Planner, w) if w != "planned" => front.push_str("reason = 'r'\n"),
        (RoleKind::Admitter, "revise") => front.push_str("required_changes = ['c']\n"),
        (RoleKind::Admitter, "escalate") => front.push_str("reason = 'r'\n"),
        (RoleKind::Judge | RoleKind::Cto, _) => front.push_str("reason = 'r'\n"),
        (RoleKind::Slicer, "escalate") => front.push_str("reason = 'r'\n"),
        (RoleKind::Slicer, _) => {
            front.push_str("[[slice]]\nid = \"S1\"\ntitle = 't'\nbrief = 'b'\ntarget = \"app\"\n")
        }
        (RoleKind::Hand, "blocked-scope") => {
            front.push_str("scope_paths = [\"src/a.rs\"]\nscope_reason = 'r'\n");
        }
        _ => {}
    }
    front
}

#[test]
fn every_flow_class_answer_set_matches_the_parser() {
    let classes = [
        NodeClass::Researcher,
        NodeClass::Planner,
        NodeClass::Admitter,
        NodeClass::Slicer,
        NodeClass::Auditor,
        NodeClass::Judge,
        NodeClass::Cto,
        NodeClass::Merge,
    ];
    for class in classes {
        let kind = RoleKind::from_class(&class).expect("role class");
        let mut words: Vec<&str> = class.words().to_vec();
        words.retain(|w| *w != "failed");
        for word in &words {
            assert_eq!(answer(kind, &minimal(kind, word)), *word, "{kind:?} {word}");
        }
        assert!(
            is_err(kind, &minimal(kind, "failed")),
            "{kind:?} takes no failed"
        );
        assert!(
            is_err(kind, &minimal(kind, "bogus")),
            "{kind:?} takes no bogus"
        );
    }
}

#[test]
fn answer_constants_equal_the_flow_words_without_failed() {
    let cases: [(NodeClass, &[&str]); 6] = [
        (NodeClass::Researcher, RESEARCHER_ANSWERS),
        (NodeClass::Admitter, ADMITTER_ANSWERS),
        (NodeClass::Auditor, AUDITOR_ANSWERS),
        (NodeClass::Judge, JUDGE_ANSWERS),
        (NodeClass::Cto, CTO_ANSWERS),
        (NodeClass::Merge, MERGE_ANSWERS),
    ];
    for (class, constant) in cases {
        let flow: Vec<&str> = class
            .words()
            .iter()
            .copied()
            .filter(|w| *w != "failed")
            .collect();
        assert_eq!(flow, constant, "{class:?}");
    }
}

#[test]
fn answer_constants_are_exactly_what_each_parser_accepts() {
    let cases: [(RoleKind, &[&str]); 7] = [
        (RoleKind::Researcher, RESEARCHER_ANSWERS),
        (RoleKind::Admitter, ADMITTER_ANSWERS),
        (RoleKind::Auditor, AUDITOR_ANSWERS),
        (RoleKind::Judge, JUDGE_ANSWERS),
        (RoleKind::Cto, CTO_ANSWERS),
        (RoleKind::Merge, MERGE_ANSWERS),
        (RoleKind::HeadCeo, HEAD_CEO_ANSWERS),
    ];
    let every_word: Vec<&str> = cases
        .iter()
        .flat_map(|(_, words)| words.iter().copied())
        .collect();
    for (kind, words) in cases {
        for word in every_word.iter().chain(&["failed"]) {
            let accepted = parse_for(kind, &env(&minimal(kind, word))).is_ok();
            assert_eq!(accepted, words.contains(word), "{kind:?} {word}");
        }
    }
}

#[test]
fn mechanical_keys_are_refused_on_every_review_role() {
    let review = [
        RoleKind::Researcher,
        RoleKind::Admitter,
        RoleKind::Auditor,
        RoleKind::Judge,
        RoleKind::Cto,
        RoleKind::Merge,
        RoleKind::HeadCeo,
    ];
    let word_of = |kind: RoleKind| match kind {
        RoleKind::Researcher => "READY",
        RoleKind::Admitter => "admitted",
        RoleKind::Auditor => "clean_pass",
        RoleKind::Judge => "block",
        RoleKind::Cto => "lock",
        RoleKind::Merge => "merged",
        _ => "reviewed",
    };
    for kind in review {
        let base = minimal(kind, word_of(kind));
        assert!(parse_for(kind, &env(&base)).is_ok(), "{kind:?} base");
        for key in ["tip", "sha", "commit", "published", "exit", "proof"] {
            let with_key = format!("{base}{key} = \"x\"\n");
            let err = parse_for(kind, &env(&with_key)).unwrap_err();
            assert!(
                matches!(err, EnvelopeError::Toml { .. }),
                "{kind:?} {key}: {err:?}"
            );
        }
    }
}

#[test]
fn mechanical_keys_are_refused_on_the_other_roles_too() {
    for kind in ROLES {
        for key in ["tip", "sha", "commit", "published", "exit"] {
            let base = match kind {
                RoleKind::Planner => "planned",
                RoleKind::Slicer => "escalate",
                RoleKind::Hand => "built",
                RoleKind::Researcher => "READY",
                RoleKind::Admitter => "admitted",
                RoleKind::Auditor => "clean_pass",
                RoleKind::Judge => "block",
                RoleKind::Cto => "lock",
                RoleKind::Merge => "merged",
                RoleKind::HeadCeo => "reviewed",
            };
            let front = format!("{}{key} = \"x\"\n", minimal(kind, base));
            assert!(is_err(kind, &front), "{kind:?} {key}");
        }
    }
}
