//! Parser-backed tests for the shared reply contract renderers.

use std::collections::BTreeSet;

use super::{HEADING, role_footer, slicer_footer};
use vivi_mail::letter::{
    LetterError,
    roles::{RoleKind, Slicer, SlicerMode, parse_for},
};

const EXAMPLE: &str = "Illustrative output (not work or commands to execute). Replace example identifiers, paths and targets with actual request values:\n";

fn examples(footer: &str) -> Vec<&str> {
    footer.split(EXAMPLE).skip(1).map(str::trim).collect()
}

fn raw(example: &str) -> toml::Value {
    let (front, _) = vivi_mail::letter::split(example).unwrap();
    toml::from_str(front).unwrap()
}

fn mode_name(mode: SlicerMode) -> &'static str {
    match mode {
        SlicerMode::Slice => "slice",
        SlicerMode::Rework => "rework",
        SlicerMode::Split => "split",
    }
}

#[test]
fn role_examples_parse_with_the_existing_role_types() {
    for kind in RoleKind::ALL {
        let footer = role_footer(kind);
        let rendered = examples(&footer);
        assert!(!rendered.is_empty(), "{} has no example", kind.name());
        for example in rendered {
            let parsed = parse_for(kind, example).unwrap_or_else(|err| {
                panic!("{} example did not parse: {err}\n{example}", kind.name())
            });
            assert!(
                kind.answer_words().contains(&parsed.answer.as_str()),
                "{} emitted unaccepted answer {}",
                kind.name(),
                parsed.answer
            );
        }
    }
}

#[test]
fn slicer_examples_match_the_requested_mode() {
    for mode in [SlicerMode::Slice, SlicerMode::Rework, SlicerMode::Split] {
        let footer = slicer_footer(mode);
        assert!(footer.contains(&format!("requested mode is `{}`", mode_name(mode))));
        let rendered = examples(&footer);
        assert_eq!(
            rendered.len(),
            2,
            "{} should show sliced and escalate",
            mode_name(mode)
        );
        let raw_sliced = raw(rendered[0]);
        assert_eq!(raw_sliced["answer"].as_str(), Some("sliced"));
        assert_eq!(raw_sliced["mode"].as_str(), Some(mode_name(mode)));
        let parsed = parse_for(RoleKind::Slicer, rendered[0]).unwrap();
        assert_eq!(parsed.answer, "sliced");
        let typed: Slicer = vivi_mail::letter::parse(rendered[0]).unwrap().front;
        assert_eq!(typed.mode, mode);

        let raw_escalation = raw(rendered[1]);
        assert_eq!(raw_escalation["answer"].as_str(), Some("escalate"));
        assert_eq!(raw_escalation["mode"].as_str(), Some(mode_name(mode)));
        assert!(!raw_escalation["reason"].as_str().unwrap().trim().is_empty());
        assert!(raw_escalation.get("slice").is_none());
        let escalation = parse_for(RoleKind::Slicer, rendered[1]).unwrap();
        assert_eq!(escalation.answer, "escalate");
    }
}

#[test]
fn slice_examples_have_exactly_the_six_documented_fields() {
    let expected = BTreeSet::from(["id", "title", "brief", "target", "write_scope", "deps"]);
    for mode in [SlicerMode::Slice, SlicerMode::Rework, SlicerMode::Split] {
        let footer = slicer_footer(mode);
        let rendered = examples(&footer);
        let value = raw(rendered[0]);
        let slices = value["slice"].as_array().unwrap();
        let count = if mode == SlicerMode::Split { 2 } else { 1 };
        assert_eq!(slices.len(), count);
        let mut split_prefix = None;
        let mut split_target = None;
        for item in slices {
            let fields = item.as_table().unwrap();
            let actual = fields.keys().map(String::as_str).collect::<BTreeSet<_>>();
            assert_eq!(actual, expected);
            assert!(!fields["write_scope"].as_array().unwrap().is_empty());
            assert!(fields["target"].as_str().is_some_and(|s| !s.is_empty()));
            if mode == SlicerMode::Split {
                let id = fields["id"].as_str().unwrap();
                let (prefix, number) = id.rsplit_once('.').unwrap();
                assert!(!prefix.is_empty() && number.parse::<u32>().is_ok());
                assert_eq!(split_prefix.get_or_insert(prefix), &prefix);
                let target = fields["target"].as_str().unwrap();
                assert_eq!(split_target.get_or_insert(target), &target);
            }
        }
        let typed: Slicer = vivi_mail::letter::parse(rendered[0]).unwrap().front;
        assert!(
            typed
                .slice
                .iter()
                .all(|slice| !slice.write_scope.is_empty())
        );
        if mode == SlicerMode::Split {
            assert!((2..=4).contains(&typed.slice.len()));
        }
    }
}

#[test]
fn reply_footers_fit_the_preserved_tail() {
    let mut footers = RoleKind::ALL
        .into_iter()
        .map(role_footer)
        .collect::<Vec<_>>();
    footers.extend([SlicerMode::Slice, SlicerMode::Rework, SlicerMode::Split].map(slicer_footer));
    for footer in footers {
        assert!(footer.starts_with(HEADING));
        assert!(footer.len() < 4096, "footer is {} bytes", footer.len());
        assert!(footer.contains("Unknown keys are rejected"));
        assert!(footer.contains("literal strings"));
    }
}

#[test]
fn bare_sliced_remains_no_opening() {
    let result = vivi_mail::letter::parse::<Slicer>("answer = 'sliced'");
    assert_eq!(result.unwrap_err(), LetterError::NoOpening);
}

#[test]
fn reply_contracts_state_shared_envelope_rules() {
    for footer in RoleKind::ALL
        .into_iter()
        .map(role_footer)
        .chain([SlicerMode::Slice, SlicerMode::Rework, SlicerMode::Split].map(slicer_footer))
    {
        assert!(footer.contains("nothing before it"));
        assert!(footer.contains("answer is the first key"));
        assert!(footer.contains("All tables belong inside front matter"));
        assert!(footer.contains("must not be wrapped in Markdown code fences"));
    }
}
