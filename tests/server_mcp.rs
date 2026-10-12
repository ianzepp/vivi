//! MCP streamable HTTP and project-scoped API keys.

use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::util::ServiceExt;
use vivi_server::api_router;

fn store_key() -> String {
    STANDARD.encode([0x42u8; 32])
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vivi-server-mcp-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn spki_pem(signing: &SigningKey) -> String {
    const PREFIX: &[u8] = &[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut der = PREFIX.to_vec();
    der.extend_from_slice(signing.verifying_key().as_bytes());
    format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
        STANDARD.encode(der)
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn mailspace_for(name: &str) -> String {
    let hex = sha256_hex(name.as_bytes());
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn prefix_for(name: &str) -> String {
    mailspace_for(name)[..8].to_owned()
}

fn address(name: &str) -> String {
    format!("{name}@{}.local", prefix_for(name))
}

fn traveling(n: u64) -> String {
    format!("msg_{n:08x}{:016x}", 0)
}

fn eml(id: &str, body: &str) -> Vec<u8> {
    format!("From: a@p.local\nTo: b@p.local\nMessage-ID: <{id}@p.local>\n\n{body}").into_bytes()
}

fn vouch_letter(id: &str, name: &str, signing_key: &SigningKey) -> Vec<u8> {
    let body = json!({"vouch": {"name": name, "public_key_pem": spki_pem(signing_key)}});
    eml(id, &body.to_string())
}

async fn raw(
    app: &Router,
    method: &str,
    uri: &str,
    body: Vec<u8>,
    token: Option<&str>,
    extra: &[(&str, &str)],
) -> (StatusCode, Value, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    for (name, value) in extra {
        builder = builder.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value, text)
}

async fn send_json(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let bytes = body
        .as_ref()
        .map(|v| v.to_string().into_bytes())
        .unwrap_or_default();
    let mut extra = Vec::new();
    if body.is_some() {
        extra.push(("content-type", "application/json"));
    }
    let (status, value, _) = raw(app, method, uri, bytes, token, &extra).await;
    (status, value)
}

struct Fixture {
    app: Router,
    project: String,
    token: String,
}

async fn fixture(name: &str) -> Fixture {
    let dir = fresh_dir(name);
    let app = api_router(dir, Some(store_key()), None);
    let creator_key = key(1);
    let (status, project) = send_json(
        &app,
        "POST",
        "/v1/projects",
        Some(json!({"public_key_pem": spki_pem(&creator_key), "creator_name": "creator"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{project}");
    let project = project["project_id"].as_str().unwrap().to_owned();
    let (challenge_id, bytes) = {
        let (status, challenge) = send_json(
            &app,
            "POST",
            &format!("/v1/projects/{project}/identities/challenge"),
            Some(json!({"public_key_pem": spki_pem(&creator_key)})),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{challenge}");
        let bytes = STANDARD
            .decode(challenge["challenge"].as_str().unwrap())
            .unwrap();
        (
            challenge["challenge_id"].as_str().unwrap().to_owned(),
            bytes,
        )
    };
    let signature = STANDARD.encode(creator_key.sign(&bytes).to_bytes());
    let (status, identity) = send_json(
        &app,
        "POST",
        &format!("/v1/projects/{project}/identities"),
        Some(json!({
            "name": "creator",
            "public_key_pem": spki_pem(&creator_key),
            "challenge_id": challenge_id,
            "signature_base64": signature,
            "mailspace_id": mailspace_for("creator"),
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{identity}");
    Fixture {
        app,
        project,
        token: identity["token"].as_str().unwrap().to_owned(),
    }
}

impl Fixture {
    async fn add_member(&self, name: &str, seed: u8) -> String {
        let signing = key(seed);
        let raw_vouch = vouch_letter(&traveling(90 + u64::from(seed)), name, &signing);
        let (status, vouched) = raw(
            &self.app,
            "PUT",
            &format!(
                "/v1/projects/{}/items/{}",
                self.project,
                traveling(90 + u64::from(seed))
            ),
            raw_vouch,
            Some(&self.token),
            &[
                ("content-type", "message/rfc822"),
                ("x-vivi-to", "postmaster"),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{vouched}");
        let (status, challenge) = send_json(
            &self.app,
            "POST",
            &format!("/v1/projects/{}/identities/challenge", self.project),
            Some(json!({"public_key_pem": spki_pem(&signing)})),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{challenge}");
        let bytes = STANDARD
            .decode(challenge["challenge"].as_str().unwrap())
            .unwrap();
        let signature = STANDARD.encode(signing.sign(&bytes).to_bytes());
        let (status, identity) = send_json(
            &self.app,
            "POST",
            &format!("/v1/projects/{}/identities", self.project),
            Some(json!({
                "name": name,
                "public_key_pem": spki_pem(&signing),
                "challenge_id": challenge["challenge_id"],
                "signature_base64": signature,
                "mailspace_id": mailspace_for(name),
            })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{identity}");
        identity["token"].as_str().unwrap().to_owned()
    }

    async fn rpc(&self, token: &str, method: &str, params: Value) -> (StatusCode, Value) {
        let (status, value, _) = raw(
            &self.app,
            "POST",
            &format!("/v1/projects/{}/mcp", self.project),
            json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
                .to_string()
                .into_bytes(),
            Some(token),
            &[("content-type", "application/json")],
        )
        .await;
        (status, value)
    }

    async fn call(&self, token: &str, name: &str, arguments: Value) -> Value {
        let (status, value) = self
            .rpc(
                token,
                "tools/call",
                json!({"name": name, "arguments": arguments}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["error"], Value::Null, "{value}");
        let text = value["result"]["content"][0]["text"]
            .as_str()
            .expect("tool text");
        let parsed: Value = serde_json::from_str(text).unwrap_or_else(|_| json!({"raw": text}));
        if value["result"]["isError"].as_bool() == Some(true) {
            panic!("tool error: {parsed}");
        }
        parsed
    }

    async fn call_err(&self, token: &str, name: &str, arguments: Value) -> String {
        let (status, value) = self
            .rpc(
                token,
                "tools/call",
                json!({"name": name, "arguments": arguments}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["result"]["isError"], true, "{value}");
        value["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

#[tokio::test]
async fn mcp_requires_a_bearer() {
    let fx = fixture("auth").await;
    let (status, value, _) = raw(
        &fx.app,
        "POST",
        &format!("/v1/projects/{}/mcp", fx.project),
        json!({"jsonrpc":"2.0","id":1,"method":"ping"})
            .to_string()
            .into_bytes(),
        None,
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
    assert_eq!(value["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn initialize_and_tools_list() {
    let fx = fixture("init").await;
    let (status, init) = fx
        .rpc(
            &fx.token,
            "initialize",
            json!({"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{init}");
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(init["result"]["serverInfo"]["name"], "vivi-server");
    let (status, listed) = fx.rpc(&fx.token, "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let names: Vec<_> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        names,
        vec![
            "overview", "list", "show", "thread", "send", "reply", "done", "reopen", "memo",
            "graph"
        ]
    );
}

#[tokio::test]
async fn send_list_show_thread_done_graph() {
    let fx = fixture("loop").await;
    let to = address("creator");
    let sent = fx
        .call(
            &fx.token,
            "send",
            json!({
                "kind": "need",
                "to": to,
                "subject": "ship MCP",
                "body": "done_when: the connector works"
            }),
        )
        .await;
    let handle = sent["handle"].as_str().unwrap().to_owned();
    assert_eq!(handle.len(), 8);

    let listed = fx
        .call(&fx.token, "list", json!({"kind": "need", "status": "open"}))
        .await;
    assert_eq!(listed["items"][0]["handle"], handle);
    assert_eq!(listed["items"][0]["subject"], "ship MCP");

    let shown = fx.call(&fx.token, "show", json!({"handle": handle})).await;
    assert!(
        shown["text"]
            .as_str()
            .unwrap()
            .contains("done_when: the connector works"),
        "{shown}"
    );

    let replied = fx
        .call(
            &fx.token,
            "reply",
            json!({"handle": handle, "body": "working on it"}),
        )
        .await;
    assert!(replied["handle"].as_str().is_some());

    let thread = fx
        .call(&fx.token, "thread", json!({"handle": handle}))
        .await;
    assert!(
        thread["messages"].as_array().unwrap().len() >= 2,
        "{thread}"
    );

    let overview = fx.call(&fx.token, "overview", json!({})).await;
    assert_eq!(overview["you"]["name"], "creator");
    assert_eq!(overview["totals"]["needs_open"], 1);

    let ready = fx.call(&fx.token, "graph", json!({"view": "ready"})).await;
    assert_eq!(ready["ready"][0]["handle"], handle);

    let closed = fx
        .call(
            &fx.token,
            "done",
            json!({"handle": handle, "note": "shipped"}),
        )
        .await;
    assert_eq!(closed["mode"], "closed", "{closed}");

    let after = fx
        .call(&fx.token, "list", json!({"kind": "need", "status": "open"}))
        .await;
    assert!(after["items"].as_array().unwrap().is_empty(), "{after}");

    let reopened = fx
        .call(&fx.token, "reopen", json!({"handle": handle}))
        .await;
    assert_eq!(reopened["mode"], "reopened", "{reopened}");
    let open_again = fx
        .call(&fx.token, "list", json!({"kind": "need", "status": "open"}))
        .await;
    assert_eq!(open_again["items"][0]["handle"], handle);
}

#[tokio::test]
async fn hand_done_is_a_report_not_a_close() {
    let fx = fixture("report").await;
    let hand = fx.add_member("hand", 2).await;
    let sent = fx
        .call(
            &fx.token,
            "send",
            json!({
                "kind": "task",
                "to": address("hand"),
                "subject": "implement",
                "body": "write the handler"
            }),
        )
        .await;
    let handle = sent["handle"].as_str().unwrap().to_owned();
    let reported = fx
        .call(
            &hand,
            "done",
            json!({"handle": handle, "note": "done on my side"}),
        )
        .await;
    assert_eq!(reported["mode"], "reported", "{reported}");
    let listed = fx
        .call(&fx.token, "list", json!({"kind": "task", "status": "open"}))
        .await;
    assert_eq!(listed["items"][0]["handle"], handle, "{listed}");
}

#[tokio::test]
async fn memo_is_caller_local_on_the_server() {
    let fx = fixture("memo").await;
    let saved = fx
        .call(
            &fx.token,
            "memo",
            json!({"action": "save", "subject": "note", "body": "remember the prefix"}),
        )
        .await;
    let handle = saved["handle"].as_str().unwrap().to_owned();
    let listed = fx.call(&fx.token, "memo", json!({"action": "list"})).await;
    assert_eq!(listed["items"][0]["handle"], handle);
    let shown = fx
        .call(
            &fx.token,
            "memo",
            json!({"action": "show", "handle": handle}),
        )
        .await;
    assert!(
        shown["body"]
            .as_str()
            .unwrap()
            .contains("remember the prefix"),
        "{shown}"
    );
}

#[tokio::test]
async fn graph_blocks_on_an_open_dependency() {
    let fx = fixture("deps").await;
    let need = fx
        .call(
            &fx.token,
            "send",
            json!({
                "kind": "need",
                "to": address("creator"),
                "subject": "parent",
                "body": "must land first"
            }),
        )
        .await;
    let parent = need["handle"].as_str().unwrap().to_owned();
    let task = fx
        .call(
            &fx.token,
            "send",
            json!({
                "kind": "task",
                "to": address("creator"),
                "subject": "child",
                "body": "after parent",
                "depends_on": parent
            }),
        )
        .await;
    let child = task["handle"].as_str().unwrap().to_owned();
    let graph = fx.call(&fx.token, "graph", json!({"view": "status"})).await;
    let blocked: Vec<_> = graph["blocked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["handle"].as_str().unwrap().to_owned())
        .collect();
    let ready: Vec<_> = graph["ready"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["handle"].as_str().unwrap().to_owned())
        .collect();
    assert!(ready.contains(&parent), "{graph}");
    assert!(blocked.contains(&child), "{graph}");
}

#[tokio::test]
async fn api_key_mints_lists_uses_and_revokes() {
    let fx = fixture("keys").await;
    let (status, minted) = send_json(
        &fx.app,
        "POST",
        &format!("/v1/projects/{}/keys", fx.project),
        Some(json!({"label": "grok bot"})),
        Some(&fx.token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{minted}");
    let key = minted["key"].as_str().unwrap().to_owned();
    assert!(key.starts_with("vivi_"), "{key}");
    let key_id = minted["key_id"].as_str().unwrap().to_owned();

    let overview = fx.call(&key, "overview", json!({})).await;
    assert_eq!(overview["you"]["name"], "creator");

    let (status, listed) = send_json(
        &fx.app,
        "GET",
        &format!("/v1/projects/{}/keys", fx.project),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["keys"][0]["label"], "grok bot");
    assert!(listed["keys"][0].get("key").is_none());
    assert!(listed["keys"][0].get("digest").is_none());

    let (status, denied) = send_json(
        &fx.app,
        "POST",
        &format!("/v1/projects/{}/keys", fx.project),
        Some(json!({"label": "nested"})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{denied}");

    let (status, _) = send_json(
        &fx.app,
        "DELETE",
        &format!("/v1/projects/{}/keys/{key_id}", fx.project),
        None,
        Some(&fx.token),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, rejected) = fx.rpc(&key, "ping", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{rejected}");
}

#[tokio::test]
async fn unknown_tool_is_a_tool_error() {
    let fx = fixture("unknown").await;
    let err = fx.call_err(&fx.token, "explode", json!({})).await;
    assert!(err.contains("unknown tool"), "{err}");
}

#[tokio::test]
async fn mail_list_hides_other_members_mail() {
    let fx = fixture("vis").await;
    let hand = fx.add_member("hand", 3).await;
    fx.call(
        &fx.token,
        "send",
        json!({
            "kind": "mail",
            "to": address("creator"),
            "subject": "private",
            "body": "only creator"
        }),
    )
    .await;
    let listed = fx
        .call(&hand, "list", json!({"kind": "mail", "status": "all"}))
        .await;
    assert!(listed["items"].as_array().unwrap().is_empty(), "{listed}");
}
