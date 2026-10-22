use axum::http::{HeaderMap, HeaderValue};

use super::config::{BackendKind, ProviderConfig};
use super::session::{header_to_add, session_id, valid_header_name};

fn provider(session_header: Option<&str>) -> ProviderConfig {
    ProviderConfig {
        kind: BackendKind::Native,
        adapter: Some("openai-passthrough".into()),
        command: None,
        backend: None,
        health: None,
        models: Vec::new(),
        credentials: None,
        fallback_credentials: None,
        key_cmd: Some("echo k".into()),
        upstream: None,
        token_url: None,
        client_id: None,
        client_version: None,
        session_header: session_header.map(str::to_owned),
        working_directory: None,
        env: std::collections::BTreeMap::default(),
    }
}

const BODY: &[u8] = br#"{"model":"glm-5","messages":[
    {"role":"system","content":"shared prompt"},
    {"role":"user","content":"fix the bug"}]}"#;

#[test]
fn adds_a_session_header_when_configured_and_absent() {
    let item = provider(Some("x-opencode-session"));
    let (name, value) = header_to_add(&item, &HeaderMap::new(), BODY).expect("added");
    assert_eq!(name.as_str(), "x-opencode-session");
    assert!(value.to_str().expect("ascii").starts_with("ses_"));
}

#[test]
fn keeps_the_client_session_header() {
    let item = provider(Some("x-opencode-session"));
    let mut inbound = HeaderMap::new();
    inbound.insert("x-opencode-session", HeaderValue::from_static("ses_client"));
    assert!(header_to_add(&item, &inbound, BODY).is_none());
}

#[test]
fn adds_nothing_when_the_option_is_unset() {
    assert!(header_to_add(&provider(None), &HeaderMap::new(), BODY).is_none());
}

#[test]
fn session_id_is_stable_per_conversation_and_ignores_later_turns() {
    let later = br#"{"model":"glm-5","messages":[
        {"role":"system","content":"shared prompt"},
        {"role":"user","content":"fix the bug"},
        {"role":"assistant","content":"done"},
        {"role":"user","content":"thanks"}]}"#;
    assert_eq!(session_id(BODY), session_id(later));
}

#[test]
fn session_id_differs_by_model_and_first_user_message() {
    let other_model = br#"{"model":"kimi","messages":[{"role":"user","content":"fix the bug"}]}"#;
    let other_task = br#"{"model":"glm-5","messages":[{"role":"user","content":"write docs"}]}"#;
    assert_ne!(session_id(BODY), session_id(other_model));
    assert_ne!(session_id(BODY), session_id(other_task));
}

#[test]
fn session_id_is_random_without_a_derivable_conversation() {
    for body in [&b""[..], b"not json", br#"{"model":"m"}"#] {
        assert_ne!(session_id(body), session_id(body));
    }
}

#[test]
fn header_name_validation_rejects_credentials_and_hop_headers() {
    assert!(valid_header_name("x-opencode-session"));
    assert!(!valid_header_name("Authorization"));
    assert!(!valid_header_name("authorization"));
    assert!(!valid_header_name("host"));
    assert!(!valid_header_name("transfer-encoding"));
    assert!(!valid_header_name("X-Upper"));
    assert!(!valid_header_name("bad name"));
    assert!(!valid_header_name(""));
}

/// Serve one request on a local socket and return its raw head, lowercased.
async fn capture_upstream(item: &ProviderConfig, inbound: HeaderMap) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let mut item = item.clone();
    item.upstream = Some(format!("http://{}", listener.local_addr().expect("addr")));
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buf = vec![0u8; 8192];
        let n = socket.read(&mut buf).await.expect("read");
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
            .await
            .expect("write");
        String::from_utf8_lossy(&buf[..n]).to_lowercase()
    });
    super::kimi::handle(
        &reqwest::Client::new(),
        &item,
        &super::route::Action::ChatCompletions,
        axum::http::Method::POST,
        None,
        inbound,
        bytes::Bytes::from_static(BODY),
        "req",
    )
    .await
    .expect("handled");
    server.await.expect("server")
}

#[tokio::test]
async fn upstream_receives_one_generated_session_header() {
    let head = capture_upstream(&provider(Some("x-opencode-session")), HeaderMap::new()).await;
    assert_eq!(
        head.matches("x-opencode-session: ses_").count(),
        1,
        "{head}"
    );
}

#[tokio::test]
async fn upstream_receives_the_client_session_header_unchanged() {
    let mut inbound = HeaderMap::new();
    inbound.insert("x-opencode-session", HeaderValue::from_static("ses_client"));
    let head = capture_upstream(&provider(Some("x-opencode-session")), inbound).await;
    assert_eq!(head.matches("x-opencode-session:").count(), 1, "{head}");
    assert!(head.contains("x-opencode-session: ses_client"));
}

#[tokio::test]
async fn upstream_receives_no_session_header_when_unset() {
    let head = capture_upstream(&provider(None), HeaderMap::new()).await;
    assert!(!head.contains("x-opencode-session"), "{head}");
}
