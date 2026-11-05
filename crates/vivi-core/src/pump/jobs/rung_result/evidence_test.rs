use super::{Comparison, Evidence};

#[test]
fn evidence_serializes() {
    let evidence = Evidence {
        exit: Some(101),
        failing: vec!["alpha::test".into()],
        complete: true,
        recognized: true,
        bounded: true,
    };
    let encoded = serde_json::to_string(&evidence).expect("serialize evidence");
    assert_eq!(
        serde_json::from_str::<Evidence>(&encoded).unwrap(),
        evidence
    );
    assert_eq!(Comparison::Unavailable, Comparison::Unavailable);
}

#[test]
fn unknown_and_missing_capture_completeness_are_not_complete() {
    let input = "runner capture receipt: unknown\nrunner capture stderr: 0/0 bytes complete";
    assert!(!super::capture_complete(input, "receipt"));
    assert!(super::capture_complete(input, "stderr"));
}

#[test]
fn output_bound_is_explicit() {
    assert_eq!(super::MAX_EVIDENCE_BYTES, 262144);
}
