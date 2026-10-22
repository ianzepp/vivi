//! Native xAI Grok adapter against synthetic login files and a local mock.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use vivi::proxy::config::{self, ProxyConfig};
use vivi::proxy::dead::DEAD_MESSAGE;
use vivi::proxy::runtime;

#[path = "support/proxy_upstream.rs"]
mod proxy_upstream;

use proxy_upstream::{Mock, Script, Seen, sse_body};

const PAST: &str = "2020-01-01T00:00:00.000000Z";
const FUTURE: &str = "2099-01-01T00:00:00.000000Z";

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap()
}

fn entry(key: &str, refresh: &str, expires_at: &str, client_id: &str) -> Value {
    json!({
        "key": key,
        "auth_mode": "oidc",
        "user_id": "user-1",
        "email": "synthetic@example.invalid",
        "coding_data_retention_opt_out": true,
        "refresh_token": refresh,
        "expires_at": expires_at,
        "oidc_client_id": client_id,
    })
}

fn write_login(dir: &TempDir, addr: SocketAddr, entries: &[(&str, Value)]) -> PathBuf {
    let mut root = serde_json::Map::new();
    for (client_id, value) in entries {
        root.insert(format!("http://{addr}::{client_id}"), value.clone());
    }
    let path = dir.path().join("auth.json");
    std::fs::write(&path, Value::Object(root).to_string()).unwrap();
    set_mode(&path, 0o644);
    path
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}

fn config_for(dir: &TempDir, addr: SocketAddr, extra: &str) -> ProxyConfig {
    let text = format!(
        r#"
        log_dir = "{}"
        [providers.grok]
        kind = "native"
        adapter = "xai-grok"
        upstream = "http://{addr}/v1"
        token_url = "http://{addr}/oauth2/token"
        {extra}
        "#,
        dir.path().join("logs").display(),
    );
    config::parse_at(dir.path().join("proxy.toml"), &text).unwrap()
}

fn with_credentials(dir: &TempDir, addr: SocketAddr, creds: &Path, extra: &str) -> ProxyConfig {
    config_for(
        dir,
        addr,
        &format!("credentials = \"{}\"\n{extra}", creds.display()),
    )
}

fn start(rt: &tokio::runtime::Runtime, mut config: ProxyConfig) -> (runtime::TestServer, String) {
    config.listen = "127.0.0.1:0".parse().unwrap();
    let server = rt.block_on(runtime::spawn_test(config)).unwrap();
    let base = format!("http://{}", server.addr);
    (server, base)
}

fn post(base: &str, path: &str) -> reqwest::blocking::Response {
    client()
        .post(format!("{base}/grok/v1{path}"))
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .body(r#"{"model":"grok-test","messages":[{"role":"user","content":"hi"}]}"#)
        .send()
        .unwrap()
}

fn reply(status: u16, body: &str) -> Script {
    Script {
        status,
        headers: vec![("content-type".into(), "application/json".into())],
        body: body.as_bytes().to_vec(),
        sse: false,
    }
}

fn is_token(row: &Seen) -> bool {
    row.path.ends_with("/token")
}

fn tokens(mock: &Mock) -> Vec<Seen> {
    mock.seen().into_iter().filter(is_token).collect()
}

fn upstream(mock: &Mock) -> Vec<Seen> {
    mock.seen()
        .into_iter()
        .filter(|row| !is_token(row))
        .collect()
}

fn auth_of(row: &Seen) -> String {
    row.headers
        .iter()
        .find(|(name, _)| name == "authorization")
        .map(|(_, value)| value.clone())
        .unwrap_or_default()
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn fresh_token_is_used_without_refresh() {
    let mock = Mock::new();
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_login(
        &dir,
        addr,
        &[("cid-1", entry("fresh-key", "r1", FUTURE, "cid-1"))],
    );
    let rt = runtime();
    let (server, base) = start(&rt, with_credentials(&dir, addr, &creds, ""));
    let response = post(&base, "/chat/completions");
    assert_eq!(response.status().as_u16(), 200);
    assert!(tokens(&mock).is_empty());
    let seen = upstream(&mock);
    assert_eq!(seen[0].path, "/v1/chat/completions");
    assert_eq!(auth_of(&seen[0]), "Bearer fresh-key");
    server.stop();
}

#[test]
fn near_expiry_token_refreshes_and_writes_back_in_place() {
    let mock = Mock::new();
    mock.set_token(200, r#"{"access_token":"new-key","expires_in":3600}"#);
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let other = entry("other-key", "other-r", FUTURE, "cid-2");
    let creds = write_login(
        &dir,
        addr,
        &[("cid-1", entry("old-key", "r1", PAST, "cid-1"))],
    );
    let mut root = read(&creds);
    root["https://elsewhere.example::cid-2"] = other.clone();
    std::fs::write(&creds, root.to_string()).unwrap();
    set_mode(&creds, 0o644);
    let rt = runtime();
    let (server, base) = start(&rt, with_credentials(&dir, addr, &creds, ""));
    assert_eq!(post(&base, "/chat/completions").status().as_u16(), 200);
    let form = String::from_utf8(tokens(&mock)[0].body.clone()).unwrap();
    assert!(form.contains("grant_type=refresh_token"), "{form}");
    assert!(form.contains("refresh_token=r1"), "{form}");
    assert!(form.contains("client_id=cid-1"), "{form}");
    assert_eq!(auth_of(&upstream(&mock)[0]), "Bearer new-key");
    let saved = read(&creds);
    let name = format!("http://{addr}::cid-1");
    assert_eq!(saved[&name]["key"], "new-key");
    assert_eq!(saved[&name]["refresh_token"], "r1");
    assert_eq!(saved[&name]["email"], "synthetic@example.invalid");
    assert_eq!(saved[&name]["user_id"], "user-1");
    assert_eq!(saved[&name]["coding_data_retention_opt_out"], true);
    assert_ne!(saved[&name]["expires_at"], PAST);
    assert!(saved[&name]["expires_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(saved["https://elsewhere.example::cid-2"], other);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&creds).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    assert_eq!(post(&base, "/chat/completions").status().as_u16(), 200);
    assert_eq!(
        tokens(&mock).len(),
        1,
        "second request reuses the new token"
    );
    server.stop();
}

#[test]
fn refresh_token_rotation_is_written_back() {
    let mock = Mock::new();
    mock.set_token(
        200,
        r#"{"access_token":"new-key","refresh_token":"r2","expires_in":3600}"#,
    );
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_login(&dir, addr, &[("cid-1", entry("old", "r1", PAST, "cid-1"))]);
    let rt = runtime();
    let (server, base) = start(&rt, with_credentials(&dir, addr, &creds, ""));
    assert_eq!(post(&base, "/chat/completions").status().as_u16(), 200);
    let saved = read(&creds);
    assert_eq!(
        saved[format!("http://{addr}::cid-1")]["refresh_token"],
        "r2"
    );
    server.stop();
}

#[test]
fn a_401_triggers_exactly_one_refresh_and_retry() {
    let mock = Mock::new();
    mock.push(reply(401, r#"{"error":"expired"}"#));
    mock.push(reply(200, r#"{"ok":true}"#));
    mock.set_token(200, r#"{"access_token":"new-key","expires_in":3600}"#);
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_login(
        &dir,
        addr,
        &[("cid-1", entry("stale-key", "r1", FUTURE, "cid-1"))],
    );
    let rt = runtime();
    let (server, base) = start(&rt, with_credentials(&dir, addr, &creds, ""));
    assert_eq!(post(&base, "/chat/completions").status().as_u16(), 200);
    assert_eq!(tokens(&mock).len(), 1);
    let seen = upstream(&mock);
    assert_eq!(seen.len(), 2);
    assert_eq!(auth_of(&seen[0]), "Bearer stale-key");
    assert_eq!(auth_of(&seen[1]), "Bearer new-key");
    server.stop();
}

#[test]
fn a_second_401_is_returned_without_another_refresh() {
    let mock = Mock::new();
    mock.push(reply(401, r#"{"error":"no"}"#));
    mock.push(reply(401, r#"{"error":"still no"}"#));
    mock.set_token(200, r#"{"access_token":"new-key","expires_in":3600}"#);
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_login(&dir, addr, &[("cid-1", entry("k", "r1", FUTURE, "cid-1"))]);
    let rt = runtime();
    let (server, base) = start(&rt, with_credentials(&dir, addr, &creds, ""));
    assert_eq!(post(&base, "/chat/completions").status().as_u16(), 401);
    assert_eq!(tokens(&mock).len(), 1);
    assert_eq!(upstream(&mock).len(), 2);
    server.stop();
}

#[test]
fn refused_refresh_errors_without_token_text_and_goes_dead() {
    let mock = Mock::new();
    mock.set_token(400, r#"{"error":"invalid_grant","detail":"SECRET-BODY"}"#);
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_login(
        &dir,
        addr,
        &[(
            "cid-1",
            entry("SECRET-ACCESS", "SECRET-REFRESH", PAST, "cid-1"),
        )],
    );
    let rt = runtime();
    let (server, base) = start(&rt, with_credentials(&dir, addr, &creds, ""));
    for _ in 0..2 {
        let response = post(&base, "/chat/completions");
        assert_eq!(response.status().as_u16(), 502);
        let text = response.text().unwrap();
        assert!(text.contains(DEAD_MESSAGE), "{text}");
        for secret in ["SECRET-BODY", "SECRET-ACCESS", "SECRET-REFRESH"] {
            assert!(!text.contains(secret), "{text}");
        }
    }
    assert_eq!(tokens(&mock).len(), 1, "dead credential is not retried");
    assert!(upstream(&mock).is_empty());
    let health: Value = client()
        .get(format!("{base}/grok/health"))
        .header("host", "127.0.0.1")
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(health["status"], "credential_dead");
    server.stop();
}

#[test]
fn transient_refresh_failure_is_a_redacted_502_and_not_dead() {
    let mock = Mock::new();
    mock.set_token(503, r#"{"detail":"SECRET-BODY"}"#);
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_login(&dir, addr, &[("cid-1", entry("old", "r1", PAST, "cid-1"))]);
    let rt = runtime();
    let (server, base) = start(&rt, with_credentials(&dir, addr, &creds, ""));
    for _ in 0..2 {
        let response = post(&base, "/chat/completions");
        assert_eq!(response.status().as_u16(), 502);
        assert!(!response.text().unwrap().contains("SECRET-BODY"));
    }
    assert_eq!(tokens(&mock).len(), 2);
    server.stop();
}

#[test]
fn client_id_picks_among_entries_and_ambiguity_is_an_error() {
    let mock = Mock::new();
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_login(
        &dir,
        addr,
        &[
            ("cid-1", entry("key-one", "r1", FUTURE, "cid-1")),
            ("cid-2", entry("key-two", "r2", FUTURE, "cid-2")),
        ],
    );
    let rt = runtime();
    let (server, base) = start(&rt, with_credentials(&dir, addr, &creds, ""));
    let response = post(&base, "/chat/completions");
    assert_eq!(response.status().as_u16(), 502);
    assert!(!response.text().unwrap().contains("key-"));
    assert!(upstream(&mock).is_empty());
    server.stop();
    let (server, base) = start(
        &rt,
        with_credentials(&dir, addr, &creds, "client_id = \"cid-2\""),
    );
    assert_eq!(post(&base, "/chat/completions").status().as_u16(), 200);
    assert_eq!(auth_of(&upstream(&mock)[0]), "Bearer key-two");
    server.stop();
}

#[test]
fn key_cmd_is_a_static_bearer_with_no_refresh() {
    let mock = Mock::new();
    mock.push(reply(401, r#"{"error":"no"}"#));
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let rt = runtime();
    let (server, base) = start(
        &rt,
        config_for(&dir, addr, "key_cmd = \"printf static-key\""),
    );
    assert_eq!(post(&base, "/chat/completions").status().as_u16(), 401);
    assert!(tokens(&mock).is_empty());
    let seen = upstream(&mock);
    assert_eq!(seen.len(), 1);
    assert_eq!(auth_of(&seen[0]), "Bearer static-key");
    server.stop();
}

#[test]
fn any_path_passes_through_and_streams() {
    let mock = Mock::new();
    mock.push(Script {
        status: 200,
        headers: vec![],
        body: sse_body(&[r#"{"type":"response.completed"}"#]),
        sse: true,
    });
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_login(&dir, addr, &[("cid-1", entry("k", "r1", FUTURE, "cid-1"))]);
    let rt = runtime();
    let (server, base) = start(&rt, with_credentials(&dir, addr, &creds, ""));
    let response = post(&base, "/responses");
    assert_eq!(response.status().as_u16(), 200);
    assert!(response.text().unwrap().contains("response.completed"));
    let response = client()
        .get(format!("{base}/grok/v1/models?limit=2"))
        .header("host", "127.0.0.1")
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let paths: Vec<String> = upstream(&mock).into_iter().map(|row| row.path).collect();
    assert_eq!(paths, ["/v1/responses", "/v1/models?limit=2"]);
    server.stop();
}

#[test]
fn configured_models_are_served_locally() {
    let mock = Mock::new();
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_login(&dir, addr, &[("cid-1", entry("k", "r1", FUTURE, "cid-1"))]);
    let rt = runtime();
    let (server, base) = start(
        &rt,
        with_credentials(&dir, addr, &creds, "models = [\"grok-code\"]"),
    );
    let list: Value = client()
        .get(format!("{base}/grok/v1/models"))
        .header("host", "127.0.0.1")
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(list["data"][0]["id"], "grok-code");
    assert!(mock.seen().is_empty());
    server.stop();
}
