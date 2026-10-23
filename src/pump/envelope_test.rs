//! Tests for the generic envelope layer.

use serde::Deserialize;

use super::{EnvelopeError, Validate, parse, split};

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Probe {
    answer: String,
    #[serde(default)]
    count: Option<u32>,
    #[serde(default)]
    note: Option<String>,
}

impl Validate for Probe {
    fn validate(&self) -> Result<(), String> {
        if self.answer == "bad" {
            return Err("answer \"bad\" is not allowed".to_string());
        }
        Ok(())
    }
}

#[test]
fn split_allows_leading_blank_lines() {
    let (front, body) = split("\n  \n+++\nanswer = \"x\"\n+++\nthe report\n").unwrap();
    assert_eq!(front, "answer = \"x\"\n");
    assert_eq!(body, "the report\n");
}

#[test]
fn split_refuses_a_missing_opening_fence() {
    assert_eq!(
        split("answer = \"x\"\n+++\n"),
        Err(EnvelopeError::NoOpening)
    );
    assert_eq!(split("   \n\n"), Err(EnvelopeError::NoOpening));
}

#[test]
fn split_refuses_an_unclosed_fence() {
    assert_eq!(split("+++\nanswer = \"x\"\n"), Err(EnvelopeError::Unclosed));
}

#[test]
fn a_fence_inside_a_literal_string_is_fine_unless_it_stands_alone() {
    let inline = "+++\nanswer = \"x\"\nnote = '''\nuses +++ mid-line\n'''\n+++\nbody";
    let parsed = parse::<Probe>(inline).unwrap();
    assert_eq!(parsed.front.note.as_deref(), Some("uses +++ mid-line\n"));
    assert_eq!(parsed.body, "body");

    let alone = "+++\nanswer = \"x\"\nnote = '''\n+++\n'''\n+++\nbody";
    assert!(matches!(
        parse::<Probe>(alone),
        Err(EnvelopeError::Toml { .. })
    ));
}

#[test]
fn parse_returns_front_and_trimmed_body() {
    let parsed =
        parse::<Probe>("+++\nanswer = \"ok\"\ncount = 3\n+++\n\n# Report\n\ntext\n\n").unwrap();
    assert_eq!(parsed.front.answer, "ok");
    assert_eq!(parsed.front.count, Some(3));
    assert_eq!(parsed.body, "# Report\n\ntext");
}

#[test]
fn a_wrong_type_reports_its_line() {
    let err = parse::<Probe>("\n+++\nanswer = \"ok\"\ncount = \"three\"\n+++\n").unwrap_err();
    match &err {
        EnvelopeError::Toml { line, .. } => assert_eq!(*line, 4),
        other => panic!("{other:?}"),
    }
    let shown = err.to_string();
    assert!(shown.starts_with("front matter line 4:"), "{shown}");
    assert!(!shown.contains('\n'));
}

#[test]
fn an_unknown_key_is_refused() {
    let err = parse::<Probe>("+++\nanswer = \"ok\"\nproof = 1\n+++\n").unwrap_err();
    assert!(
        matches!(err, EnvelopeError::Toml { line: 3, .. }),
        "{err:?}"
    );
}

#[test]
fn a_broken_conditional_rule_is_invalid() {
    let err = parse::<Probe>("+++\nanswer = \"bad\"\n+++\n").unwrap_err();
    assert_eq!(
        err,
        EnvelopeError::Invalid("answer \"bad\" is not allowed".to_string())
    );
    assert_eq!(err.to_string(), "answer \"bad\" is not allowed");
}

#[test]
fn a_table_below_the_closing_fence_is_refused_with_its_line() {
    let text = "+++\nanswer = \"ok\"\n+++\nprose\n\n  [[check]]\ncmd = \"x\"\n";
    let err = parse::<Probe>(text).unwrap_err();
    assert_eq!(
        err,
        EnvelopeError::TableBelowFence {
            line: 6,
            table: "[[check]]".to_string()
        }
    );
    assert!(err.to_string().starts_with("line 6:"), "{err}");
    let follow = "+++\nanswer = \"ok\"\n+++\n[[follow_up]]\n";
    assert!(matches!(
        parse::<Probe>(follow),
        Err(EnvelopeError::TableBelowFence { line: 4, .. })
    ));
}

#[test]
fn the_same_tables_inside_the_front_matter_are_not_refused_for_position() {
    let text = "+++\nanswer = \"ok\"\n[[check]]\ncmd = \"x\"\n+++\nprose\n";
    let err = parse::<Probe>(text).unwrap_err();
    // Probe has no `check` key, so this is the generic unknown-key refusal,
    // not the below-fence one.
    assert!(matches!(err, EnvelopeError::Toml { .. }), "{err:?}");
}
