use super::*;
use crate::letter::{LetterError, parse, render};

type Res = Result<(), LetterError>;

fn round_trip<T>(front: &T) -> Result<T, LetterError>
where
    T: Serialize + serde::de::DeserializeOwned + Validate,
{
    let text = render(front, "prose body")?;
    let envelope = parse::<T>(&text)?;
    assert_eq!(envelope.body, "prose body");
    Ok(envelope.front)
}

fn refused<T: serde::de::DeserializeOwned + Validate>(text: &str) -> bool {
    matches!(parse::<T>(text), Err(LetterError::Toml { .. }))
}

#[test]
fn memo_round_trips() -> Res {
    let full = MemoFront {
        tags: vec!["a".to_string(), "b c".to_string()],
    };
    assert_eq!(round_trip(&full)?, full);
    assert_eq!(round_trip(&MemoFront::default())?, MemoFront::default());
    Ok(())
}

#[test]
fn memo_blank_tag_is_invalid() {
    let text = "+++\ntags = [\"ok\", \" \"]\n+++\n\nx";
    assert!(matches!(
        parse::<MemoFront>(text),
        Err(LetterError::Invalid(_))
    ));
}

#[test]
fn rule_round_trips() -> Res {
    for scope in [RuleScope::Local, RuleScope::Project] {
        let full = RuleFront {
            scope: Some(scope),
            supersedes: Some("abc123".to_string()),
        };
        assert_eq!(round_trip(&full)?, full);
    }
    assert_eq!(round_trip(&RuleFront::default())?, RuleFront::default());
    Ok(())
}

#[test]
fn rule_scope_and_blank_supersedes_refused() {
    assert!(refused::<RuleFront>("+++\nscope = \"global\"\n+++\n\nx"));
    let blank = "+++\nsupersedes = \"  \"\n+++\n\nx";
    assert!(matches!(
        parse::<RuleFront>(blank),
        Err(LetterError::Invalid(_))
    ));
}

#[test]
fn close_round_trips() -> Res {
    let mut repos = BTreeMap::new();
    repos.insert("vivi".to_string(), "abc1234".to_string());
    repos.insert("vivi-server".to_string(), "def5678".to_string());
    let full = CloseFront {
        verdict: Some("clean".to_string()),
        branch: Some("w3/x".to_string()),
        commit: Some("0123abc".to_string()),
        repos,
    };
    assert_eq!(round_trip(&full)?, full);
    assert_eq!(round_trip(&CloseFront::default())?, CloseFront::default());
    Ok(())
}

#[test]
fn close_repos_table_is_last_and_open() -> Res {
    let mut repos = BTreeMap::new();
    repos.insert("anything".to_string(), "tip".to_string());
    let front = CloseFront {
        verdict: Some("v".to_string()),
        branch: None,
        commit: None,
        repos,
    };
    let text = render(&front, "x")?;
    assert!(text.find("verdict").unwrap_or(usize::MAX) < text.find("[repos]").unwrap_or(0));
    assert!(parse::<CloseFront>(&text).is_ok());
    Ok(())
}

#[test]
fn close_branch_without_commit_is_invalid() {
    let text = "+++\nbranch = \"b\"\n+++\n\nx";
    assert!(matches!(
        parse::<CloseFront>(text),
        Err(LetterError::Invalid(_))
    ));
    let text = "+++\ncommit = \"c\"\n+++\n\nx";
    assert!(matches!(
        parse::<CloseFront>(text),
        Err(LetterError::Invalid(_))
    ));
}

#[test]
fn close_blank_values_are_invalid() {
    for text in [
        "+++\nverdict = \" \"\n+++\n\nx",
        "+++\n[repos]\nvivi = \"\"\n+++\n\nx",
        "+++\n[repos]\n\" \" = \"tip\"\n+++\n\nx",
    ] {
        assert!(matches!(
            parse::<CloseFront>(text),
            Err(LetterError::Invalid(_))
        ));
    }
}

#[test]
fn close_clear_sentinel_is_accepted() {
    assert!(parse::<CloseFront>("+++\nverdict = \"-\"\n+++\n\nx").is_ok());
}

#[test]
fn close_unknown_key_refused() {
    assert!(refused::<CloseFront>("+++\nextra = 1\n+++\n\nx"));
}

#[test]
fn empty_renders_and_parses() -> Res {
    assert_eq!(render(&EmptyFront {}, "x")?, "+++\n+++\n\nx");
    let envelope = parse::<EmptyFront>("+++\n+++\n\nx")?;
    assert_eq!(envelope.front, EmptyFront {});
    assert_eq!(envelope.body, "x");
    Ok(())
}

#[test]
fn empty_refuses_any_key() {
    assert!(refused::<EmptyFront>("+++\nverdict = \"x\"\n+++\n\nx"));
}

#[test]
fn unknown_keys_refused_for_memo_and_rule() {
    assert!(refused::<MemoFront>("+++\nextra = 1\n+++\n\nx"));
    assert!(refused::<RuleFront>("+++\nextra = 1\n+++\n\nx"));
}

#[test]
fn close_events_are_the_two_verbs() {
    assert_eq!(CLOSE_EVENTS, ["done", "reopened"]);
}
