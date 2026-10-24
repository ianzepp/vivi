//! The audit log: one JSON object per line, append-only.

use std::path::PathBuf;

use vivi_server::audit::{AuditEvent, AuditLog, AuditOutcome, AuditRecord};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vivi-server-audit-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn lines(dir: &PathBuf) -> Vec<String> {
    std::fs::read_to_string(dir.join("audit.jsonl"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn record_appends_one_line_with_the_pinned_shape() {
    let dir = scratch("shape");
    let log = AuditLog::new(&dir);
    log.record(AuditEvent {
        key_id: Some("key_abc".to_owned()),
        principal: Some("operator@p1".to_owned()),
        project_id: Some("p1".to_owned()),
        tool: "send".to_owned(),
        outcome: AuditOutcome::Ok,
        summary: "sent need h-1".to_owned(),
    })
    .unwrap();
    let lines = lines(&dir);
    assert_eq!(lines.len(), 1);
    let raw = &lines[0];
    // Wire order: at, key_id, principal, project_id, tool, outcome, summary.
    // (A serde_json Value reorders keys, so assert on the raw bytes.)
    let mut cursor = 0usize;
    for field in [
        "\"at\":",
        "\"key_id\":",
        "\"principal\":",
        "\"project_id\":",
        "\"tool\":",
        "\"outcome\":",
        "\"summary\":",
    ] {
        let at = raw[cursor..]
            .find(field)
            .unwrap_or_else(|| panic!("line must contain {field} after the last field: {raw}"))
            + cursor;
        cursor = at + field.len();
    }
    let value: serde_json::Value = serde_json::from_str(raw).unwrap();
    assert!(
        value["at"].as_str().unwrap().ends_with('Z'),
        "at is rfc3339"
    );
    assert_eq!(value["key_id"], "key_abc");
    assert_eq!(value["principal"], "operator@p1");
    assert_eq!(value["project_id"], "p1");
    assert_eq!(value["tool"], "send");
    assert_eq!(value["outcome"], "ok");
    assert_eq!(value["summary"], "sent need h-1");
    // The written line deserializes into the strict record.
    let record: AuditRecord = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(record.outcome, AuditOutcome::Ok);
}

#[test]
fn two_records_append_two_lines_with_null_key_and_project_fields() {
    let dir = scratch("two-lines");
    let log = AuditLog::new(&dir);
    log.record(AuditEvent {
        key_id: Some("key_abc".to_owned()),
        principal: None,
        project_id: None,
        tool: "key_mint".to_owned(),
        outcome: AuditOutcome::Refused,
        summary: "refused: not an operator".to_owned(),
    })
    .unwrap();
    log.record(AuditEvent {
        key_id: None,
        principal: Some("host".to_owned()),
        project_id: None,
        tool: "create_project".to_owned(),
        outcome: AuditOutcome::Error,
        summary: "storage failure".to_owned(),
    })
    .unwrap();
    let lines = lines(&dir);
    assert_eq!(lines.len(), 2);
    let first: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    let second: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    // The contract pins key_id and project_id as null, never omitted;
    // principal (never None from a writer today) stays absent.
    assert!(first.as_object().unwrap().contains_key("key_id"));
    assert_eq!(first["project_id"], serde_json::Value::Null);
    assert!(!first.as_object().unwrap().contains_key("principal"));
    assert_eq!(first["outcome"], "refused");
    assert_eq!(second["key_id"], serde_json::Value::Null);
    assert_eq!(second["principal"], "host");
    assert_eq!(second["outcome"], "error");
}

#[test]
fn an_unknown_field_on_a_line_is_refused_by_the_record_type() {
    let record: Result<AuditRecord, _> = serde_json::from_str(concat!(
        r#"{"at":"2026-10-23T00:00:00Z","key_id":"key_a","principal":null,"#,
        r#""project_id":null,"tool":"send","outcome":"ok","summary":"s","extra":1}"#
    ));
    assert!(record.is_err(), "unknown fields must not parse");
}

#[cfg(unix)]
#[test]
fn audit_jsonl_is_0600_after_create() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("mode");
    let log = AuditLog::new(&dir);
    log.record(AuditEvent {
        key_id: None,
        principal: None,
        project_id: None,
        tool: "send".to_owned(),
        outcome: AuditOutcome::Ok,
        summary: "first".to_owned(),
    })
    .unwrap();
    let mode = std::fs::metadata(dir.join("audit.jsonl"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}
