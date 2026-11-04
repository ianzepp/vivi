//! Tests for the pump's mapping of flow classes onto role reply schemas.

use super::{FlowKind, NodeClass};
use vivi_mail::letter::records::RecordKind;
use vivi_mail::letter::review::{
    ADMITTER_ANSWERS, AUDITOR_ANSWERS, CTO_ANSWERS, JUDGE_ANSWERS, MERGE_ANSWERS,
    RESEARCHER_ANSWERS,
};
use vivi_mail::letter::roles::{RoleKind, parse_for};

fn env(front: &str) -> String {
    format!("+++\n{front}+++\nreport\n")
}

fn answer(kind: RoleKind, front: &str) -> String {
    parse_for(kind, &env(front)).unwrap().answer
}

fn is_err(kind: RoleKind, front: &str) -> bool {
    parse_for(kind, &env(front)).is_err()
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
        _ => {}
    }
    front
}

#[test]
fn role_kind_maps_role_nodes_only() {
    assert_eq!(NodeClass::Slicer.role_kind(), Some(RoleKind::Slicer));
    assert_eq!(NodeClass::Cto.role_kind(), Some(RoleKind::Cto));
    assert_eq!(NodeClass::Merge.role_kind(), Some(RoleKind::Merge));
    assert_eq!(NodeClass::Repairer.role_kind(), Some(RoleKind::Repairer));
    assert_eq!(NodeClass::RungNeed.role_kind(), None);
    assert_eq!(NodeClass::Fanout.role_kind(), None);
    assert_eq!(NodeClass::Decision.role_kind(), None);
}

#[test]
fn record_kind_maps_to_flow_kind() {
    assert_eq!(FlowKind::from(RecordKind::Build), FlowKind::Build);
    assert_eq!(FlowKind::from(RecordKind::Research), FlowKind::Research);
    assert_eq!(FlowKind::from(RecordKind::Release), FlowKind::Release);
    assert_eq!(FlowKind::from(RecordKind::Repair), FlowKind::Repair);
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
        let kind = class.role_kind().expect("role class");
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
