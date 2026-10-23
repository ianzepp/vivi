use serde::{Deserialize, Serialize};

use super::*;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct Probe {
    answer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

impl Validate for Probe {
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Serialize)]
struct Empty {}

#[test]
fn render_writes_fences_front_and_prose() {
    let probe = Probe {
        answer: "ok".into(),
        note: None,
    };
    assert_eq!(
        render(&probe, "the prose").unwrap(),
        "+++\nanswer = \"ok\"\n+++\n\nthe prose"
    );
    let with_note = Probe {
        answer: "ok".into(),
        note: Some("n".into()),
    };
    assert_eq!(
        render(&with_note, "p").unwrap(),
        "+++\nanswer = \"ok\"\nnote = \"n\"\n+++\n\np"
    );
}

#[test]
fn render_of_empty_struct() {
    assert_eq!(render(&Empty {}, "prose").unwrap(), "+++\n+++\n\nprose");
}

#[test]
fn render_then_parse_round_trips() {
    let probe = Probe {
        answer: "ok".into(),
        note: Some("n".into()),
    };
    let text = render(&probe, "body text").unwrap();
    let parsed = parse::<Probe>(&text).unwrap();
    assert_eq!(parsed.front, probe);
    assert_eq!(parsed.body, "body text");
}

#[test]
fn new_errors_display_one_line() {
    let legacy = LetterError::Legacy {
        handle: "abc".into(),
        kind: "need".into(),
    };
    assert_eq!(
        legacy.to_string(),
        "letter abc is in the old format (no front matter); it is history and cannot be executed: send a new need with front matter"
    );
    assert_eq!(
        LetterError::UnknownKind("zap".into()).to_string(),
        "unknown letter kind `zap`: expected need, want, task, memo, rule, pump, mail or chat"
    );
    assert_eq!(
        LetterError::UnknownFormat("2".into()).to_string(),
        "letter format `2` is not known to this vivi (it reads format 1): upgrade vivi"
    );
}

#[test]
fn format_constants() {
    assert_eq!(FORMAT_HEADER, "X-Vivi-Format");
    assert_eq!(FORMAT_VERSION, "1");
}

#[test]
fn text_and_list_checks() {
    assert!(check_text("title", "x").is_ok());
    assert_eq!(
        check_text("title", "  \n").unwrap_err(),
        "`title` must not be blank"
    );
    assert!(check_list("deps", &["a".into(), "b".into()]).is_ok());
    assert!(check_list("deps", &[]).is_ok());
    assert_eq!(
        check_list("deps", &["a".into(), " ".into(), String::new()]).unwrap_err(),
        "`deps` entry 2 is blank"
    );
    assert!(is_false(&false));
    assert!(!is_false(&true));
}

#[test]
fn an_empty_unknown_kind_names_the_missing_header() {
    assert_eq!(
        LetterError::UnknownKind(String::new()).to_string(),
        "letter has no `X-Vivi-Kind` header: a format-1 letter names its kind (need, want, task, memo, rule, pump, mail or chat)"
    );
}

#[test]
fn parse_letter_keeps_table_lines_in_prose_and_parse_refuses_them() {
    let text = "+++\nanswer = \"ok\"\n+++\n\n[[check]]\nx";
    assert!(matches!(
        parse::<Probe>(text),
        Err(LetterError::TableBelowFence { line: 5, .. })
    ));
    let letter = parse_letter::<Probe>(text).unwrap();
    assert_eq!(letter.body, "[[check]]\nx");
}
