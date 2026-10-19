use super::{ProviderHealth, aggregate_json, aggregate_ok};

fn row(name: &str, status: &'static str) -> ProviderHealth {
    ProviderHealth {
        name: name.to_string(),
        kind: "native".to_string(),
        status,
        last_error: None,
        checked_ms_ago: Some(5),
    }
}

#[test]
fn aggregate_ok_true_when_every_provider_is_ok() {
    let rows = [row("codex", "ok"), row("kimi", "ok")];
    assert!(aggregate_ok(&rows));
}

#[test]
fn aggregate_ok_true_when_one_provider_is_ok_and_one_is_credential_dead() {
    let rows = [row("codex", "credential_dead"), row("kimi", "ok")];
    assert!(
        aggregate_ok(&rows),
        "one dead provider must not fail the whole listener"
    );
}

#[test]
fn aggregate_ok_false_when_no_provider_is_ok() {
    let rows = [row("codex", "credential_dead"), row("kimi", "down")];
    assert!(!aggregate_ok(&rows));
}

#[test]
fn aggregate_ok_false_without_providers() {
    assert!(!aggregate_ok(&[]));
}

#[test]
fn aggregate_json_reports_any_healthy_for_mixed_rows() {
    let rows = [row("codex", "credential_dead"), row("kimi", "ok")];
    let body = aggregate_json(&rows);
    assert_eq!(
        body.get("ok").and_then(serde_json::Value::as_bool),
        Some(aggregate_ok(&rows))
    );
    assert_eq!(
        body.get("ok").and_then(serde_json::Value::as_bool),
        Some(true)
    );
}
