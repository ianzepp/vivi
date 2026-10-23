//! Tests for the flow kind of a need.

use super::of;
use crate::pump::flow::def::FlowKind;

fn marks(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_string()).collect()
}

#[test]
fn record_kind_line_beats_the_marks() {
    let body = "Title\nkind: release\nmore";
    assert_eq!(
        of(Some(body), &marks(&["research"])),
        (FlowKind::Release, None)
    );
    assert_eq!(
        of(Some("kind: repair"), &marks(&[])),
        (FlowKind::Repair, None)
    );
}

#[test]
fn front_matter_kind_beats_the_marks_and_a_missing_one_falls_to_them() {
    use crate::letter::fixture::record_body;
    let body = record_body("abcd1234", "release");
    assert_eq!(
        of(Some(&body), &marks(&["research"])),
        (FlowKind::Release, None)
    );
    let bare = record_body("abcd1234", "");
    assert_eq!(
        of(Some(&bare), &marks(&["research"])),
        (FlowKind::Research, None)
    );
    // front matter that does not parse falls to the marks, ignoring any `kind:` line
    let broken = "+++\nneed = \nkind: release\n";
    assert_eq!(of(Some(broken), &marks(&["repair"])).0, FlowKind::Repair);
}

#[test]
fn marks_map_in_order_research_release_repair() {
    assert_eq!(
        of(None, &marks(&["repair", "release", "research"])).0,
        FlowKind::Research
    );
    assert_eq!(
        of(None, &marks(&["repair", "release"])).0,
        FlowKind::Release
    );
    assert_eq!(of(None, &marks(&["repair"])).0, FlowKind::Repair);
}

#[test]
fn no_line_and_no_mark_is_build() {
    assert_eq!(of(None, &marks(&[])), (FlowKind::Build, None));
    assert_eq!(
        of(Some("no kind here"), &marks(&["other"])).0,
        FlowKind::Build
    );
}

#[test]
fn unknown_word_falls_back_to_marks_and_is_returned() {
    let (kind, word) = of(Some("kind: mystery"), &marks(&["release"]));
    assert_eq!(kind, FlowKind::Release);
    assert_eq!(word.as_deref(), Some("mystery"));
}
