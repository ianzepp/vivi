use serde_json::{Value, json};

use super::*;

fn accepted(arguments: &Value, expected: &[&str]) {
    let expected: Vec<String> = expected.iter().map(|entry| (*entry).to_owned()).collect();
    match parse_to(arguments) {
        Ok(parsed) => assert_eq!(parsed, expected, "{arguments}"),
        Err(error) => panic!("{arguments} should parse: {}", error.message),
    }
}

fn refused(arguments: &Value) {
    if let Ok(parsed) = parse_to(arguments) {
        panic!("{arguments} should be refused, parsed {parsed:?}");
    }
}

#[test]
fn an_audience_parses_or_is_refused_by_its_shape() {
    let accepted_cases: &[(Value, &[&str])] = &[
        (json!({}), &[]),
        (json!({ "to": [] }), &[]),
        (json!({ "to": "a@x.local" }), &["a@x.local"]),
        (
            json!({ "to": " a@x.local , b@y.local " }),
            &["a@x.local", "b@y.local"],
        ),
        (
            json!({ "to": ["a@x.local", "b@y.local"] }),
            &["a@x.local", "b@y.local"],
        ),
        (json!({ "to": [" a@x.local "] }), &["a@x.local"]),
    ];
    for (arguments, expected) in accepted_cases {
        accepted(arguments, expected);
    }

    let refused_cases = [
        json!({ "to": null }),
        json!({ "to": 7 }),
        json!({ "to": true }),
        json!({ "to": {} }),
        json!({ "to": [["a@x.local"]] }),
        json!({ "to": ["a@x.local", 7] }),
        json!({ "to": ["a@x.local", null] }),
    ];
    for arguments in &refused_cases {
        refused(arguments);
    }
}

#[test]
fn a_supplied_but_blank_address_is_refused() {
    let refused_cases = [
        json!({ "to": "" }),
        json!({ "to": " " }),
        json!({ "to": "a@x.local," }),
        json!({ "to": ",a@x.local" }),
        json!({ "to": "a@x.local, ,b@y.local" }),
        json!({ "to": ["a@x.local", ""] }),
        json!({ "to": ["  "] }),
    ];
    for arguments in &refused_cases {
        refused(arguments);
    }
}
