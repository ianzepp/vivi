//! Smoke coverage for the `/v1/mcp` route of the project-server fake.

#[path = "support/project_server.rs"]
mod project_server;

use serde_json::{Value, json};
use vivi::b64;
use vivi::sync::identity::SyncIdentity;

use project_server::{HttpReply, ProjectServer, http};

fn post(server: &ProjectServer, path: &str, key: &str, body: Value) -> HttpReply {
    let auth = format!("Bearer {key}");
    http(
        &server.url(),
        "POST",
        path,
        &[
            ("Content-Type", "application/json"),
            ("Authorization", &auth),
        ],
        Some(&body.to_string()),
    )
}

fn rpc(server: &ProjectServer, key: &str, body: Value) -> Value {
    let reply = post(server, "/v1/mcp", key, body);
    assert_eq!(reply.status, 200, "{}", reply.body);
    serde_json::from_str(&reply.body).unwrap()
}

fn call(server: &ProjectServer, key: &str, name: &str, arguments: Value) -> Value {
    rpc(
        server,
        key,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        }),
    )
}

fn text(reply: &Value) -> &str {
    reply["result"]["content"][0]["text"].as_str().unwrap()
}

fn setup() -> (ProjectServer, String) {
    let server = ProjectServer::start();
    let key = server.issue_api_key("alice");
    server.seed_item("n1", "a subject", "a body");
    (server, key)
}

#[test]
fn initialize_and_echo_work() {
    let (server, key) = setup();
    let init = rpc(
        &server,
        &key,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2025-03-26" } }),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
    let echo = call(&server, &key, "echo", json!({ "text": "hi" }));
    assert_eq!(echo["result"]["isError"], false);
    assert_eq!(text(&echo), "alice: hi");
}

#[test]
fn tools_list_marks_project_on_show_not_echo() {
    let (server, key) = setup();
    let list = rpc(
        &server,
        &key,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
    );
    let tools = list["result"]["tools"].as_array().unwrap();
    let find = |name: &str| tools.iter().find(|t| t["name"] == name).unwrap();
    let show = find("show");
    assert_eq!(
        show["inputSchema"]["properties"]["project"]["type"],
        "string"
    );
    assert_eq!(show["inputSchema"]["required"], json!(["project"]));
    let echo = find("echo");
    assert!(echo["inputSchema"]["properties"].get("project").is_none());
    assert!(echo["inputSchema"].get("required").is_none());
}

#[test]
fn show_without_project_is_a_tool_error() {
    let (server, key) = setup();
    let reply = call(&server, &key, "show", json!({ "handle": "n1" }));
    assert_eq!(reply["result"]["isError"], true);
    assert_eq!(text(&reply), "project is required");
}

#[test]
fn show_with_other_project_is_a_tool_error() {
    let (server, key) = setup();
    let reply = call(
        &server,
        &key,
        "show",
        json!({ "project": "other", "handle": "n1" }),
    );
    assert_eq!(reply["result"]["isError"], true);
    assert_eq!(text(&reply), "this key has no grant on other");
}

#[test]
fn show_with_the_right_project_returns_the_item() {
    let (server, key) = setup();
    let reply = call(
        &server,
        &key,
        "show",
        json!({ "project": server.project(), "handle": "n1" }),
    );
    assert_eq!(reply["result"]["isError"], false);
    assert!(text(&reply).contains("subject: a subject"));
}

#[test]
fn batch_applies_the_project_rule_per_item() {
    let (server, key) = setup();
    let batch = rpc(
        &server,
        &key,
        json!([
            { "jsonrpc": "2.0", "id": 1, "method": "tools/call",
              "params": { "name": "done", "arguments": { "handle": "n1" } } },
            { "jsonrpc": "2.0", "id": 2, "method": "tools/call",
              "params": { "name": "done",
                          "arguments": { "project": server.project(), "handle": "n1" } } },
        ]),
    );
    assert_eq!(text(&batch[0]), "project is required");
    assert_eq!(batch[1]["result"]["isError"], false);
}

#[test]
fn refuse_mcp_next_applies() {
    let (server, key) = setup();
    server.refuse_mcp_next(1);
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    assert_eq!(post(&server, "/v1/mcp", &key, body.clone()).status, 401);
    assert_eq!(post(&server, "/v1/mcp", &key, body).status, 200);
}

#[test]
fn requests_record_their_path() {
    let (server, key) = setup();
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    post(&server, "/v1/mcp", &key, body);
    let seen = server.mcp_requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/v1/mcp");
}

#[test]
fn the_per_project_mcp_route_is_gone() {
    let (server, key) = setup();
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    let old = format!("/v1/projects/{}/mcp", server.project());
    assert_eq!(post(&server, &old, &key, body).status, 404);
    assert!(server.mcp_requests().is_empty());
}

#[test]
fn a_live_identity_token_is_refused_on_v1_mcp() {
    let (server, key) = setup();
    let dir = tempfile::tempdir().unwrap();
    let identity = SyncIdentity::ensure(&dir.path().join(".vivi")).unwrap();
    server.register("alice", &identity.public_key_pem());
    let challenge = http(
        &server.url(),
        "POST",
        &format!("/v1/projects/{}/identities/challenge", server.project()),
        &[("Content-Type", "application/json")],
        Some(&json!({ "public_key_pem": identity.public_key_pem() }).to_string()),
    );
    let parsed: Value = serde_json::from_str(&challenge.body).unwrap();
    let bytes = b64::decode(parsed["challenge"].as_str().unwrap()).unwrap();
    let minted = http(
        &server.url(),
        "POST",
        &format!("/v1/projects/{}/auth/token", server.project()),
        &[("Content-Type", "application/json")],
        Some(
            &json!({
                "name": "alice",
                "challenge_id": parsed["challenge_id"].as_str().unwrap(),
                "signature_base64": b64::encode(&identity.sign(&bytes)),
            })
            .to_string(),
        ),
    );
    assert_eq!(minted.status, 200, "{}", minted.body);
    let minted: Value = serde_json::from_str(&minted.body).unwrap();
    let token = minted["token"].as_str().unwrap();

    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    let refused = post(&server, "/v1/mcp", token, body.clone());
    assert_eq!(refused.status, 401);
    let refused: Value = serde_json::from_str(&refused.body).unwrap();
    assert_eq!(refused["error"]["code"], "unauthorized");
    assert_eq!(
        refused["error"]["message"],
        "identity tokens are not accepted on /v1/mcp"
    );
    assert_eq!(post(&server, "/v1/mcp", &key, body).status, 200);
}

#[test]
fn get_is_not_found() {
    let (server, key) = setup();
    let auth = format!("Bearer {key}");
    let reply = http(
        &server.url(),
        "GET",
        "/v1/mcp",
        &[("Authorization", &auth)],
        None,
    );
    assert_eq!(reply.status, 404);
}
