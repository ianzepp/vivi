//! Tests for canonical, immutable reply-validation context.

use super::{ReplyContract, SlicerRequirements, canonical_role};
use vivi_mail::letter::roles::{RoleKind, SlicerMode};

#[test]
fn canonical_roles_are_exact_role_kind_names() {
    for role in RoleKind::ALL {
        assert_eq!(canonical_role(role.name()), Some(role));
        assert_eq!(ReplyContract::for_role(role).role_kind(), Some(role));
    }
    for alias in ["hand-1", "Head-CEO", "cto-seat", ""] {
        assert_eq!(canonical_role(alias), None, "{alias}");
    }
}

#[test]
fn optional_context_distinguishes_unknown_from_known_empty() {
    let unknown = SlicerRequirements {
        mode: None,
        configured_targets: None,
        check_targets: true,
        check_scope: true,
        earlier_slice_ids: None,
    };
    let empty = SlicerRequirements {
        configured_targets: Some(Vec::new()),
        earlier_slice_ids: Some(Vec::new()),
        ..unknown.clone()
    };
    let unknown_json = serde_json::to_value(&unknown).unwrap();
    let empty_json = serde_json::to_value(&empty).unwrap();
    assert_eq!(unknown_json["configured_targets"], serde_json::Value::Null);
    assert_eq!(empty_json["configured_targets"], serde_json::json!([]));
    assert_eq!(unknown_json["earlier_slice_ids"], serde_json::Value::Null);
    assert_eq!(empty_json["earlier_slice_ids"], serde_json::json!([]));
}

#[test]
fn contracts_round_trip_serde() {
    let contract = ReplyContract {
        role: "slicer".into(),
        run: Some(3),
        answers: Some(vec!["sliced".into()]),
        slicer: Some(SlicerRequirements {
            mode: Some(SlicerMode::Rework),
            configured_targets: Some(vec!["default".into()]),
            check_targets: true,
            check_scope: true,
            earlier_slice_ids: Some(vec!["S1".into()]),
        }),
    };
    let encoded = serde_json::to_string(&contract).unwrap();
    assert_eq!(
        serde_json::from_str::<ReplyContract>(&encoded).unwrap(),
        contract
    );
}
