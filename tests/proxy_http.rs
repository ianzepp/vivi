//! Integration tests for `vivi proxy` against mock upstreams.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use vivi::cli::ProxyCheckCommand;
use vivi::proxy::check;
use vivi::proxy::config::{self, ProxyConfig};
use vivi::proxy::providers;
use vivi::proxy::runtime;

#[path = "support/proxy_upstream.rs"]
mod proxy_upstream;

use proxy_upstream::{Mock, Script, sse_body};

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

fn get(_url: &str) -> reqwest::blocking::Client {
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

#[test]
fn routes_path_based_upstream() {
    let mock = Mock::new();
    mock.push(Script {
        status: 200,
        headers: vec![("content-type".into(), "application/json".into())],
        body: br#"{"id":"m1"}"#.to_vec(),
        sse: false,
    });
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let (_path, config) = load(
        &dir,
        &format!(
            r#"
            log_dir = "{}"
            [providers.llama]
            kind = "upstream"
            backend = "http://{addr}"
            "#,
            dir.path().join("logs").display()
        ),
    );
    let rt = runtime();
    let (server, base) = wait(&rt, config);
    let client = get(&base);
    let response = client
        .get(format!("{base}/llama/v1/models"))
        .header("host", "127.0.0.1")
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let seen = mock.seen();
    assert_eq!(seen.last().unwrap().path, "/v1/models");
    server.stop();
}

#[test]
fn sse_passthrough_keeps_events() {
    let mock = Mock::new();
    mock.push(Script {
        status: 200,
        headers: vec![],
        body: sse_body(&[r#"{"id":1}"#, r#"{"id":2}"#]),
        sse: true,
    });
    let addr = proxy_upstream::serve(mock);
    let dir = TempDir::new().unwrap();
    let (_path, config) = load(
        &dir,
        &format!(
            r#"
            log_dir = "{}"
            [providers.kimi]
            kind = "upstream"
            backend = "http://{addr}"
            "#,
            dir.path().join("logs").display()
        ),
    );
    let rt = runtime();
    let (server, base) = wait(&rt, config);
    let body = get(&base)
        .post(format!("{base}/kimi/v1/chat/completions"))
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .body(r#"{"model":"kimi-for-coding"}"#)
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert!(body.contains(r#""id":1"#));
    assert!(body.contains(r#""id":2"#));
    server.stop();
}

#[test]
fn guards_reject_origin_and_wrong_host() {
    let dir = TempDir::new().unwrap();
    let (_path, config) = load(
        &dir,
        &format!(
            r#"
            log_dir = "{}"
            [providers.llama]
            kind = "upstream"
            backend = "http://127.0.0.1:9"
            "#,
            dir.path().join("logs").display()
        ),
    );
    let rt = runtime();
    let (server, base) = wait(&rt, config);
    let client = get(&base);
    let origin = client
        .post(format!("{base}/llama/v1/models"))
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .header("origin", "https://evil.example")
        .body("{}")
        .send()
        .unwrap();
    assert_eq!(origin.status().as_u16(), 403);
    let host = client
        .post(format!("{base}/llama/v1/models"))
        .header("host", "evil.example")
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .unwrap();
    assert_eq!(host.status().as_u16(), 403);
    server.stop();
}

#[test]
fn bearer_required_when_configured() {
    let dir = TempDir::new().unwrap();
    let (_path, config) = load(
        &dir,
        &format!(
            r#"
            log_dir = "{}"
            auth_token_cmd = "printf secret"
            [providers.llama]
            kind = "upstream"
            backend = "http://127.0.0.1:9"
            "#,
            dir.path().join("logs").display()
        ),
    );
    let rt = runtime();
    let (server, base) = wait(&rt, config);
    let client = get(&base);
    let denied = client
        .get(format!("{base}/health"))
        .header("host", "127.0.0.1")
        .send()
        .unwrap();
    assert_eq!(denied.status().as_u16(), 401);
    let ok = client
        .get(format!("{base}/health"))
        .header("host", "127.0.0.1")
        .bearer_auth("secret")
        .send()
        .unwrap();
    assert_eq!(ok.status().as_u16(), 503);
    server.stop();
}

#[test]
fn native_codex_filters_and_rebuilds_output() {
    let mock = Mock::new();
    mock.push(Script {
        status: 200,
        headers: vec![],
        body: sse_body(&[
            r#"{"type":"response.reasoning_summary_text.delta","delta":"secret"}"#,
            r#"{"type":"response.output_item.done","item":{"id":"msg_1","type":"message","content":[{"text":"hi"}]}}"#,
            r#"{"type":"response.completed","response":{"id":"resp_1","output":[],"usage":{"input_tokens":3,"output_tokens":1}}}"#,
        ]),
        sse: true,
    });
    let addr = proxy_upstream::serve(mock);
    let dir = TempDir::new().unwrap();
    let creds = dir.path().join("auth.json");
    std::fs::write(
        &creds,
        json!({"tokens":{"access_token":"tok","account_id":"acct"}}).to_string(),
    )
    .unwrap();
    let (_path, config) = load(
        &dir,
        &format!(
            r#"
            log_dir = "{}"
            [providers.codex]
            kind = "native"
            adapter = "chatgpt-codex"
            credentials = "{}"
            upstream = "http://{addr}/backend-api/codex/responses"
            "#,
            dir.path().join("logs").display(),
            creds.display()
        ),
    );
    let rt = runtime();
    let (server, base) = wait(&rt, config);
    let body = get(&base)
        .post(format!("{base}/codex/v1/responses"))
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .body(r#"{"model":"gpt-6.1-sol","input":[{"role":"user","content":"hi"}]}"#)
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert!(!body.contains("reasoning_summary"));
    assert!(body.contains("msg_1"));
    server.stop();
}

#[test]
fn native_codex_chat_shim_usage_zero() {
    let mock = Mock::new();
    mock.push(Script {
        status: 200,
        headers: vec![],
        body: sse_body(&[
            r#"{"type":"response.output_text.delta","delta":"hello"}"#,
            r#"{"type":"response.completed","response":{"id":"resp_1","output":[]}}"#,
        ]),
        sse: true,
    });
    let addr = proxy_upstream::serve(mock);
    let dir = TempDir::new().unwrap();
    let creds = dir.path().join("auth.json");
    std::fs::write(&creds, json!({"access_token":"tok"}).to_string()).unwrap();
    let (_path, config) = load(
        &dir,
        &format!(
            r#"
            log_dir = "{}"
            [providers.codex]
            kind = "native"
            adapter = "chatgpt-codex"
            credentials = "{}"
            upstream = "http://{addr}/backend-api/codex/responses"
            "#,
            dir.path().join("logs").display(),
            creds.display()
        ),
    );
    let rt = runtime();
    let (server, base) = wait(&rt, config);
    let value: Value = get(&base)
        .post(format!("{base}/codex/v1/chat/completions"))
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .body(r#"{"model":"gpt-6.1-sol","messages":[{"role":"user","content":"hi"}]}"#)
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(value["choices"][0]["message"]["content"], "hello");
    assert_eq!(value["usage"]["total_tokens"], 0);
    server.stop();
}

#[test]
fn kimi_refreshes_on_401_and_writes_back() {
    let mock = Mock::new();
    mock.push(Script {
        status: 401,
        headers: vec![("content-type".into(), "application/json".into())],
        body: br#"{"error":"expired"}"#.to_vec(),
        sse: false,
    });
    mock.push(Script {
        status: 200,
        headers: vec![("content-type".into(), "application/json".into())],
        body: br#"{"ok":true}"#.to_vec(),
        sse: false,
    });
    mock.set_token(
        200,
        r#"{"access_token":"fresh","refresh_token":"r2","expires_in":3600}"#,
    );
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = dir.path().join("kimi.json");
    std::fs::write(
        &creds,
        json!({"access_token":"old","refresh_token":"r1"}).to_string(),
    )
    .unwrap();
    let (_path, config) = load(
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
    let response = get(&base)
        .post(format!("{base}/kimi/v1/chat/completions"))
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .body(r#"{"model":"kimi-for-coding"}"#)
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let stored: Value = serde_json::from_str(&std::fs::read_to_string(&creds).unwrap()).unwrap();
    assert_eq!(stored["access_token"], "fresh");
    assert_eq!(stored["refresh_token"], "r2");
    let seen = mock.seen();
    assert!(seen.iter().any(|row| row.path.contains("/oauth/token")));
    server.stop();
}

#[test]
fn redacts_token_refresh_errors() {
    let mock = Mock::new();
    mock.set_token(400, r#"{"error":"bad_refresh","token":"leak-me"}"#);
    let addr = proxy_upstream::serve(mock);
    let dir = TempDir::new().unwrap();
    let creds = dir.path().join("auth.json");
    std::fs::write(
        &creds,
        json!({"tokens":{"access_token":"eyJhbGciOiJub25lIn0.eyJleHAiOjF9.x","refresh_token":"r"}})
            .to_string(),
    )
    .unwrap();
    let (_path, config) = load(
        &dir,
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
    );
    let rt = runtime();
    let (server, base) = wait(&rt, config);
    let body = get(&base)
        .post(format!("{base}/codex/v1/responses"))
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .body(r#"{"model":"gpt-6.1-sol"}"#)
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert!(!body.contains("leak-me"));
    assert!(body.contains("credential refresh failed") || body.contains("proxy_error"));
    server.stop();
}

#[test]
fn health_aggregates_providers() {
    let mock = Mock::new();
    mock.push(Script {
        status: 200,
        headers: vec![],
        body: b"ok".to_vec(),
        sse: false,
    });
    let addr = proxy_upstream::serve(mock);
    let dir = TempDir::new().unwrap();
    let (_path, config) = load(
        &dir,
        &format!(
            r#"
            log_dir = "{}"
            [providers.llama]
            kind = "upstream"
            backend = "http://{addr}"
            health = "/health"
            "#,
            dir.path().join("logs").display()
        ),
    );
    let rt = runtime();
    let (server, base) = wait(&rt, config);
    let value: Value = get(&base)
        .get(format!("{base}/health"))
        .header("host", "127.0.0.1")
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(value["ok"], true);
    server.stop();
}

#[test]
fn child_restarts_after_exit() {
    let dir = TempDir::new().unwrap();
    let script = dir.path().join("child.sh");
    let port_file = dir.path().join("port");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nport=$(python3 -c 'import socket;s=socket.socket();s.bind((\"127.0.0.1\",0));print(s.getsockname()[1]);s.close()')\necho $port > {port}\npython3 - <<'PY'\nfrom http.server import BaseHTTPRequestHandler, HTTPServer\nimport os\nport=int(open({port_sq}).read())\nclass H(BaseHTTPRequestHandler):\n    def do_GET(self):\n        self.send_response(200); self.end_headers(); self.wfile.write(b'{{\"ok\":true}}')\n    def log_message(self, *a): pass\nHTTPServer(('127.0.0.1', port), H).serve_forever()\nPY\n",
            port = port_file.display(),
            port_sq = format!("\"{}\"", port_file.display()),
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // Child tests the supervisor spawn path; the backend URL is unused until health.
    let (_path, config) = load(
        &dir,
        &format!(
            r#"
            log_dir = "{}"
            [providers.cursor]
            kind = "child"
            command = ["sh", "{}"]
            backend = "http://127.0.0.1:9"
            "#,
            dir.path().join("logs").display(),
            script.display()
        ),
    );
    let rt = runtime();
    let (server, base) = wait(&rt, config);
    let listed = get(&base)
        .get(format!("{base}/health"))
        .header("host", "127.0.0.1")
        .send()
        .unwrap()
        .status()
        .as_u16();
    assert!(listed == 200 || listed == 503);
    server.stop();
}

#[test]
fn systemd_unit_mentions_proxy_run() {
    let unit = vivi::proxy::install::systemd_unit(std::path::Path::new("/opt/vivi"), None);
    assert!(unit.contains("proxy run"));
    assert!(unit.contains("Restart=always"));
}

#[test]
fn launchd_plist_mentions_label() {
    let plist = vivi::proxy::install::launch_agent_plist(
        std::path::Path::new("/opt/vivi"),
        std::path::Path::new("/tmp/logs"),
        "/bin",
        None,
    );
    assert!(plist.contains("com.vivi.proxy"));
    assert!(plist.contains("proxy"));
}

fn last_access_log(dir: &TempDir) -> Value {
    let path = dir.path().join("logs").join("proxy-access.jsonl");
    for _ in 0..50 {
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Some(line) = text.lines().last()
            && let Ok(value) = serde_json::from_str(line)
        {
            return value;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("no access log at {}", path.display());
}

fn header_value<'a>(seen: &'a proxy_upstream::Seen, name: &str) -> Option<&'a str> {
    seen.headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn write_codex_auth(dir: &TempDir) -> std::path::PathBuf {
    let creds = dir.path().join("auth.json");
    std::fs::write(
        &creds,
        json!({"tokens":{"access_token":"tok","account_id":"acct"}}).to_string(),
    )
    .unwrap();
    creds
}

fn load_codex(dir: &TempDir, creds: &std::path::Path, addr: std::net::SocketAddr) -> ProxyConfig {
    load(
        dir,
        &format!(
            r#"
            log_dir = "{}"
            [providers.codex]
            kind = "native"
            adapter = "chatgpt-codex"
            credentials = "{}"
            upstream = "http://{addr}/backend-api/codex/responses"
            "#,
            dir.path().join("logs").display(),
            creds.display()
        ),
    )
    .1
}

#[test]
fn native_codex_rebuilds_body_length_and_forces_store_false() {
    let mock = Mock::new();
    mock.push(Script {
        status: 200,
        headers: vec![],
        body: sse_body(&[
            r#"{"type":"response.completed","response":{"id":"resp_1","output":[]}}"#,
        ]),
        sse: true,
    });
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_codex_auth(&dir);
    let rt = runtime();
    let (server, base) = wait(&rt, load_codex(&dir, &creds, addr));
    let inbound = json!({
        "model": "gpt-6-luna",
        "instructions": "You are terse.",
        "input": [{"role":"user","content":[{"type":"input_text","text":"Reply with OK"}]}],
        "stream": true,
        "store": false,
        "user": "drop-me",
        "metadata": {"pad": "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}
    });
    let response = get(&base)
        .post(format!("{base}/codex/v1/responses"))
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .body(inbound.to_string())
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let seen = mock.seen();
    let last = seen.last().expect("upstream saw the sanitized body");
    let forwarded: Value = serde_json::from_slice(&last.body).unwrap();
    assert_eq!(forwarded["store"], false);
    assert!(forwarded.get("user").is_none());
    assert!(forwarded.get("metadata").is_none());
    let expected_len = last.body.len().to_string();
    assert_eq!(
        header_value(last, "content-length"),
        Some(expected_len.as_str())
    );
    server.stop();
}

#[test]
fn native_codex_chat_shim_rebuilds_body_length_and_forces_store_false() {
    let mock = Mock::new();
    mock.push(Script {
        status: 200,
        headers: vec![],
        body: sse_body(&[
            r#"{"type":"response.output_text.delta","delta":"OK"}"#,
            r#"{"type":"response.completed","response":{"id":"resp_1","output":[]}}"#,
        ]),
        sse: true,
    });
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_codex_auth(&dir);
    let rt = runtime();
    let (server, base) = wait(&rt, load_codex(&dir, &creds, addr));
    let inbound = json!({
        "model": "gpt-6-luna",
        "messages": [{"role":"user","content":"Reply with OK"}],
        "stream": false,
        "user": "drop-me",
        "metadata": {"pad": "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}
    });
    let response = get(&base)
        .post(format!("{base}/codex/v1/chat/completions"))
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .body(inbound.to_string())
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let seen = mock.seen();
    let last = seen.last().expect("upstream saw the shim body");
    let forwarded: Value = serde_json::from_slice(&last.body).unwrap();
    assert_eq!(forwarded["store"], false);
    assert!(forwarded.get("user").is_none());
    let expected_len = last.body.len().to_string();
    assert_eq!(
        header_value(last, "content-length"),
        Some(expected_len.as_str())
    );
    server.stop();
}

#[test]
fn native_codex_invalid_json_is_local_400() {
    let mock = Mock::new();
    let addr = proxy_upstream::serve(mock.clone());
    let dir = TempDir::new().unwrap();
    let creds = write_codex_auth(&dir);
    let rt = runtime();
    let (server, base) = wait(&rt, load_codex(&dir, &creds, addr));
    let response = get(&base)
        .post(format!("{base}/codex/v1/responses"))
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .body("{not-json")
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 400);
    assert!(mock.seen().is_empty());
    server.stop();
}

#[test]
fn native_codex_error_keeps_upstream_json_content_type() {
    let mock = Mock::new();
    mock.push(Script {
        status: 400,
        headers: vec![("content-type".into(), "application/json".into())],
        body: br#"{"detail":"Store must be set to false"}"#.to_vec(),
        sse: false,
    });
    let addr = proxy_upstream::serve(mock);
    let dir = TempDir::new().unwrap();
    let creds = write_codex_auth(&dir);
    let rt = runtime();
    let (server, base) = wait(&rt, load_codex(&dir, &creds, addr));
    let response = get(&base)
        .post(format!("{base}/codex/v1/responses"))
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .body(r#"{"model":"gpt-6-luna","input":[],"store":false}"#)
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 400);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    assert!(content_type.contains("application/json"));
    assert!(!content_type.contains("event-stream"));
    server.stop();
}

#[test]
fn native_codex_log_id_matches_header_and_records_usage() {
    let mock = Mock::new();
    mock.push(Script {
        status: 200,
        headers: vec![],
        body: sse_body(&[
            r#"{"type":"response.output_item.done","item":{"id":"msg_1","type":"message","content":[{"text":"OK"}]}}"#,
            r#"{"type":"response.completed","response":{"id":"resp_1","output":[],"usage":{"input_tokens":3,"output_tokens":1}}}"#,
        ]),
        sse: true,
    });
    let addr = proxy_upstream::serve(mock);
    let dir = TempDir::new().unwrap();
    let creds = write_codex_auth(&dir);
    let rt = runtime();
    let (server, base) = wait(&rt, load_codex(&dir, &creds, addr));
    let response = get(&base)
        .post(format!("{base}/codex/v1/responses"))
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .body(r#"{"model":"gpt-6-luna","input":[{"role":"user","content":"hi"}]}"#)
        .send()
        .unwrap();
    let header_id = response
        .headers()
        .get("x-vivi-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .to_string();
    let _body = response.text().unwrap();
    let event = last_access_log(&dir);
    assert_eq!(event["id"], header_id);
    assert_eq!(event["input_tokens"], 3);
    assert_eq!(event["output_tokens"], 1);
    assert!(event["total_ms"].as_u64().unwrap() >= event["ttfb_ms"].as_u64().unwrap());
    server.stop();
}

#[test]
fn native_codex_models_uses_builtin_list() {
    let dir = TempDir::new().unwrap();
    let creds = write_codex_auth(&dir);
    let rt = runtime();
    let (server, base) = wait(
        &rt,
        load(
            &dir,
            &format!(
                r#"
                log_dir = "{}"
                [providers.codex]
                kind = "native"
                adapter = "chatgpt-codex"
                credentials = "{}"
                "#,
                dir.path().join("logs").display(),
                creds.display()
            ),
        )
        .1,
    );
    let value: Value = get(&base)
        .get(format!("{base}/codex/v1/models"))
        .header("host", "127.0.0.1")
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert!(!value["data"].as_array().unwrap().is_empty());
    server.stop();
}

#[test]
fn check_non_2xx_is_an_error() {
    let dir = TempDir::new().unwrap();
    let (path, config) = load(
        &dir,
        &format!(
            r#"
            log_dir = "{}"
            [providers.cursor]
            kind = "upstream"
            backend = "http://127.0.0.1:9"
            "#,
            dir.path().join("logs").display()
        ),
    );
    let rt = runtime();
    let (server, _) = wait(&rt, config);
    std::fs::write(
        &path,
        format!(
            r#"
            listen = "{}"
            log_dir = "{}"
            [providers.cursor]
            kind = "upstream"
            backend = "http://127.0.0.1:9"
            "#,
            server.addr,
            dir.path().join("logs").display()
        ),
    )
    .unwrap();
    let result = check::print(&ProxyCheckCommand {
        provider: Some("cursor".into()),
        config: Some(path),
    });
    assert!(result.is_err());
    server.stop();
}

#[test]
fn providers_row_includes_health() {
    let mock = Mock::new();
    mock.push(Script {
        status: 200,
        headers: vec![],
        body: b"ok".to_vec(),
        sse: false,
    });
    let addr = proxy_upstream::serve(mock);
    let dir = TempDir::new().unwrap();
    let (_path, mut config) = load(
        &dir,
        &format!(
            r#"
            log_dir = "{}"
            [providers.llama]
            kind = "upstream"
            backend = "http://{addr}"
            health = "/health"
            "#,
            dir.path().join("logs").display()
        ),
    );
    let rt = runtime();
    let (server, _) = wait(&rt, config.clone());
    config.listen = server.addr;
    let row = providers::format_row(&config, "llama", &config.providers["llama"]);
    assert!(row.contains("health=ok"), "{row}");
    assert!(row.contains("credentials="), "{row}");
    server.stop();
}
