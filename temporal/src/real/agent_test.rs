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
