//! Tests for flow class parsing and per-class answer words.

use super::{FAILED, FlowKind, NodeClass};

const STEP_CLASSES: [&str; 15] = [
    "researcher",
    "planner",
    "admitter",
    "slicer",
    "auditor",
    "judge",
    "cto",
    "merge",
    "merge-main",
    "fanout",
    "repairer",
    "rung-need",
    "rung-main",
    "rung-release",
    "rung-raised",
];

#[test]
fn every_step_class_parses_and_answers_failed() {
    for name in STEP_CLASSES {
        let class = NodeClass::parse(name).unwrap_or_else(|| panic!("{name} parses"));
        assert!(class.is_step(), "{name} is a step");
        assert!(class.words().contains(&FAILED), "{name} lacks failed");
    }
}

#[test]
fn non_step_classes_parse() {
    for name in [
        "join",
        "raisecheck",
        "milestone",
        "park",
        "lock",
        "decision",
        "done",
    ] {
        let class = NodeClass::parse(name).unwrap_or_else(|| panic!("{name} parses"));
        assert!(!class.is_step(), "{name} is not a step");
    }
}

#[test]
fn restart_carries_its_entry() {
    assert_eq!(
        NodeClass::parse("restart-plan"),
        Some(NodeClass::Restart("plan".to_string()))
    );
    assert_eq!(NodeClass::parse("restart-"), None);
    assert_eq!(NodeClass::parse("bogus"), None);
}

#[test]
fn join_and_raisecheck_words_follow_the_table() {
    assert_eq!(
        NodeClass::Join.words(),
        &["verified", "raised", "block", "judge", FAILED]
    );
    assert_eq!(NodeClass::Raisecheck.words(), &["raised", "none"]);
}

#[test]
fn repairer_words_include_overtime() {
    assert_eq!(
        NodeClass::Repairer.words(),
        &["built", "overtime", "unchanged", FAILED]
    );
}

#[test]
fn flow_kind_round_trips() {
    for kind in [
        FlowKind::Build,
        FlowKind::Research,
        FlowKind::Release,
        FlowKind::Repair,
    ] {
        assert_eq!(FlowKind::parse(kind.name()), Some(kind));
    }
    assert_eq!(FlowKind::parse("nope"), None);
}

#[test]
fn class_name_is_the_inverse_of_parse() {
    for name in STEP_CLASSES {
        assert_eq!(NodeClass::parse(name).unwrap().name(), name);
    }
    for name in [
        "join",
        "raisecheck",
        "milestone",
        "park",
        "lock",
        "decision",
        "done",
        "restart-plan",
        "restart-rework1",
    ] {
        assert_eq!(NodeClass::parse(name).unwrap().name(), name);
    }
}
