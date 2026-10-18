//! Sticky credential-dead behavior for native Kimi and Codex.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use vivi::cli::ProxyCheckCommand;
use vivi::proxy::check;
use vivi::proxy::config::{self, ProxyConfig};
use vivi::proxy::dead::DEAD_MESSAGE;
use vivi::proxy::providers;
use vivi::proxy::runtime;

#[path = "support/proxy_upstream.rs"]
mod proxy_upstream;

use proxy_upstream::{Mock, Script, sse_body};

const EXPIRED_JWT: &str = "eyJhbGciOiJub25lIn0.eyJleHAiOjF9.x";
const FUTURE_JWT: &str = "eyJhbGciOiJub25lIn0.eyJleHAiOjk5OTk5OTk5OTl9.x";

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn load(dir: &TempDir, text: &str) -> (PathBuf, ProxyConfig) {
    let path = dir.path().join("proxy.toml");
    std::fs::write(&path, text).unwrap();
    (path.clone(), config::parse_at(path, text).unwrap())
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap()
}

fn wait(rt: &tokio::runtime::Runtime, mut config: ProxyConfig) -> (runtime::TestServer, String) {
    config.listen = "127.0.0.1:0".parse().unwrap();
    let server = rt.block_on(runtime::spawn_test(config)).unwrap();
    let base = format!("http://{}", server.addr);
    (server, base)
}

fn token_count(seen: &[proxy_upstream::Seen]) -> usize {
    seen.iter()
        .filter(|row| row.path.contains("/oauth/token") || row.path.ends_with("/token"))
        .count()
}

fn upstream_count(seen: &[proxy_upstream::Seen]) -> usize {
    seen.iter()
        .filter(|row| !row.path.contains("/oauth/token") && !row.path.ends_with("/token"))
        .count()
}

fn post_chat(base: &str, provider: &str) -> reqwest::blocking::Response {
    client()
        .post(format!("{base}/{provider}/v1/chat/completions"))
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .body(r#"{"model":"test","messages":[{"role":"user","content":"hi"}]}"#)
        .send()
        .unwrap()
}

fn post_responses(base: &str) -> reqwest::blocking::Response {
    client()
        .post(format!("{base}/codex/v1/responses"))
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .body(r#"{"model":"gpt-6.1-sol","input":[]}"#)
        .send()
        .unwrap()
}

fn assert_dead_body(response: reqwest::blocking::Response) {
    assert_eq!(response.status().as_u16(), 502);
    let body = response.text().unwrap();
    assert!(body.contains(DEAD_MESSAGE), "{body}");
    assert!(body.contains("credential_error"), "{body}");
}

fn get_json(base: &str, path: &str) -> (u16, Value) {
    let response = client()
        .get(format!("{base}{path}"))
        .header("host", "127.0.0.1")
        .send()
        .unwrap();
    let status = response.status().as_u16();
    let value = response.json().unwrap();
    (status, value)
}

fn kimi_config(dir: &TempDir, creds: &std::path::Path, addr: std::net::SocketAddr) -> ProxyConfig {
    load(
        dir,
        &format!(
            r#"
            log_dir = "{}"
            [providers.kimi]
            kind = "native"
            adapter = "kimi-coding"
            credentials = "{}"
            upstream = "http://{addr}/coding/v1"
            token_url = "http://{addr}/oauth/token"
            "#,
            dir.path().join("logs").display(),
            creds.display()
        ),
    )
    .1
}

fn kimi_llama_config(
    dir: &TempDir,
    creds: &std::path::Path,
    kimi: std::net::SocketAddr,
    llama: std::net::SocketAddr,
) -> ProxyConfig {
    load(
        dir,
        &format!(
            r#"
            log_dir = "{}"
            [providers.kimi]
            kind = "native"
            adapter = "kimi-coding"
            credentials = "{}"
            upstream = "http://{kimi}/coding/v1"
            token_url = "http://{kimi}/oauth/token"
            [providers.llama]
            kind = "upstream"
            backend = "http://{llama}"
            "#,
            dir.path().join("logs").display(),
            creds.display()
        ),
    )
    .1
}

fn write_kimi(dir: &TempDir) -> PathBuf {
    let creds = dir.path().join("kimi.json");
    std::fs::write(
        &creds,
        json!({"access_token":"old","refresh_token":"r1"}).to_string(),
    )
    .unwrap();
    creds
}

fn write_codex(dir: &TempDir, access: &str) -> PathBuf {
    let creds = dir.path().join("auth.json");
    std::fs::write(
        &creds,
        json!({"tokens":{"access_token":access,"refresh_token":"r1","account_id":"acct"}})
            .to_string(),
    )
    .unwrap();
    creds
}

fn codex_config(dir: &TempDir, creds: &std::path::Path, addr: std::net::SocketAddr) -> ProxyConfig {
    load(
        dir,
        &format!(
            r#"
            log_dir = "{}"
            [providers.codex]
            kind = "native"
            adapter = "chatgpt-codex"
            credentials = "{}"
            token_url = "http://{addr}/oauth/token"
            upstream = "http://{addr}/backend-api/codex/responses"
            "#,
            dir.path().join("logs").display(),
            creds.display()
        ),
    )
    .1
}

fn push_401(mock: &Mock) {
    mock.push(Script {
        status: 401,
        headers: vec![("content-type".into(), "application/json".into())],
        body: br#"{"error":"expired"}"#.to_vec(),
        sse: false,
    });
}

fn push_ok(mock: &Mock) {
    mock.push(Script {
        status: 200,
        headers: vec![("content-type".into(), "application/json".into())],
        body: br#"{"ok":true}"#.to_vec(),
        sse: false,
    });
}

fn push_sse(mock: &Mock) {
    mock.push(Script {
        status: 200,
        headers: vec![],
        body: sse_body(&[
            r#"{"type":"response.completed","response":{"id":"resp_1","output":[]}}"#,
        ]),
        sse: true,
    });
}

#[test]
fn idle_proxy_contacts_nothing() {
    let mock = Mock::new();
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_kimi(&dir);
    let rt = runtime();
    let (server, _base) = wait(&rt, kimi_config(&dir, &creds, addr));
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        mock.seen().is_empty(),
        "idle proxy must not refresh or probe: {:?}",
        mock.seen()
    );
    server.stop();
}

#[test]
fn kimi_refused_refresh_is_sticky_until_file_changes() {
    let mock = Mock::new();
    push_401(&mock);
    mock.set_token(400, r#"{"error":"invalid_grant"}"#);
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_kimi(&dir);
    let rt = runtime();
    let (server, base) = wait(&rt, kimi_config(&dir, &creds, addr));
    assert_dead_body(post_chat(&base, "kimi"));
    assert_eq!(token_count(&mock.seen()), 1);
    assert_eq!(upstream_count(&mock.seen()), 1);
    for _ in 0..3 {
        assert_dead_body(post_chat(&base, "kimi"));
    }
    assert_eq!(token_count(&mock.seen()), 1);
    assert_eq!(upstream_count(&mock.seen()), 1);
    let (status, health) = get_json(&base, "/kimi/health");
    assert_eq!(status, 503);
    assert_eq!(health["status"], "credential_dead");
    let (agg_status, agg) = get_json(&base, "/health");
    assert_eq!(agg_status, 503);
    assert_eq!(agg["ok"], false);
    std::fs::write(
        &creds,
        json!({"access_token":"fresh-from-login","refresh_token":"r2"}).to_string(),
    )
    .unwrap();
    push_ok(&mock);
    let recovered = post_chat(&base, "kimi");
    assert_eq!(recovered.status().as_u16(), 200);
    assert_eq!(token_count(&mock.seen()), 1);
    assert_eq!(upstream_count(&mock.seen()), 2);
    server.stop();
}

#[test]
fn kimi_transient_token_5xx_does_not_mark_dead() {
    let mock = Mock::new();
    push_401(&mock);
    push_401(&mock);
    mock.set_token(503, r#"{"error":"busy"}"#);
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_kimi(&dir);
    let rt = runtime();
    let (server, base) = wait(&rt, kimi_config(&dir, &creds, addr));
    let first = post_chat(&base, "kimi");
    assert_eq!(first.status().as_u16(), 502);
    assert_eq!(token_count(&mock.seen()), 1);
    let second = post_chat(&base, "kimi");
    assert_eq!(second.status().as_u16(), 502);
    assert_eq!(token_count(&mock.seen()), 2);
    assert_eq!(upstream_count(&mock.seen()), 2);
    let (status, health) = get_json(&base, "/kimi/health");
    assert_eq!(status, 200);
    assert_eq!(health["status"], "ok");
    server.stop();
}

#[test]
fn kimi_dead_leaves_other_providers_alone() {
    let kimi_mock = Mock::new();
    push_401(&kimi_mock);
    kimi_mock.set_token(400, r#"{"error":"invalid_grant"}"#);
    let llama_mock = Mock::new();
    llama_mock.push(Script {
        status: 200,
        headers: vec![("content-type".into(), "application/json".into())],
        body: br#"{"object":"list","data":[]}"#.to_vec(),
        sse: false,
    });
    let kimi_addr = proxy_upstream::serve(kimi_mock.clone());
    let llama_addr = proxy_upstream::serve(llama_mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_kimi(&dir);
    let rt = runtime();
    let (server, base) = wait(&rt, kimi_llama_config(&dir, &creds, kimi_addr, llama_addr));
    assert_dead_body(post_chat(&base, "kimi"));
    let llama = client()
        .get(format!("{base}/llama/v1/models"))
        .header("host", "127.0.0.1")
        .send()
        .unwrap();
    assert_eq!(llama.status().as_u16(), 200);
    assert_eq!(token_count(&kimi_mock.seen()), 1);
    assert!(!llama_mock.seen().is_empty());
    server.stop();
}

#[test]
fn kimi_check_and_providers_show_credential_dead() {
    let mock = Mock::new();
    push_401(&mock);
    mock.set_token(401, r#"{"error":"invalid_grant"}"#);
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_kimi(&dir);
    let (path, config) = load(
        &dir,
        &format!(
            r#"
            log_dir = "{}"
            [providers.kimi]
            kind = "native"
            adapter = "kimi-coding"
            credentials = "{}"
            upstream = "http://{addr}/coding/v1"
            token_url = "http://{addr}/oauth/token"
            "#,
            dir.path().join("logs").display(),
            creds.display()
        ),
    );
    let rt = runtime();
    let (server, base) = wait(&rt, config);
    assert_dead_body(post_chat(&base, "kimi"));
    std::fs::write(
        &path,
        format!(
            r#"
            listen = "{}"
            log_dir = "{}"
            [providers.kimi]
            kind = "native"
            adapter = "kimi-coding"
            credentials = "{}"
            "#,
            server.addr,
            dir.path().join("logs").display(),
            creds.display()
        ),
    )
    .unwrap();
    let result = check::print(&ProxyCheckCommand {
        provider: Some("kimi".into()),
        config: Some(path.clone()),
    });
    assert!(result.is_err());
    let mut live = config::load(Some(&path)).unwrap();
    live.listen = server.addr;
    let row = providers::format_row(&live, "kimi", &live.providers["kimi"]);
    assert!(row.contains("health=credential_dead"), "{row}");
    assert!(row.contains("credentials=dead"), "{row}");
    server.stop();
}

#[test]
fn codex_refused_refresh_is_sticky_until_file_changes() {
    let mock = Mock::new();
    mock.set_token(400, r#"{"error":"invalid_grant"}"#);
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_codex(&dir, EXPIRED_JWT);
    let rt = runtime();
    let (server, base) = wait(&rt, codex_config(&dir, &creds, addr));
    assert_dead_body(post_responses(&base));
    assert_eq!(token_count(&mock.seen()), 1);
    assert_eq!(upstream_count(&mock.seen()), 0);
    for _ in 0..3 {
        assert_dead_body(post_responses(&base));
    }
    assert_eq!(token_count(&mock.seen()), 1);
    assert_eq!(upstream_count(&mock.seen()), 0);
    let (status, health) = get_json(&base, "/codex/health");
    assert_eq!(status, 503);
    assert_eq!(health["status"], "credential_dead");
    std::fs::write(
        &creds,
        json!({"tokens":{"access_token":FUTURE_JWT,"refresh_token":"r2","account_id":"acct"}})
            .to_string(),
    )
    .unwrap();
    push_sse(&mock);
    let recovered = post_responses(&base);
    assert_eq!(recovered.status().as_u16(), 200);
    assert_eq!(token_count(&mock.seen()), 1);
    assert_eq!(upstream_count(&mock.seen()), 1);
    server.stop();
}

#[test]
fn codex_transient_token_5xx_does_not_mark_dead() {
    let mock = Mock::new();
    mock.set_token(500, r#"{"error":"busy"}"#);
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_codex(&dir, EXPIRED_JWT);
    let rt = runtime();
    let (server, base) = wait(&rt, codex_config(&dir, &creds, addr));
    assert_eq!(post_responses(&base).status().as_u16(), 502);
    assert_eq!(token_count(&mock.seen()), 1);
    assert_eq!(upstream_count(&mock.seen()), 0);
    assert_eq!(post_responses(&base).status().as_u16(), 502);
    assert_eq!(token_count(&mock.seen()), 2);
    let (status, health) = get_json(&base, "/codex/health");
    assert_eq!(status, 200);
    assert_eq!(health["status"], "ok");
    server.stop();
}
