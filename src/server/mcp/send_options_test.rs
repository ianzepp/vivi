use super::*;
use serde_json::json;

fn ok(arguments: Value) -> SendOptions {
    match SendOptions::parse(&arguments) {
        Ok(options) => options,
        Err(err) => panic!("parse should succeed: {}", err.message),
    }
}

#[test]
fn dispatch_defaults_to_false_when_absent_or_null() {
    for arguments in [json!({}), json!({"dispatch": null})] {
        let options = ok(arguments);
        assert!(!options.dispatch);
        assert_eq!(options.priority, None);
    }
}

#[test]
fn explicit_false_and_true_dispatch_parse() {
    let options = ok(json!({"dispatch": false, "kind": "task"}));
    assert!(!options.dispatch);
    assert_eq!(options.priority, None);

    let options = ok(json!({"dispatch": true, "kind": "need"}));
    assert!(options.dispatch);
    assert_eq!(options.priority, None);
}

#[test]
fn malformed_dispatch_fails() {
    for arguments in [
        json!({"dispatch": "yes", "kind": "need"}),
        json!({"dispatch": 1, "kind": "need"}),
        json!({"dispatch": [], "kind": "need"}),
    ] {
        assert!(SendOptions::parse(&arguments).is_err(), "{arguments}");
    }
}

#[test]
fn priority_accepts_zero_and_the_i64_maximum() {
    let options = ok(json!({"dispatch": true, "kind": "need", "priority": 0}));
    assert!(options.dispatch);
    assert_eq!(options.priority, Some(0));

    let options = ok(json!({
        "dispatch": true,
        "kind": "need",
        "priority": i64::MAX,
    }));
    assert_eq!(options.priority, Some(i64::MAX));
}

#[test]
fn an_invalid_priority_value_class_fails() {
    for priority in [
        Value::Null,
        json!("1"),
        json!(true),
        json!(1.0),
        json!(-1),
        json!([]),
        json!({}),
        json!(9_223_372_036_854_775_808u64),
    ] {
        let arguments = json!({"dispatch": true, "kind": "need", "priority": priority});
        assert!(SendOptions::parse(&arguments).is_err(), "{priority}");
    }
}

#[test]
fn a_priority_requires_dispatch() {
    for arguments in [
        json!({"priority": 2}),
        json!({"priority": 2, "dispatch": false}),
        json!({"priority": 2, "dispatch": null}),
        json!({"priority": 0}),
    ] {
        assert!(SendOptions::parse(&arguments).is_err(), "{arguments}");
    }
}

#[test]
fn dispatch_requires_kind_need() {
    for kind in ["task", "want", "mail", "chat"] {
        let arguments = json!({"dispatch": true, "kind": kind});
        assert!(SendOptions::parse(&arguments).is_err(), "{kind}");
    }
    // A missing or non-string kind is not a need either.
    assert!(SendOptions::parse(&json!({"dispatch": true})).is_err());
    assert!(SendOptions::parse(&json!({"dispatch": true, "kind": 7})).is_err());
}

#[test]
fn ordinary_sends_without_dispatch_are_not_touched() {
    for kind in ["task", "want", "mail", "chat"] {
        let options = ok(json!({"kind": kind}));
        assert!(!options.dispatch);
        assert_eq!(options.priority, None);
    }
}

#[test]
fn non_negative_integer_validates_a_supplied_level() {
    assert_eq!(non_negative_integer(&json!(0), "level").ok(), Some(0));
    assert_eq!(
        non_negative_integer(&json!(i64::MAX), "level").ok(),
        Some(i64::MAX)
    );
    for value in [
        Value::Null,
        json!(-1),
        json!(1.0),
        json!("0"),
        json!(9_223_372_036_854_775_808u64),
    ] {
        assert!(non_negative_integer(&value, "level").is_err(), "{value}");
    }
}
