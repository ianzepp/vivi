use super::{intake_clippy_diagnostic_difference, parse};

#[test]
fn rustc_text_normalizes_checkout_root_and_color_only() {
    let a = "error[E0308]: mismatched types\n --> /work/one/crates/demo/src/lib.rs:7:4\n";
    let b = "\u{1b}[31merror[E0308]: mismatched types\u{1b}[0m\n --> /other/two/crates/demo/src/lib.rs:7:4\n";
    assert_eq!(parse(a), parse(b));
}

#[test]
fn code_location_and_message_changes_are_new_evidence() {
    let base = parse("error[E0308]: mismatched types\n --> /work/x/src/lib.rs:7:4\n").unwrap();
    for changed in [
        "error[E0309]: mismatched types\n --> /work/x/src/lib.rs:7:4\n",
        "error[E0308]: mismatched types\n --> /work/x/src/lib.rs:8:4\n",
        "error[E0308]: expected integer\n --> /work/x/src/lib.rs:7:4\n",
    ] {
        let candidate = parse(changed).unwrap();
        assert!(intake_clippy_diagnostic_difference(&base, &candidate));
    }
}

#[test]
fn cargo_json_diagnostic_keeps_identity_fields() {
    let json = r#"{"reason":"compiler-message","message":{"level":"error","message":"borrow conflict","code":{"code":"E0502"},"spans":[{"file_name":"/checkout/src/main.rs","line_start":12,"column_start":3,"is_primary":true}]}}"#;
    let signature = parse(json).unwrap().pop().unwrap();
    assert!(signature.contains("E0502"));
    assert!(signature.contains("src/main.rs:12:3"));
    assert!(signature.contains("borrow conflict"));
}

#[test]
fn incomplete_code_diagnostic_is_unavailable() {
    assert_eq!(parse("error[E0308]: mismatch\n"), None);
}
