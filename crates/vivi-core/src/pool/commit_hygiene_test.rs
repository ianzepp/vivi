use super::*;

#[test]
fn unit_tags_are_stripped_from_the_subject() {
    assert_eq!(commit_subject("[3ea8dee9-1 a1] Retire X", None), "Retire X");
}

#[test]
fn pump_job_tags_are_stripped_from_the_subject() {
    assert_eq!(
        commit_subject("[pump-job:need-init-x] initialize a need branch", None),
        "initialize a need branch"
    );
}

#[test]
fn stacked_tags_are_all_stripped() {
    assert_eq!(
        commit_subject("[a1] [b2]  Fix it\nsecond line", None),
        "Fix it"
    );
}

#[test]
fn a_plain_subject_is_kept() {
    assert_eq!(commit_subject("Fix the thing", None), "Fix the thing");
    assert_eq!(commit_subject("Fix [the] thing", None), "Fix [the] thing");
}

#[test]
fn an_unclosed_tag_is_kept() {
    assert_eq!(commit_subject("[oops Fix it", None), "[oops Fix it");
}

#[test]
fn a_subject_of_only_tags_falls_back() {
    assert_eq!(commit_subject("[3ea8dee9-1 a1]", None), FALLBACK_SUBJECT);
    assert_eq!(commit_subject("", None), FALLBACK_SUBJECT);
}

#[test]
fn the_pump_run_suffix_is_stripped_when_the_need_is_known() {
    assert_eq!(
        commit_subject("Give the tick a clock (4c01ea79 r1)", Some("4c01ea79")),
        "Give the tick a clock"
    );
    assert_eq!(
        commit_subject("[pump-job:x] Add a test (4c01ea79 r12)", Some("4c01ea79")),
        "Add a test"
    );
}

#[test]
fn only_the_pump_run_suffix_of_this_need_is_stripped() {
    let kept = "Give the tick a clock (4c01ea79 r1)";
    assert_eq!(commit_subject(kept, None), kept);
    assert_eq!(commit_subject(kept, Some("deadbeef")), kept);
    assert_eq!(
        commit_subject("Fix (see r2)", Some("4c01ea79")),
        "Fix (see r2)"
    );
    assert_eq!(
        commit_subject("Note (4c01ea79 rX)", Some("4c01ea79")),
        "Note (4c01ea79 rX)"
    );
}

#[test]
fn a_subject_that_is_only_the_run_suffix_falls_back() {
    assert_eq!(
        commit_subject("[a1] (4c01ea79 r1)", Some("4c01ea79")),
        FALLBACK_SUBJECT
    );
}

#[test]
fn unit_ids_come_out_of_unit_branch_names() {
    assert_eq!(unit_id("vivi/unit/3ea8dee9-1-a2"), Some("3ea8dee9-1"));
    assert_eq!(unit_id("vivi/unit/3ea8dee9-a10"), Some("3ea8dee9"));
    assert_eq!(unit_id("vivi/need/3ea8dee9"), None);
    assert_eq!(unit_id("vivi/unit/3ea8dee9-1"), None);
    assert_eq!(unit_id("vivi/unit/-a1"), None);
}
