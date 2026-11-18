use serde_json::json;

use super::*;
use crate::types::LandRequest;

fn unit(slice: &str) -> LandRequest {
    LandRequest {
        reply_to: format!("slice-n1-{slice}"),
        need: "n1".into(),
        slice: slice.into(),
        head: format!("{slice}-head"),
        branch: format!("temporal/n1/{slice}"),
    }
}

fn input() -> ComposeInput {
    ComposeInput {
        target: "pump-sandbox".into(),
        units: vec![unit("a"), unit("b"), unit("c"), unit("d")],
    }
}

#[test]
fn compose_report_maps_back_to_units() {
    let report = json!({
        "base": "base0", "candidate": "0123456789abcdef", "ref": null,
        "included": [{"branch": "temporal/n1/a", "tip": "x", "pin_resolved": false}],
        "excluded": [
            {"branch": "temporal/n1/b", "reason": "conflict", "files": ["src/lib.rs"]},
            {"branch": "temporal/n1/c", "reason": "already-merged"},
            {"branch": "temporal/n1/d", "reason": "missing"}
        ]
    });
    let got = read_compose(&report, &input(), "base0").unwrap();
    let cand = got.candidate.unwrap();
    assert_eq!(cand.id, "pump-sandbox-0123456789ab");
    assert_eq!(cand.base, "base0");
    assert_eq!(cand.units, vec![unit("a")]);
    assert_eq!(got.already, vec![unit("c")]);
    assert_eq!(got.excluded.len(), 2);
    assert_eq!(got.excluded[0].reason, "conflicts with main in src/lib.rs");
    assert_eq!(got.excluded[1].reason, "the branch is missing on the lane");
}

#[test]
fn compose_with_nothing_included_has_no_candidate() {
    let report = json!({"candidate": null, "included": [],
        "excluded": [{"branch": "temporal/n1/a", "reason": "no-op"}]});
    let got = read_compose(&report, &input(), "b").unwrap();
    assert!(got.candidate.is_none());
    assert_eq!(got.already, vec![unit("a")]);
    assert!(read_compose(&json!({"x": 1}), &input(), "b").is_none());
}

#[test]
fn gate_pass_fail_and_broken_base() {
    assert_eq!(
        read_gate(&json!({"status": "ok", "pass": true})),
        Ok(GateResult::Pass)
    );
    let red = read_gate(&json!({"sha": "c1", "status": "ok", "pass": false,
        "new_failures": ["lib[src/lib.rs]::tests::adds"]}))
    .unwrap();
    let GateResult::Fail { text } = red else {
        panic!("expected a red gate")
    };
    assert!(text.contains("tests::adds"), "{text}");
    let built = read_gate(&json!({"status": "build_failed", "pass": false,
        "build_errors": ["error[E0308]: mismatched types"]}))
    .unwrap();
    assert!(matches!(built, GateResult::Fail { text } if text.contains("E0308")));
    assert!(read_gate(&json!({"status": "baseline_build_failed", "pass": false})).is_err());
    assert!(read_gate(&json!({"status": "error", "error": "lock"})).is_err());
    assert!(read_gate(&json!({"status": "ok"})).is_err());
}
