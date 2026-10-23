use super::{
    ADMITTER_ROLE, HEAD_CTO_ROLE, PUMP_PLANNER_ROLE, REPAIRER_ROLE, RESEARCHER_ROLE, SLICER_ROLE,
    pre_v20_charter, shipped_charter,
};

#[test]
fn every_shipped_charter_has_the_v20_envelope() {
    for role in [
        PUMP_PLANNER_ROLE,
        "merge",
        "auditor",
        RESEARCHER_ROLE,
        ADMITTER_ROLE,
        SLICER_ROLE,
        REPAIRER_ROLE,
        HEAD_CTO_ROLE,
    ] {
        let text = shipped_charter(role).unwrap();
        assert!(!pre_v20_charter(text), "{role} charter flagged");
    }
}

#[test]
fn a_blank_charter_is_not_flagged() {
    assert!(!pre_v20_charter(""));
    assert!(!pre_v20_charter("  \n\t\n"));
}

#[test]
fn a_plan_marker_charter_is_flagged() {
    assert!(pre_v20_charter(
        "+++\nx\n+++\nAnswer with PUMP-PLAN-BEGIN {json} PUMP-PLAN-END"
    ));
}

#[test]
fn a_charter_without_a_front_matter_line_is_flagged() {
    assert!(pre_v20_charter("You are the planner. Reply in prose."));
}
