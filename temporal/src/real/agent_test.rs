use super::*;

#[test]
fn verdicts() {
    assert_eq!(
        parse_verdict("looks fine\nVERDICT: APPROVE"),
        Some(ReviewResult::Approve)
    );
    assert_eq!(
        parse_verdict("`VERDICT: APPROVE`\n"),
        Some(ReviewResult::Approve)
    );
    assert_eq!(
        parse_verdict("VERDICT: CHANGES: cover the error path"),
        Some(ReviewResult::Changes {
            text: "cover the error path".into()
        })
    );
    // The last verdict line wins.
    assert_eq!(
        parse_verdict("VERDICT: CHANGES: x\nVERDICT: APPROVE"),
        Some(ReviewResult::Approve)
    );
    assert_eq!(parse_verdict("I approve."), None);
    assert_eq!(parse_verdict("VERDICT: maybe"), None);
}

#[test]
fn implement_brief_carries_feedback_and_rules() {
    let config = crate::config::Config::parse(include_str!("../../deploy/sandbox.toml")).unwrap();
    let target = config.target("pump-sandbox").unwrap();
    let input = ImplementInput {
        need: "t1".into(),
        slice: "s1".into(),
        branch: "temporal/t1/s1".into(),
        round: 2,
        feedback: Some("test adds ... FAILED".into()),
        resume: None,
        target: "pump-sandbox".into(),
        brief: "Add a `sub` function.".into(),
    };
    let brief = implement_brief(&input, target, "abc123");
    assert!(brief.contains("Add a `sub` function."));
    assert!(brief.contains("test adds ... FAILED"));
    assert!(brief.contains("cargo test --quiet"));
    assert!(brief.contains("abc123"));
}

fn step() -> StepInput {
    StepInput {
        need: "t1".into(),
        slice: "s1".into(),
        round: 1,
        head: "head123".into(),
        target: "pump-sandbox".into(),
        branch: "temporal/t1/s1".into(),
        brief: "Add a `sub` function.".into(),
    }
}

#[test]
fn review_brief_carries_the_diff() {
    let diff = "+pub fn sub(a: i32, b: i32) -> i32 { a - b }";
    let brief = review_brief(&step(), "base456", diff);
    assert!(brief.contains("git diff base456..head123"));
    assert!(brief.contains(diff));
    assert!(brief.contains("Add a `sub` function."));
    assert!(brief.contains("do not build"));
    assert!(
        brief
            .trim_end()
            .ends_with("`VERDICT: APPROVE` or `VERDICT: CHANGES: <what to change, one line>`")
    );
}

#[test]
fn review_brief_cuts_a_huge_diff() {
    let diff = "é".repeat(REVIEW_DIFF_MAX);
    let brief = review_brief(&step(), "b", &diff);
    assert!(brief.len() < REVIEW_DIFF_MAX + 2_000);
    assert!(brief.contains("more bytes cut"));
}
