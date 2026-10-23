//! Generic key set/unset: typing, implicit parents, quoted keys, refusals.

use std::fs;

use tempfile::tempdir;
use toml_edit::DocumentMut;

use super::{parse_key, parse_value, set_in, set_key, unset_in, value_type};
use crate::mailspace::Mailspace;

fn doc(text: &str) -> DocumentMut {
    text.parse().unwrap()
}

fn set(d: &mut DocumentMut, key: &str, raw: &str) -> Result<(), crate::error::VivariumError> {
    set_in(d, &parse_key(key)?, parse_value(raw))
}

fn err_text(result: Result<(), crate::error::VivariumError>) -> String {
    result.unwrap_err().to_string()
}

#[test]
fn values_are_typed() {
    let cases = [
        ("30", "integer"),
        ("1.5", "float"),
        ("true", "boolean"),
        ("[\"p\"]", "array"),
        ("develop", "string"),
        ("\"30\"", "string"),
        ("2026-10-23", "datetime"),
    ];
    for (raw, kind) in cases {
        assert_eq!(value_type(&parse_value(raw)), kind, "{raw}");
    }
}

#[test]
fn set_creates_missing_parent_and_keeps_comments() {
    let mut d = doc("# top\n[pump]\ntick = 5 # tick\n");
    set(&mut d, "pump.dispatch.budget", "3").unwrap();
    let out = d.to_string();
    assert!(out.contains("# top"), "{out}");
    assert!(out.contains("tick = 5 # tick"), "{out}");
    assert!(out.contains("budget = 3"), "{out}");
    assert!(!out.contains("[pump]\n[pump."), "{out}");
    assert!(out.contains("[pump.dispatch]"), "{out}");
    assert_eq!(out.matches("[pump]").count(), 1, "{out}");
}

#[test]
fn set_on_empty_doc_prints_no_empty_header() {
    let mut d = doc("");
    set(&mut d, "pump.tick", "30").unwrap();
    assert_eq!(d.to_string(), "[pump]\ntick = 30\n");
}

#[test]
fn quoted_key_segments() {
    let mut d = doc("[pools.\"glm-x\"]\nkind = \"x\"\n");
    set(&mut d, "pools.\"glm-x\".slots", "4").unwrap();
    assert_eq!(d["pools"]["glm-x"]["slots"].as_integer(), Some(4));
}

#[test]
fn identities_refused() {
    for key in ["identities.0.name", "identities"] {
        let text = parse_key(key).unwrap_err().to_string();
        assert!(text.contains("role roster"), "{text}");
    }
}

#[test]
fn table_and_non_table_walks_refused() {
    let mut d = doc("[pump]\ntick = 5\n");
    assert!(err_text(set(&mut d, "pump", "1")).contains("is a table"));
    assert!(err_text(set(&mut d, "pump.tick.x", "1")).contains("is a integer, not a table"));
    let mut d = doc("[[a]]\nx = 1\n");
    assert!(err_text(set(&mut d, "a.x", "1")).contains("array of tables"));
}

#[test]
fn unset_missing_errs_and_remove_is_byte_exact() {
    let text = "# c\n[pump]\ntick = 5\nname = \"n\" # keep\n";
    let mut d = doc(text);
    assert!(err_text(unset_in(&mut d, &parse_key("pump.nope").unwrap())).contains("is not set"));
    assert!(err_text(unset_in(&mut d, &parse_key("a.b.c").unwrap())).contains("is not set"));
    unset_in(&mut d, &parse_key("pump.tick").unwrap()).unwrap();
    assert_eq!(d.to_string(), "# c\n[pump]\nname = \"n\" # keep\n");
}

#[test]
fn set_key_rolls_back_a_bad_value() {
    let tmp = tempdir().unwrap();
    Mailspace::init_with_defaults(Some(tmp.path())).unwrap();
    let config = tmp.path().join(".vivi/mailspace.toml");
    let before = fs::read_to_string(&config).unwrap();
    let err = set_key(tmp.path(), "pump.tick", "0")
        .unwrap_err()
        .to_string();
    assert!(err.contains("rolled back"), "{err}");
    assert_eq!(fs::read_to_string(&config).unwrap(), before);
}

#[test]
fn set_key_writes_typed_value() {
    let tmp = tempdir().unwrap();
    Mailspace::init_with_defaults(Some(tmp.path())).unwrap();
    let out = set_key(tmp.path(), "pump.tick", "30").unwrap();
    assert_eq!(out.kind, "integer");
    assert_eq!(out.value, "30");
    assert!(out.write.backup.is_some());
    assert!(out.write.still.is_empty(), "{:?}", out.write.still);
}
