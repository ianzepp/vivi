use super::*;
use crate::judgment::Judgment;

fn config(provider: Option<&str>, key_cmd: Option<&str>, endpoint: Option<&str>) -> Judgment {
    Judgment {
        provider: provider.map(str::to_string),
        key_cmd: key_cmd.map(str::to_string),
        endpoint: endpoint.map(str::to_string),
        model: None,
        timeout_ms: None,
    }
}

#[test]
fn from_config_none_when_absent() {
    assert!(TypesafeProvider::from_config(None).unwrap().is_none());
    assert!(
        TypesafeProvider::from_config(Some(&config(None, None, None)))
            .unwrap()
            .is_none()
    );
}

#[test]
fn from_config_rejects_unknown_provider() {
    let err = TypesafeProvider::from_config(Some(&config(Some("openai"), None, None))).unwrap_err();
    assert!(err.to_string().contains("not supported"), "{err}");
}

#[test]
fn from_config_requires_key_cmd() {
    let err =
        TypesafeProvider::from_config(Some(&config(Some("typesafe"), None, None))).unwrap_err();
    assert!(err.to_string().contains("key_cmd"), "{err}");
}

#[test]
fn from_config_resolves_key_cmd() {
    let provider = TypesafeProvider::from_config(Some(&config(
        Some("typesafe"),
        Some("printf test-key-value"),
        None,
    )))
    .unwrap()
    .expect("provider");
    assert_eq!(provider.model(), "jev-latest");
}

#[test]
fn key_cmd_failure_reports_no_secret() {
    let err = TypesafeProvider::from_config(Some(&config(
        Some("typesafe"),
        Some("echo leaked-secret; exit 1"),
        None,
    )))
    .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("key_cmd failed"), "{message}");
    assert!(!message.contains("leaked-secret"), "{message}");
}

#[test]
fn key_cmd_empty_output_is_rejected() {
    let err = TypesafeProvider::from_config(Some(&config(Some("typesafe"), Some("true"), None)))
        .unwrap_err();
    assert!(err.to_string().contains("empty"), "{err}");
}

#[test]
fn unreachable_endpoint_classifies_quickly() {
    let provider = TypesafeProvider::from_config(Some(&config(
        Some("typesafe"),
        Some("printf test-key-value"),
        Some("http://127.0.0.1:1"),
    )))
    .unwrap()
    .expect("provider");
    let start = std::time::Instant::now();
    let err = provider
        .ask(
            &serde_json::json!({"state": "x"}),
            &[JudgmentQuestion::noul("q", "is x x?", "yes", "no")],
        )
        .unwrap_err();
    assert!(err.to_string().contains("unreachable"), "{}", err);
    assert!(
        start.elapsed() < std::time::Duration::from_secs(2),
        "local refusal should be fast"
    );
}
