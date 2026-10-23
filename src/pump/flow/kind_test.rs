//! Tests for the flow kind of a need.

use super::of;
use crate::letter::fixture::record_body;
use crate::pump::flow::def::FlowKind;

fn marks(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_string()).collect()
}

#[test]
fn front_matter_kind_beats_the_marks_and_a_missing_one_falls_to_them() {
    let body = record_body("abcd1234", "release");
    assert_eq!(of(Some(&body), &marks(&["research"])), FlowKind::Release);
    let bare = record_body("abcd1234", "");
    assert_eq!(of(Some(&bare), &marks(&["research"])), FlowKind::Research);
}

#[test]
fn a_body_that_does_not_parse_uses_the_marks() {
    let broken = "+++\nneed = \nkind: release\n";
    assert_eq!(of(Some(broken), &marks(&["repair"])), FlowKind::Repair);
    assert_eq!(
        of(Some("kind: release"), &marks(&["repair"])),
        FlowKind::Repair
    );
}

#[test]
fn marks_map_in_order_research_release_repair() {
    assert_eq!(
        of(None, &marks(&["repair", "release", "research"])),
        FlowKind::Research
    );
    assert_eq!(of(None, &marks(&["repair", "release"])), FlowKind::Release);
    assert_eq!(of(None, &marks(&["repair"])), FlowKind::Repair);
}

#[test]
fn no_kind_and_no_mark_is_build() {
    assert_eq!(of(None, &marks(&[])), FlowKind::Build);
    assert_eq!(
        of(Some("no kind here"), &marks(&["other"])),
        FlowKind::Build
    );
}
