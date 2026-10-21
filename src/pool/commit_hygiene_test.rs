use super::*;

#[test]
fn unit_tags_are_stripped_from_the_subject() {
    assert_eq!(commit_subject("[3ea8dee9-1 a1] Retire X"), "Retire X");
}

#[test]
fn pump_job_tags_are_stripped_from_the_subject() {
    assert_eq!(
        commit_subject("[pump-job:need-init-x] initialize a need branch"),
        "initialize a need branch"
    );
}

#[test]
fn stacked_tags_are_all_stripped() {
    assert_eq!(commit_subject("[a1] [b2]  Fix it\nsecond line"), "Fix it");
}

#[test]
fn a_plain_subject_is_kept() {
    assert_eq!(commit_subject("Fix the thing"), "Fix the thing");
    assert_eq!(commit_subject("Fix [the] thing"), "Fix [the] thing");
}

#[test]
fn an_unclosed_tag_is_kept() {
    assert_eq!(commit_subject("[oops Fix it"), "[oops Fix it");
}

#[test]
fn a_subject_of_only_tags_falls_back() {
    assert_eq!(commit_subject("[3ea8dee9-1 a1]"), FALLBACK_SUBJECT);
    assert_eq!(commit_subject(""), FALLBACK_SUBJECT);
}

#[test]
fn unit_ids_come_out_of_unit_branch_names() {
    assert_eq!(unit_id("vivi/unit/3ea8dee9-1-a2"), Some("3ea8dee9-1"));
    assert_eq!(unit_id("vivi/unit/3ea8dee9-a10"), Some("3ea8dee9"));
    assert_eq!(unit_id("vivi/need/3ea8dee9"), None);
    assert_eq!(unit_id("vivi/unit/3ea8dee9-1"), None);
    assert_eq!(unit_id("vivi/unit/-a1"), None);
}
