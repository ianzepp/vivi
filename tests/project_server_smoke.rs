//! Smoke coverage for the fake project server in `tests/support/`, driven
//! over loopback HTTP with a real `SyncIdentity` doing the challenge/sign/
//! token dance.

#[path = "support/project_server.rs"]
mod project_server;

use serde_json::{Value, json};
use vivi::b64;
use vivi::sync::identity::SyncIdentity;

use project_server::{HttpReply, ProjectServer, http};

/// A fresh identity, already persisted (the tempdir can go: the key is live
/// in memory once `ensure` returns).
fn identity() -> SyncIdentity {
    let dir = tempfile::tempdir().unwrap();
    SyncIdentity::ensure(&dir.path().join(".vivi")).unwrap()
}

fn header<'a>(reply: &'a HttpReply, name: &str) -> Option<&'a str> {
    reply
        .headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn error_code(reply: &HttpReply) -> String {
    let parsed: Value = serde_json::from_str(&reply.body).unwrap_or(Value::Null);
    parsed["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn call(
    server: &ProjectServer,
    method: &str,
    tail: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> HttpReply {
    let path = format!("/v1/projects/{}/{}", server.project(), tail);
    let mut headers: Vec<(String, String)> = Vec::new();
    if body.is_some() {
        headers.push(("Content-Type".into(), "application/json".into()));
    }
    if let Some(bearer) = bearer {
        headers.push(("Authorization".into(), format!("Bearer {bearer}")));
    }
    let body_text = body.map(|value| value.to_string());
    let header_refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    http(
        &server.url(),
        method,
        &path,
        &header_refs,
        body_text.as_deref(),
    )
}

fn post(server: &ProjectServer, tail: &str, bearer: Option<&str>, body: Value) -> HttpReply {
    call(server, "POST", tail, bearer, Some(body))
}

fn get(server: &ProjectServer, tail: &str, bearer: Option<&str>) -> HttpReply {
    call(server, "GET", tail, bearer, None)
}

fn delete(server: &ProjectServer, tail: &str, bearer: Option<&str>) -> HttpReply {
    call(server, "DELETE", tail, bearer, None)
}

fn mcp(server: &ProjectServer, bearer: &str, message: Value, accept: Option<&str>) -> HttpReply {
    let path = "/v1/mcp".to_string();
    let mut headers: Vec<(String, String)> = vec![
        ("Content-Type".into(), "application/json".into()),
        ("Authorization".into(), format!("Bearer {bearer}")),
    ];
    if let Some(accept) = accept {
        headers.push(("Accept".into(), accept.into()));
    }
    let header_refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    http(
        &server.url(),
        "POST",
        &path,
        &header_refs,
        Some(&message.to_string()),
    )
}

fn ping(id: i64) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "ping" })
}

/// The full dance: challenge, sign the 32 challenge bytes, mint a token.
fn mint_token(server: &ProjectServer, identity: &SyncIdentity, name: &str) -> String {
    let challenge = post(
        server,
        "identities/challenge",
        None,
        json!({ "public_key_pem": identity.public_key_pem() }),
    );
    assert_eq!(
        challenge.status, 200,
        "challenge status {}",
        challenge.status
    );
    let parsed: Value = serde_json::from_str(&challenge.body).unwrap();
    let bytes = b64::decode(parsed["challenge"].as_str().unwrap()).unwrap();
    let minted = post(
        server,
        "auth/token",
        None,
        json!({
            "name": name,
            "challenge_id": parsed["challenge_id"].as_str().unwrap(),
            "signature_base64": b64::encode(&identity.sign(&bytes)),
        }),
    );
    assert_eq!(minted.status, 200, "token status {}", minted.status);
    let parsed: Value = serde_json::from_str(&minted.body).unwrap();
    parsed["token"].as_str().unwrap().to_string()
}

#[test]
fn challenge_sign_token_then_initialize_list_and_echo() {
    let server = ProjectServer::start();
    let identity = identity();
    server.register("alice", &identity.public_key_pem());

    let challenge = post(
        &server,
        "identities/challenge",
        None,
        json!({ "public_key_pem": identity.public_key_pem() }),
    );
    assert_eq!(challenge.status, 200, "status {}", challenge.status);
    let parsed: Value = serde_json::from_str(&challenge.body).unwrap();
    assert!(parsed["challenge_id"].as_str().is_some());
    assert!(parsed["expires_in"].as_i64().unwrap() > 0);
    let bytes = b64::decode(parsed["challenge"].as_str().unwrap()).unwrap();
    assert_eq!(bytes.len(), 32);
    let mut distinct = bytes.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        32,
        "the 32 challenge bytes are pairwise distinct"
    );

    let minted = post(
        &server,
        "auth/token",
        None,
        json!({
            "name": "alice",
            "challenge_id": parsed["challenge_id"].as_str().unwrap(),
            "signature_base64": b64::encode(&identity.sign(&bytes)),
        }),
    );
    assert_eq!(minted.status, 200, "status {}", minted.status);
    let minted_value: Value = serde_json::from_str(&minted.body).unwrap();
    let token = minted_value["token"].as_str().unwrap().to_string();
    let expires_at = minted_value["token_expires_at"].as_str().unwrap();
    assert!(
        expires_at.ends_with('Z') && expires_at.contains('T'),
        "{expires_at}"
    );
    assert_eq!(server.tokens_minted(), 1);
    assert_eq!(
        get(&server, "keys", Some(&token)).status,
        200,
        "the minted token is live on the sync side"
    );

    let key = server.issue_api_key("alice");
    let init = mcp(
        &server,
        &key,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-11-25" },
        }),
        None,
    );
    assert_eq!(init.status, 200);
    assert_eq!(header(&init, "content-type"), Some("application/json"));
    let init_value: Value = serde_json::from_str(&init.body).unwrap();
    assert_eq!(init_value["id"], 1);
    assert_eq!(init_value["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(init_value["result"]["capabilities"]["tools"], json!({}));
    assert_eq!(
        init_value["result"]["serverInfo"]["name"],
        "fake-vivi-server"
    );

    let list = mcp(
        &server,
        &key,
        json!({ "jsonrpc": "2.0", "id": "abc", "method": "tools/list" }),
        None,
    );
    let list_value: Value = serde_json::from_str(&list.body).unwrap();
    assert_eq!(list_value["id"], "abc", "the response id is verbatim");
    assert_eq!(list_value["result"]["tools"][0]["name"], "echo");
    assert_eq!(
        list_value["result"]["tools"][0]["inputSchema"]["type"],
        "object"
    );

    let echo = mcp(
        &server,
        &key,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "echo", "arguments": { "text": "hello" } },
        }),
        None,
    );
    let echo_value: Value = serde_json::from_str(&echo.body).unwrap();
    assert_eq!(echo_value["result"]["content"][0]["type"], "text");
    assert_eq!(echo_value["result"]["content"][0]["text"], "alice: hello");
    assert_eq!(echo_value["result"]["isError"], false);
}

#[test]
fn initialize_echoes_only_supported_protocol_versions() {
    let server = ProjectServer::start();
    let token = server.issue_api_key("alice");

    for version in ["2024-11-05", "2025-03-26", "2025-11-25"] {
        let reply = mcp(
            &server,
            &token,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": { "protocolVersion": version },
            }),
            None,
        );
        let parsed: Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(parsed["result"]["protocolVersion"], version);
    }

    let reply = mcp(
        &server,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "initialize",
            "params": { "protocolVersion": "1999-09-09" },
        }),
        None,
    );
    let parsed: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(parsed["result"]["protocolVersion"], "2025-03-26");
}

#[test]
fn ping_answers_an_empty_result_and_unknown_methods_are_32601() {
    let server = ProjectServer::start();
    let token = server.issue_api_key("alice");

    let pong = mcp(&server, &token, ping(7), None);
    let pong_value: Value = serde_json::from_str(&pong.body).unwrap();
    assert_eq!(pong_value["id"], 7);
    assert_eq!(pong_value["result"], json!({}));

    let unknown = mcp(
        &server,
        &token,
        json!({ "jsonrpc": "2.0", "id": "x", "method": "no/such" }),
        None,
    );
    assert_eq!(unknown.status, 200);
    let unknown_value: Value = serde_json::from_str(&unknown.body).unwrap();
    assert_eq!(unknown_value["error"]["code"], -32601);
    assert_eq!(unknown_value["id"], "x");

    let wrong_tool = mcp(
        &server,
        &token,
        json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": { "name": "shout" } }),
        None,
    );
    assert_eq!(wrong_tool.status, 200);
    let wrong_value: Value = serde_json::from_str(&wrong_tool.body).unwrap();
    assert_eq!(wrong_value["result"]["isError"], true);
    assert!(
        wrong_value["result"]["content"][0]["text"]
            .as_str()
            .is_some()
    );
}

#[test]
fn a_bad_signature_is_unauthorized_and_mints_nothing() {
    let server = ProjectServer::start();
    let identity = identity();
    server.register("alice", &identity.public_key_pem());

    let challenge = post(
        &server,
        "identities/challenge",
        None,
        json!({ "public_key_pem": identity.public_key_pem() }),
    );
    let parsed: Value = serde_json::from_str(&challenge.body).unwrap();
    let minted = post(
        &server,
        "auth/token",
        None,
        json!({
            "name": "alice",
            "challenge_id": parsed["challenge_id"].as_str().unwrap(),
            "signature_base64": b64::encode(&identity.sign(b"not the challenge bytes")),
        }),
    );

    assert_eq!(minted.status, 401);
    assert_eq!(error_code(&minted), "unauthorized");
    assert_eq!(server.tokens_minted(), 0);
}

#[test]
fn a_name_nobody_registered_cannot_mint_a_token() {
    let server = ProjectServer::start();
    let identity = identity();
    server.register("alice", &identity.public_key_pem());

    let challenge = post(
        &server,
        "identities/challenge",
        None,
        json!({ "public_key_pem": identity.public_key_pem() }),
    );
    let parsed: Value = serde_json::from_str(&challenge.body).unwrap();
    let bytes = b64::decode(parsed["challenge"].as_str().unwrap()).unwrap();
    let minted = post(
        &server,
        "auth/token",
        None,
        json!({
            "name": "mallory",
            "challenge_id": parsed["challenge_id"].as_str().unwrap(),
            "signature_base64": b64::encode(&identity.sign(&bytes)),
        }),
    );

    assert_eq!(minted.status, 401);
    assert_eq!(error_code(&minted), "unauthorized");
    assert_eq!(server.tokens_minted(), 0);
}

#[test]
fn a_notification_is_202_with_an_empty_body() {
    let server = ProjectServer::start();
    let token = server.issue_api_key("alice");

    let reply = mcp(
        &server,
        &token,
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        None,
    );

    assert_eq!(reply.status, 202);
    assert!(reply.body.is_empty());
    assert_eq!(server.mcp_requests().len(), 1, "recorded before judgment");
}

#[test]
fn an_event_stream_accept_wraps_the_same_json_in_one_sse_frame() {
    let server = ProjectServer::start();
    let token = server.issue_api_key("alice");

    let plain = mcp(
        &server,
        &token,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        None,
    );
    let sse = mcp(
        &server,
        &token,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        Some("text/event-stream"),
    );

    assert_eq!(header(&sse, "content-type"), Some("text/event-stream"));
    assert!(
        sse.body.starts_with("event: message\ndata: "),
        "{}",
        sse.body
    );
    assert!(sse.body.ends_with("\n\n"));
    let data = sse
        .body
        .strip_prefix("event: message\ndata: ")
        .unwrap()
        .strip_suffix('\n')
        .unwrap()
        .strip_suffix('\n')
        .unwrap();
    let sse_value: Value = serde_json::from_str(data).unwrap();
    let plain_value: Value = serde_json::from_str(&plain.body).unwrap();
    assert_eq!(sse_value, plain_value, "the same JSON object, only framed");
}

#[test]
fn refuse_mcp_next_refuses_once_then_succeeds() {
    let server = ProjectServer::start();
    let token = server.issue_api_key("alice");

    server.refuse_mcp_next(1);
    let first = mcp(&server, &token, ping(1), None);
    assert_eq!(first.status, 401);
    assert_eq!(error_code(&first), "unauthorized");
    let second = mcp(&server, &token, ping(2), None);
    assert_eq!(second.status, 200);

    let seen = server.mcp_requests();
    assert_eq!(seen.len(), 2, "the refused request was recorded too");
    assert_eq!(seen[0].body["method"], "ping");
    assert!(seen[0].accept.is_none());
}

#[test]
fn mcp_requests_record_the_bearer_body_and_accept() {
    let server = ProjectServer::start();
    let token = server.issue_api_key("alice");

    mcp(&server, &token, ping(1), Some("application/json"));

    let seen = server.mcp_requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].bearer, token);
    assert_eq!(seen[0].body["method"], "ping");
    assert_eq!(seen[0].body["id"], 1);
    assert_eq!(seen[0].accept.as_deref(), Some("application/json"));
}

#[test]
fn revoking_all_tokens_disables_every_token() {
    let server = ProjectServer::start();
    let identity = identity();
    server.register("alice", &identity.public_key_pem());
    let token = mint_token(&server, &identity, "alice");
    assert_eq!(get(&server, "keys", Some(&token)).status, 200);

    server.revoke_all_tokens();

    assert_eq!(get(&server, "keys", Some(&token)).status, 401);
}

#[test]
fn api_keys_follow_the_documented_lifecycle() {
    let server = ProjectServer::start();
    let alice = identity();
    let bob = identity();
    server.register("alice", &alice.public_key_pem());
    server.register("bob", &bob.public_key_pem());
    let alice_token = mint_token(&server, &alice, "alice");
    let bob_token = mint_token(&server, &bob, "bob");

    let minted = post(
        &server,
        "keys",
        Some(&alice_token),
        json!({ "label": "ci" }),
    );
    assert_eq!(minted.status, 201, "status {}", minted.status);
    let minted_value: Value = serde_json::from_str(&minted.body).unwrap();
    let key_id = minted_value["key_id"].as_str().unwrap().to_string();
    assert!(key_id.starts_with("key_"));
    let raw_key = minted_value["key"].as_str().unwrap().to_string();
    assert!(raw_key.starts_with("vivi_"));
    assert_eq!(minted_value["label"], "ci");
    assert_eq!(minted_value["created_by"], "alice");
    assert!(minted_value["created_at"].as_str().unwrap().ends_with('Z'));

    let echoed = mcp(
        &server,
        &raw_key,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "echo", "arguments": { "text": "via key" } },
        }),
        None,
    );
    assert_eq!(echoed.status, 200, "an API key works on /v1/mcp");
    let echoed_value: Value = serde_json::from_str(&echoed.body).unwrap();
    assert_eq!(
        echoed_value["result"]["content"][0]["text"],
        "alice: via key"
    );

    let with_key = post(&server, "keys", Some(&raw_key), json!({ "label": "nope" }));
    assert_eq!(with_key.status, 401, "an API key cannot mint keys");
    let listed_with_key = get(&server, "keys", Some(&raw_key));
    assert_eq!(listed_with_key.status, 401, "an API key cannot list keys");

    let listed = get(&server, "keys", Some(&alice_token));
    assert_eq!(listed.status, 200);
    let listed_value: Value = serde_json::from_str(&listed.body).unwrap();
    let entries = listed_value["keys"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["key_id"], key_id.as_str());
    assert_eq!(entries[0]["label"], "ci");
    assert_eq!(entries[0]["created_by"], "alice");
    assert!(entries[0]["created_at"].as_str().is_some());
    assert!(entries[0].get("key").is_none());
    assert!(
        entries[0].get("revoked_at").is_none(),
        "live keys carry none"
    );
    assert!(
        !listed.body.contains("vivi_"),
        "the list never carries a secret"
    );

    let stranger = delete(&server, &format!("keys/{key_id}"), Some(&bob_token));
    assert_eq!(stranger.status, 404);
    assert_eq!(error_code(&stranger), "key_not_found");

    let revoked = delete(&server, &format!("keys/{key_id}"), Some(&alice_token));
    assert_eq!(revoked.status, 204);
    assert!(revoked.body.is_empty());

    let after = mcp(&server, &raw_key, ping(2), None);
    assert_eq!(after.status, 401, "a revoked key stops working");

    let again = delete(&server, &format!("keys/{key_id}"), Some(&alice_token));
    assert_eq!(again.status, 204, "revoking again is idempotent on staging");
    assert!(again.body.is_empty());

    let listed_after = get(&server, "keys", Some(&alice_token));
    assert_eq!(listed_after.status, 200);
    let after_value: Value = serde_json::from_str(&listed_after.body).unwrap();
    let entries = after_value["keys"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "staging keeps revoked keys in the list");
    assert_eq!(entries[0]["key_id"], key_id.as_str());
    let revoked_at = entries[0]["revoked_at"].as_str().unwrap();
    assert!(revoked_at.contains('T'), "an RFC 3339 stamp: {revoked_at}");
    assert!(
        !listed_after.body.contains("vivi_"),
        "even revoked, the list never carries a secret"
    );
}

#[test]
fn a_65_character_label_is_an_invalid_request_and_64_is_fine() {
    let server = ProjectServer::start();
    let identity = identity();
    server.register("alice", &identity.public_key_pem());
    let token = mint_token(&server, &identity, "alice");

    let too_long = post(
        &server,
        "keys",
        Some(&token),
        json!({ "label": "x".repeat(65) }),
    );
    assert_eq!(too_long.status, 400);
    assert_eq!(error_code(&too_long), "invalid_request");

    let empty = post(&server, "keys", Some(&token), json!({ "label": "" }));
    assert_eq!(empty.status, 400);
    assert_eq!(error_code(&empty), "invalid_request");

    let no_json = http(
        &server.url(),
        "POST",
        &format!("/v1/projects/{}/keys", server.project()),
        &[("Authorization", &format!("Bearer {token}"))],
        Some("not json at all"),
    );
    assert_eq!(no_json.status, 400, "a missing/invalid body is 400");
    assert_eq!(error_code(&no_json), "invalid_request");

    let ok = post(
        &server,
        "keys",
        Some(&token),
        json!({ "label": "y".repeat(64) }),
    );
    assert_eq!(ok.status, 201, "status {}", ok.status);
}

#[test]
fn a_custom_lifetime_still_mints_and_negative_days_do_not_panic() {
    let server = ProjectServer::start();
    let identity = identity();
    server.register("alice", &identity.public_key_pem());

    server.set_token_lifetime_secs(1);
    let token = mint_token(&server, &identity, "alice");
    assert_eq!(get(&server, "keys", Some(&token)).status, 200);
    assert_eq!(server.tokens_minted(), 1);

    server.set_token_lifetime_secs(-10);
    let past = mint_token(&server, &identity, "alice");
    assert_eq!(get(&server, "keys", Some(&past)).status, 200);
    assert_eq!(server.tokens_minted(), 2);
}

#[test]
fn another_project_id_is_project_not_found() {
    let server = ProjectServer::start();

    let reply = http(
        &server.url(),
        "GET",
        "/v1/projects/other-id/keys",
        &[],
        None,
    );

    assert_eq!(reply.status, 404);
    assert_eq!(error_code(&reply), "project_not_found");
}

#[test]
fn a_batch_is_answered_by_the_array_of_individual_replies() {
    let server = ProjectServer::start();
    let token = server.issue_api_key("alice");

    let batch = json!([
        ping(7),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": "abc", "method": "tools/list" }),
    ]);
    let reply = mcp(&server, &token, batch, None);
    assert_eq!(reply.status, 200);
    let parsed: Value = serde_json::from_str(&reply.body).unwrap();
    let replies = parsed.as_array().unwrap();
    assert_eq!(replies.len(), 2, "the notification is omitted");
    assert_eq!(replies[0]["id"], 7);
    assert_eq!(replies[0]["result"], json!({}));
    assert_eq!(replies[1]["id"], "abc");
    assert_eq!(replies[1]["result"]["tools"][0]["name"], "echo");

    let only = mcp(
        &server,
        &token,
        json!([{ "jsonrpc": "2.0", "method": "notifications/initialized" }]),
        None,
    );
    assert_eq!(
        only.status, 202,
        "a batch of only notifications has no reply"
    );
    assert!(only.body.is_empty());
}

#[test]
fn a_body_that_is_not_json_is_a_32700_parse_error() {
    let server = ProjectServer::start();
    let token = server.issue_api_key("alice");
    let bearer_header = format!("Bearer {token}");

    let reply = http(
        &server.url(),
        "POST",
        "/v1/mcp",
        &[
            ("Content-Type", "application/json"),
            ("Authorization", bearer_header.as_str()),
        ],
        Some("this is not json"),
    );

    assert_eq!(reply.status, 400);
    let parsed: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(parsed["id"], Value::Null);
    assert_eq!(parsed["error"]["code"], -32700);
    assert_eq!(parsed["error"]["message"], "parse error");
}

#[test]
fn a_mint_reply_is_exactly_token_and_token_expires_at() {
    let server = ProjectServer::start();
    let identity = identity();
    server.register("alice", &identity.public_key_pem());

    let challenge = post(
        &server,
        "identities/challenge",
        None,
        json!({ "public_key_pem": identity.public_key_pem() }),
    );
    assert_eq!(challenge.status, 200);
    let parsed: Value = serde_json::from_str(&challenge.body).unwrap();
    let bytes = b64::decode(parsed["challenge"].as_str().unwrap()).unwrap();
    let minted = post(
        &server,
        "auth/token",
        None,
        json!({
            "name": "alice",
            "challenge_id": parsed["challenge_id"].as_str().unwrap(),
            "signature_base64": b64::encode(&identity.sign(&bytes)),
        }),
    );

    assert_eq!(minted.status, 200);
    let minted_value: Value = serde_json::from_str(&minted.body).unwrap();
    let mut fields: Vec<String> = minted_value.as_object().unwrap().keys().cloned().collect();
    fields.sort();
    assert_eq!(
        fields,
        ["token", "token_expires_at"],
        "the mint reply is exactly these two fields"
    );
    assert_eq!(
        server.requests_seen(),
        2,
        "the counter counts every request"
    );
}
