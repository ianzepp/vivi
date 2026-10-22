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
use vivi_server::projects::{AppendItem, ProjectStore};

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
    /// The data directory the app serves, for a test that fills a project
    /// straight through a [`ProjectStore`] on the same files.
    dir: PathBuf,
    project: String,
    token: String,
}

async fn fixture(name: &str) -> Fixture {
    let dir = fresh_dir(name);
    let app = api_router(dir.clone(), Some(store_key()), None);
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
        dir,
        project,
        token: identity["token"].as_str().unwrap().to_owned(),
    }
}

impl Fixture {
    async fn add_member(&self, name: &str, seed: u8) -> String {
        let signing = key(seed);
        let raw_vouch = vouch_letter(&traveling(90 + u64::from(seed)), name, &signing);
        let (status, vouched, _) = raw(
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
            "graph", "mark", "unmark", "board"
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
    let subjects: Vec<_> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["subject"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(
        !subjects.iter().any(|s| s == "private"),
        "hand must not see creator's self-mail: {listed}"
    );
}

// ---------------------------------------------------------------------------
// Item events: letters are folded by the client's rule, never listed as work.
// ---------------------------------------------------------------------------

/// The `Message-ID` an MCP-composed letter carries for traveling id `id`
/// written by `name`.
fn rfc_id(id: &str, name: &str) -> String {
    format!("{id}@{}.local", prefix_for(name))
}

/// An item event letter as a client writes it: a reply to `of` carrying
/// `X-Vivi-Event: <kind>`, addressed to the item's author.
fn event_letter(
    from: &str,
    id: &str,
    of: &str,
    to: &str,
    kind: &str,
    extra: &[(&str, &str)],
) -> Vec<u8> {
    let mut text = format!(
        "From: {}\nTo: {to}\nSubject: Re: item\nMessage-ID: <{}>\nIn-Reply-To: <{of}>\n\
         X-Vivi-Kind: mail\nX-Vivi-Event: {kind}\nX-Vivi-Event-Of: <{of}>\n",
        address(from),
        rfc_id(id, from),
    );
    for (name, value) in extra {
        text.push_str(&format!("{name}: {value}\n"));
    }
    text.push_str("\nnote\n");
    text.into_bytes()
}

fn stamp(offset_seconds: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(offset_seconds))
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

impl Fixture {
    /// PUT a letter under traveling id number `n` as `token`'s member, with
    /// envelope `kind`. Returns the stored message id.
    async fn put_letter(&self, token: &str, n: u64, eml: Vec<u8>, kind: &str, to: &str) -> String {
        let id = traveling(n);
        let (status, body, _) = raw(
            &self.app,
            "PUT",
            &format!("/v1/projects/{}/items/{id}", self.project),
            eml,
            Some(token),
            &[
                ("content-type", "message/rfc822"),
                ("x-vivi-kind", kind),
                ("x-vivi-to", to),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        id
    }

    /// The raw `.eml` of a stored letter.
    async fn raw_letter(&self, token: &str, message_id: &str) -> String {
        let (status, _, text) = raw(
            &self.app,
            "GET",
            &format!("/v1/projects/{}/items/{message_id}", self.project),
            Vec::new(),
            Some(token),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        text
    }

    /// Send a need or task from `token`'s member to `to` and return
    /// `(handle, message_id)`.
    async fn send_item(
        &self,
        token: &str,
        kind: &str,
        to: &str,
        subject: &str,
    ) -> (String, String) {
        let sent = self
            .call(
                token,
                "send",
                json!({"kind": kind, "to": to, "subject": subject, "body": "do the thing"}),
            )
            .await;
        (
            sent["handle"].as_str().unwrap().to_owned(),
            sent["message_id"].as_str().unwrap().to_owned(),
        )
    }
}

fn handles(listed: &Value) -> Vec<String> {
    listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["handle"].as_str().unwrap().to_owned())
        .collect()
}

/// Event letters are never work, whatever envelope kind they travel under,
/// for every member that holds the item: `envelope` is `mail` as the client
/// sends them now, or the item's own kind as an older one did. `scout` is a
/// member that is neither the sender nor an addressee of either event.
async fn event_letters_are_not_work(name: &str, envelope: &str) {
    let fx = fixture(name).await;
    let hand = fx.add_member("hand", 2).await;
    let scout = fx.add_member("scout", 4).await;
    let (open_handle, open_id) = fx
        .send_item(&fx.token, "need", &address("hand"), "stays open")
        .await;
    let (_, closed_id) = fx
        .send_item(&fx.token, "need", &address("hand"), "gets closed")
        .await;
    let reopened = fx
        .put_letter(
            &hand,
            301,
            event_letter(
                "hand",
                &traveling(301),
                &rfc_id(&open_id, "creator"),
                &address("creator"),
                "reopened",
                &[("X-Vivi-Event-At", &stamp(0))],
            ),
            envelope,
            &address("creator"),
        )
        .await;
    fx.put_letter(
        &hand,
        302,
        event_letter(
            "hand",
            &traveling(302),
            &rfc_id(&closed_id, "creator"),
            &address("creator"),
            "done",
            &[("X-Vivi-Event-At", &stamp(0))],
        ),
        envelope,
        &address("creator"),
    )
    .await;

    for viewer in [&fx.token, &hand, &scout] {
        let all = fx
            .call(viewer, "list", json!({"status": "all", "limit": 100}))
            .await;
        assert_eq!(all["items"].as_array().unwrap().len(), 2, "{all}");
        let needs = fx
            .call(viewer, "list", json!({"kind": "need", "status": "all"}))
            .await;
        assert_eq!(needs["items"].as_array().unwrap().len(), 2, "{needs}");
        // Not as mail either: a mail-kind event is still no letter to list.
        let mail = fx
            .call(viewer, "list", json!({"kind": "mail", "status": "all"}))
            .await;
        assert!(
            mail["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["kind"] != "event"),
            "{mail}"
        );
        let open = fx.call(viewer, "list", json!({"status": "open"})).await;
        assert_eq!(handles(&open), vec![open_handle.clone()], "{open}");
        let overview = fx.call(viewer, "overview", json!({})).await;
        assert_eq!(overview["totals"]["needs_open"], 1, "{overview}");
        assert_eq!(overview["totals"]["ready"], 1, "{overview}");
        // The only mail is the two vouch letters to postmaster.
        assert_eq!(overview["totals"]["mail_visible"], 2, "{overview}");
        assert_eq!(
            overview["ready"],
            json!([open_handle.clone()]),
            "{overview}"
        );
        let recent = overview["recent"].as_array().unwrap();
        assert_eq!(recent.len(), 4, "two needs and two vouches: {overview}");
        assert!(
            recent.iter().all(|item| item["kind"] != "event"),
            "no event letter in recent: {overview}"
        );
        let graph = fx.call(viewer, "graph", json!({"view": "status"})).await;
        assert_eq!(graph["totals"]["ready"], 1, "{graph}");
        assert_eq!(graph["totals"]["done"], 1, "{graph}");
        assert_eq!(graph["totals"]["blocked"], 0, "{graph}");
    }

    // An event letter cannot be settled as if it were an item.
    let event_handle = reopened[4..12].to_owned();
    let err = fx
        .call_err(&fx.token, "done", json!({"handle": event_handle}))
        .await;
    assert!(err.contains("event letter"), "{err}");
    let err = fx
        .call_err(&fx.token, "reopen", json!({"handle": event_handle}))
        .await;
    assert!(err.contains("event letter"), "{err}");

    // `show` and `thread` name it for what it is, to a bystander as well.
    for viewer in [&fx.token, &scout] {
        let shown = fx
            .call(viewer, "show", json!({"handle": event_handle}))
            .await;
        assert_eq!(shown["item"]["kind"], "event", "{shown}");
        assert_eq!(shown["item"]["status"], "event", "{shown}");
    }
}

#[tokio::test]
async fn a_mail_kind_event_letter_is_absent_from_list_overview_and_ready() {
    event_letters_are_not_work("event-not-work-mail", "mail").await;
}

#[tokio::test]
async fn an_item_kind_event_letter_is_absent_from_list_overview_and_ready() {
    event_letters_are_not_work("event-not-work-item", "need").await;
}

#[tokio::test]
async fn a_cross_member_close_folds_to_done() {
    let fx = fixture("cross-close").await;
    let hand = fx.add_member("hand", 2).await;
    let (handle, id) = fx
        .send_item(&fx.token, "task", &address("hand"), "cross close")
        .await;
    // hand does not hold the item, and the author is a different mailspace:
    // under the old author-domain rule this event did not count.
    fx.put_letter(
        &hand,
        310,
        event_letter(
            "hand",
            &traveling(310),
            &rfc_id(&id, "creator"),
            &address("creator"),
            "done",
            &[("X-Vivi-Event-At", &stamp(0))],
        ),
        "task",
        &address("creator"),
    )
    .await;
    for viewer in [&fx.token, &hand] {
        let done = fx
            .call(viewer, "list", json!({"kind": "task", "status": "done"}))
            .await;
        assert_eq!(handles(&done), vec![handle.clone()], "{done}");
        let open = fx
            .call(viewer, "list", json!({"kind": "task", "status": "open"}))
            .await;
        assert!(open["items"].as_array().unwrap().is_empty(), "{open}");
        let shown = fx.call(viewer, "show", json!({"handle": handle})).await;
        assert_eq!(shown["item"]["status"], "done", "{shown}");
    }
}

#[tokio::test]
async fn two_conflicting_events_fold_to_the_newer() {
    let fx = fixture("conflict").await;
    let hand = fx.add_member("hand", 2).await;
    let (handle, id) = fx
        .send_item(&fx.token, "need", &address("hand"), "conflict")
        .await;
    let of = rfc_id(&id, "creator");
    // The reopen is stamped later but appended first; the older close is
    // appended last. Append order is not the clock.
    fx.put_letter(
        &hand,
        320,
        event_letter(
            "hand",
            &traveling(320),
            &of,
            &address("creator"),
            "reopened",
            &[("X-Vivi-Event-At", &stamp(20))],
        ),
        "need",
        &address("creator"),
    )
    .await;
    fx.put_letter(
        &fx.token,
        321,
        event_letter(
            "creator",
            &traveling(321),
            &of,
            &address("hand"),
            "done",
            &[("X-Vivi-Event-At", &stamp(10))],
        ),
        "need",
        &address("hand"),
    )
    .await;
    let open = fx
        .call(&fx.token, "list", json!({"kind": "need", "status": "open"}))
        .await;
    assert_eq!(handles(&open), vec![handle.clone()], "{open}");

    // Now a newer close arrives last and wins.
    fx.put_letter(
        &hand,
        322,
        event_letter(
            "hand",
            &traveling(322),
            &of,
            &address("creator"),
            "done",
            &[("X-Vivi-Event-At", &stamp(30))],
        ),
        "need",
        &address("creator"),
    )
    .await;
    let done = fx
        .call(&fx.token, "list", json!({"kind": "need", "status": "done"}))
        .await;
    assert_eq!(handles(&done), [handle], "{done}");
}

#[tokio::test]
async fn a_far_future_at_cannot_lock_an_item() {
    let fx = fixture("far-future").await;
    let hand = fx.add_member("hand", 2).await;
    let (handle, id) = fx
        .send_item(&fx.token, "need", &address("hand"), "locked?")
        .await;
    let of = rfc_id(&id, "creator");
    fx.put_letter(
        &hand,
        330,
        event_letter(
            "hand",
            &traveling(330),
            &of,
            &address("creator"),
            "done",
            &[("X-Vivi-Event-At", "2099-01-01T00:00:00.000000Z")],
        ),
        "mail",
        &address("creator"),
    )
    .await;
    // The close is dated past the clock: it is not counted at all.
    let done = fx
        .call(&fx.token, "list", json!({"kind": "need", "status": "done"}))
        .await;
    assert!(done["items"].as_array().unwrap().is_empty(), "{done}");
    let open = fx
        .call(&fx.token, "list", json!({"kind": "need", "status": "open"}))
        .await;
    assert_eq!(handles(&open), vec![handle.clone()], "{open}");

    // The author closes and reopens through MCP. The stamps are the clock,
    // not 2099, and each outranks the letter that is not counted.
    let closed = fx.call(&fx.token, "done", json!({"handle": handle})).await;
    assert_eq!(closed["mode"], "closed", "{closed}");
    let reopened = fx
        .call(&fx.token, "reopen", json!({"handle": handle}))
        .await;
    assert_eq!(reopened["mode"], "reopened", "{reopened}");
    for event in [&closed, &reopened] {
        let letter = fx
            .raw_letter(&fx.token, event["message_id"].as_str().unwrap())
            .await;
        let at = letter
            .lines()
            .find_map(|line| line.strip_prefix("X-Vivi-Event-At: "))
            .expect("event stamp");
        assert!(at.starts_with("20") && !at.starts_with("2099"), "{at}");
    }
    let open = fx
        .call(&fx.token, "list", json!({"kind": "need", "status": "open"}))
        .await;
    assert_eq!(handles(&open), [handle], "{open}");
}

#[tokio::test]
async fn an_event_eleven_minutes_ahead_is_not_counted_and_nine_is() {
    let fx = fixture("ten-minutes").await;
    let hand = fx.add_member("hand", 2).await;
    let (nine_handle, nine_id) = fx
        .send_item(&fx.token, "need", &address("hand"), "nine ahead")
        .await;
    let (eleven_handle, eleven_id) = fx
        .send_item(&fx.token, "need", &address("hand"), "eleven ahead")
        .await;
    for (n, id, minutes) in [(360, &nine_id, 9), (361, &eleven_id, 11)] {
        fx.put_letter(
            &hand,
            n,
            event_letter(
                "hand",
                &traveling(n),
                &rfc_id(id, "creator"),
                &address("creator"),
                "done",
                &[("X-Vivi-Event-At", &stamp(minutes * 60))],
            ),
            "mail",
            &address("creator"),
        )
        .await;
    }
    for viewer in [&fx.token, &hand] {
        let done = fx
            .call(viewer, "list", json!({"kind": "need", "status": "done"}))
            .await;
        assert_eq!(handles(&done), vec![nine_handle.clone()], "{done}");
        let open = fx
            .call(viewer, "list", json!({"kind": "need", "status": "open"}))
            .await;
        assert_eq!(handles(&open), vec![eleven_handle.clone()], "{open}");
    }
}

#[tokio::test]
async fn a_causal_chain_of_equal_instants_folds_by_depth_not_by_message_id() {
    let fx = fixture("equal-instants").await;
    let hand = fx.add_member("hand", 2).await;
    let (handle, id) = fx
        .send_item(&fx.token, "need", &address("hand"), "tie")
        .await;
    let of = rfc_id(&id, "creator");
    let at = stamp(-30);
    // done (361) -> reopened (363, the largest id) -> done (362): the chain
    // ends on a close with every instant equal, so the deepest event wins
    // whatever its id; the largest id alone would say reopened. Appended
    // deepest-first.
    let first = rfc_id(&traveling(361), "hand");
    let middle = rfc_id(&traveling(363), "creator");
    fx.put_letter(
        &hand,
        362,
        event_letter(
            "hand",
            &traveling(362),
            &of,
            &address("creator"),
            "done",
            &[
                ("X-Vivi-Event-At", &at),
                ("X-Vivi-Event-Prev", &format!("<{middle}>")),
            ],
        ),
        "mail",
        &address("creator"),
    )
    .await;
    fx.put_letter(
        &fx.token,
        363,
        event_letter(
            "creator",
            &traveling(363),
            &of,
            &address("hand"),
            "reopened",
            &[
                ("X-Vivi-Event-At", &at),
                ("X-Vivi-Event-Prev", &format!("<{first}>")),
            ],
        ),
        "mail",
        &address("hand"),
    )
    .await;
    fx.put_letter(
        &hand,
        361,
        event_letter(
            "hand",
            &traveling(361),
            &of,
            &address("creator"),
            "done",
            &[("X-Vivi-Event-At", &at)],
        ),
        "mail",
        &address("creator"),
    )
    .await;
    for viewer in [&fx.token, &hand] {
        let done = fx
            .call(viewer, "list", json!({"kind": "need", "status": "done"}))
            .await;
        assert_eq!(handles(&done), vec![handle.clone()], "{done}");
    }
}

#[tokio::test]
async fn mcp_events_are_stamped_above_the_newest_and_travel_as_mail() {
    let fx = fixture("event-letters").await;
    let hand = fx.add_member("hand", 2).await;
    let scout = fx.add_member("scout", 4).await;
    let (handle, id) = fx
        .send_item(&fx.token, "task", &address("hand"), "write events")
        .await;
    let of = rfc_id(&id, "creator");
    // hand's close is stamped two minutes ahead (counted: inside the bound).
    let ahead = chrono::Utc::now() + chrono::Duration::seconds(120);
    fx.put_letter(
        &hand,
        340,
        event_letter(
            "hand",
            &traveling(340),
            &of,
            &address("creator"),
            "done",
            &[(
                "X-Vivi-Event-At",
                &ahead.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            )],
        ),
        "mail",
        &address("creator"),
    )
    .await;
    let reopened = fx
        .call(&fx.token, "reopen", json!({"handle": handle}))
        .await;
    assert_eq!(reopened["mode"], "reopened", "{reopened}");
    let event_id = reopened["message_id"].as_str().unwrap();
    let letter = fx.raw_letter(&fx.token, event_id).await;
    let header = |name: &str| {
        letter
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}: ")))
            .map(str::to_owned)
    };
    assert_eq!(header("X-Vivi-Event").as_deref(), Some("reopened"));
    assert_eq!(header("X-Vivi-Kind").as_deref(), Some("mail"));
    assert_eq!(
        header("X-Vivi-Event-Of").as_deref(),
        Some(format!("<{of}>").as_str())
    );
    assert_eq!(
        header("In-Reply-To").as_deref(),
        Some(format!("<{of}>").as_str())
    );
    assert_eq!(
        header("X-Vivi-Event-Prev").as_deref(),
        Some(format!("<{}>", rfc_id(&traveling(340), "hand")).as_str()),
        "names the head it follows"
    );
    let at = chrono::DateTime::parse_from_rfc3339(&header("X-Vivi-Event-At").unwrap()).unwrap();
    assert!(
        at.timestamp_micros() > ahead.timestamp_micros(),
        "{at} <= {ahead}"
    );
    // Addressed to the item's recipient, not just to the writer.
    assert!(header("To").unwrap().contains(&address("hand")), "{letter}");

    // It travels as mail, not under the item's kind, and every member is
    // still served it: the addressee, and a member that is neither sender
    // nor addressee.
    for viewer in [&hand, &scout] {
        let (status, page) = send_json(
            &fx.app,
            "GET",
            &format!("/v1/projects/{}/items?since=0", fx.project),
            None,
            Some(viewer),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let record = page["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["message_id"] == event_id)
            .expect("the member is served the event");
        assert_eq!(record["kind"], Value::Null, "{record}");
        let shown = fx.call(viewer, "show", json!({"handle": handle})).await;
        assert_eq!(shown["item"]["status"], "open", "{shown}");
    }
}

/// The `From` header of a stored letter, with its angle brackets and name
/// part removed.
fn from_address(letter: &str) -> String {
    let from = letter
        .lines()
        .find_map(|line| line.strip_prefix("From: "))
        .expect("a From header");
    match (from.find('<'), from.find('>')) {
        (Some(start), Some(end)) if start < end => from[start + 1..end].to_owned(),
        _ => from.trim().to_owned(),
    }
}

#[tokio::test]
async fn an_mcp_state_change_is_written_from_the_operator_role() {
    let fx = fixture("event-operator").await;
    fx.add_member("hand", 2).await;
    let (handle, _) = fx
        .send_item(&fx.token, "task", &address("hand"), "close as operator")
        .await;
    // The creator's registration name is not a role the member's own rule
    // lets close an item for everyone; its `operator` role is.
    let operator = format!("operator@{}.local", prefix_for("creator"));
    let closed = fx.call(&fx.token, "done", json!({"handle": handle})).await;
    assert_eq!(closed["mode"], "closed", "{closed}");
    let reopened = fx
        .call(&fx.token, "reopen", json!({"handle": handle}))
        .await;
    assert_eq!(reopened["mode"], "reopened", "{reopened}");
    for event in [&closed, &reopened] {
        let letter = fx
            .raw_letter(&fx.token, event["message_id"].as_str().unwrap())
            .await;
        assert_eq!(from_address(&letter), operator, "{letter}");
        assert!(
            letter
                .lines()
                .any(|line| line.starts_with("X-Vivi-Event: ")),
            "{letter}"
        );
    }
    // The server counts it as before: closed then reopened is open.
    let open = fx
        .call(&fx.token, "list", json!({"kind": "task", "status": "open"}))
        .await;
    assert_eq!(handles(&open), [handle], "{open}");
}

#[tokio::test]
async fn a_mail_kind_event_reaches_a_member_that_is_neither_sender_nor_addressee() {
    let fx = fixture("event-bystander").await;
    let hand = fx.add_member("hand", 2).await;
    let scout = fx.add_member("scout", 4).await;
    let (handle, id) = fx
        .send_item(&fx.token, "need", &address("hand"), "watched")
        .await;
    let of = rfc_id(&id, "creator");
    let done = fx
        .put_letter(
            &hand,
            370,
            event_letter(
                "hand",
                &traveling(370),
                &of,
                &address("creator"),
                "done",
                &[("X-Vivi-Event-At", &stamp(0))],
            ),
            "mail",
            &address("creator"),
        )
        .await;
    // Plain mail from the same sender to the same addressee stays private.
    let plain = fx
        .put_letter(
            &hand,
            371,
            format!(
                "From: {}\nTo: {}\nSubject: private\nMessage-ID: <{}>\n\nonly creator\n",
                address("hand"),
                address("creator"),
                rfc_id(&traveling(371), "hand")
            )
            .into_bytes(),
            "mail",
            &address("creator"),
        )
        .await;

    let (status, page) = send_json(
        &fx.app,
        "GET",
        &format!("/v1/projects/{}/items?since=0", fx.project),
        None,
        Some(&scout),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let ids: Vec<&str> = page["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| record["message_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&done.as_str()), "{page}");
    assert!(!ids.contains(&plain.as_str()), "{page}");
    // By id, too: the event is readable, the private mail is not found.
    let (status, ..) = raw(
        &fx.app,
        "GET",
        &format!("/v1/projects/{}/items/{done}", fx.project),
        Vec::new(),
        Some(&scout),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, ..) = raw(
        &fx.app,
        "GET",
        &format!("/v1/projects/{}/items/{plain}", fx.project),
        Vec::new(),
        Some(&scout),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // The bystander folds the item closed: it holds the event.
    let shown = fx.call(&scout, "show", json!({"handle": handle})).await;
    assert_eq!(shown["item"]["status"], "done", "{shown}");
}

#[tokio::test]
async fn mcp_reopen_of_an_absorbed_item_is_refused() {
    let fx = fixture("absorbed").await;
    let (done_handle, done_id) = fx
        .send_item(&fx.token, "need", &address("creator"), "sealed done")
        .await;
    let (open_handle, open_id) = fx
        .send_item(&fx.token, "need", &address("creator"), "sealed open")
        .await;
    let seal = |n: u64, of: &str, state: &str| {
        event_letter(
            "creator",
            &traveling(n),
            &rfc_id(of, "creator"),
            &address("creator"),
            "absorbed",
            &[
                ("X-Vivi-Event-State", state),
                ("X-Vivi-Event-At", &stamp(0)),
            ],
        )
    };
    fx.put_letter(
        &fx.token,
        350,
        seal(350, &done_id, "done"),
        "need",
        &address("creator"),
    )
    .await;
    fx.put_letter(
        &fx.token,
        351,
        seal(351, &open_id, "open"),
        "need",
        &address("creator"),
    )
    .await;

    // Sealed done: reopen is refused; done is already done.
    let err = fx
        .call_err(&fx.token, "reopen", json!({"handle": done_handle}))
        .await;
    assert!(err.contains("absorbed"), "{err}");
    let again = fx
        .call(&fx.token, "done", json!({"handle": done_handle}))
        .await;
    assert_eq!(again["mode"], "already_done", "{again}");

    // Sealed open: nothing can be written against it.
    let err = fx
        .call_err(&fx.token, "reopen", json!({"handle": open_handle}))
        .await;
    assert!(err.contains("absorbed"), "{err}");
    let err = fx
        .call_err(&fx.token, "done", json!({"handle": open_handle}))
        .await;
    assert!(err.contains("absorbed"), "{err}");

    // The seal names the state: the open one is still open, flagged.
    let open = fx
        .call(&fx.token, "list", json!({"kind": "need", "status": "open"}))
        .await;
    assert_eq!(handles(&open), [open_handle], "{open}");
    assert_eq!(open["items"][0]["absorbed"], true, "{open}");
}

// ---------------------------------------------------------------------------
// A long project: the fold reads every letter, not the first page of them.
// ---------------------------------------------------------------------------

/// A project whose closing event lies past the first page a list serves.
struct Long {
    fx: Fixture,
    /// The handle of the first letter, closed by the project's last one.
    handle: String,
    /// The `Message-ID` of that closing event.
    late_rfc: String,
    /// When it is stamped: two minutes ahead (counted), the item's newest.
    late: chrono::DateTime<chrono::Utc>,
    /// Handles of the tasks nothing closes.
    open: Vec<String>,
    /// Tasks something closes, not counting the first letter.
    closed: u64,
}

impl Fixture {
    /// Append a task from `creator` of at least `bytes` bytes straight to the
    /// project store; returns its message id.
    fn append_task(&self, store: &ProjectStore, n: u64, bytes: usize) -> String {
        let id = traveling(n);
        let mut text = format!(
            "From: {}\nTo: {}\nSubject: filler {n}\nMessage-ID: <{}>\n\nletter {n}\n",
            address("creator"),
            address("hand"),
            rfc_id(&id, "creator"),
        );
        while text.len() < bytes {
            text.push_str("a line of filler text so the letter has a realistic size\n");
        }
        self.append_as_creator(store, &id, "task", text.into_bytes());
        id
    }

    fn append_as_creator(&self, store: &ProjectStore, id: &str, kind: &str, raw: Vec<u8>) {
        let item = AppendItem::new(id, "creator", Some(&address("hand")), Some(kind), raw)
            .unwrap()
            .with_from_prefix(Some(&prefix_for("creator")));
        store.append(&self.project, &item).unwrap();
    }
}

/// Fill a project through its store: one task sent over MCP, then `closed`
/// tasks each followed by its `done` event, then `open` tasks of `bytes`
/// bytes, then last of all an event that closes the first task.
async fn long_project(name: &str, closed: u64, open: u64, bytes: usize) -> Long {
    let fx = fixture(name).await;
    fx.add_member("hand", 2).await;
    let store = ProjectStore::new(&fx.dir);
    let (handle, id) = fx
        .send_item(&fx.token, "task", &address("hand"), "closed far away")
        .await;
    let of = rfc_id(&id, "creator");
    let begun = std::time::Instant::now();
    let mut n = 1_000_000u64;
    for _ in 0..closed {
        let task = fx.append_task(&store, n, 1024);
        n += 1;
        let done = event_letter(
            "creator",
            &traveling(n),
            &rfc_id(&task, "creator"),
            &address("hand"),
            "done",
            &[("X-Vivi-Event-At", &stamp(-3600))],
        );
        fx.append_as_creator(&store, &traveling(n), "mail", done);
        n += 1;
    }
    let mut opened = Vec::new();
    for _ in 0..open {
        opened.push(fx.append_task(&store, n, bytes)[4..12].to_owned());
        n += 1;
    }
    let late = chrono::Utc::now() + chrono::Duration::seconds(120);
    let late_at = late.to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    let closing = event_letter(
        "creator",
        &traveling(n),
        &of,
        &address("hand"),
        "done",
        &[("X-Vivi-Event-At", &late_at)],
    );
    fx.append_as_creator(&store, &traveling(n), "mail", closing);
    eprintln!(
        "long project {name}: {} letters appended in {:?}",
        closed * 2 + open + 2,
        begun.elapsed()
    );
    Long {
        late_rfc: rfc_id(&traveling(n), "creator"),
        fx,
        handle,
        late,
        open: opened,
        closed,
    }
}

/// Every tool that folds sees the closing event, and the fold's clock and
/// heads are the late event's: a reopen follows it.
async fn assert_every_tool_folds_the_late_event(long: &Long) {
    let Long {
        fx,
        handle,
        late_rfc,
        late,
        open,
        closed,
    } = long;
    let timed = |what: &str, started: std::time::Instant| {
        eprintln!("long project: {what} in {:?}", started.elapsed());
    };

    let started = std::time::Instant::now();
    let shown = fx.call(&fx.token, "show", json!({"handle": handle})).await;
    timed("show (one fold)", started);
    assert_eq!(shown["item"]["status"], "done", "{shown}");

    let started = std::time::Instant::now();
    let listed = fx
        .call(
            &fx.token,
            "list",
            json!({"kind": "task", "status": "open", "limit": 100}),
        )
        .await;
    timed("list open", started);
    if open.len() <= 100 {
        let mut got = handles(&listed);
        let mut want = open.clone();
        got.sort();
        want.sort();
        assert_eq!(got, want, "{listed}");
    } else {
        assert_eq!(handles(&listed).len(), 100, "{listed}");
    }
    let done = fx
        .call(
            &fx.token,
            "list",
            json!({"kind": "task", "status": "done", "limit": 100}),
        )
        .await;
    assert!(handles(&done).contains(handle), "{done}");

    let started = std::time::Instant::now();
    let overview = fx.call(&fx.token, "overview", json!({})).await;
    timed("overview", started);
    assert_eq!(
        overview["totals"]["tasks_open"],
        open.len(),
        "the first task is closed by the last letter: {overview}"
    );
    assert_eq!(overview["totals"]["ready"], open.len(), "{overview}");

    let started = std::time::Instant::now();
    let graph = fx.call(&fx.token, "graph", json!({})).await;
    timed("graph", started);
    assert_eq!(graph["totals"]["ready"], open.len(), "{graph}");
    assert_eq!(graph["totals"]["done"], closed + 1, "{graph}");

    // Settling folds the same way: the item is already closed, so a close
    // writes nothing, and a reopen follows the late event.
    let started = std::time::Instant::now();
    let again = fx.call(&fx.token, "done", json!({"handle": handle})).await;
    timed("done (fold, no write)", started);
    assert_eq!(again["mode"], "already_done", "{again}");
    let reopened = fx
        .call(&fx.token, "reopen", json!({"handle": handle}))
        .await;
    assert_eq!(reopened["mode"], "reopened", "{reopened}");
    let letter = fx
        .raw_letter(&fx.token, reopened["message_id"].as_str().unwrap())
        .await;
    let header = |name: &str| {
        letter
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}: ")))
            .map(str::to_owned)
    };
    assert_eq!(
        header("X-Vivi-Event-Prev").as_deref(),
        Some(format!("<{late_rfc}>").as_str()),
        "names the event past the first page as its head"
    );
    let at = chrono::DateTime::parse_from_rfc3339(&header("X-Vivi-Event-At").unwrap()).unwrap();
    assert!(
        at.timestamp_micros() > late.timestamp_micros(),
        "{at} is not past {late}"
    );
    let shown = fx.call(&fx.token, "show", json!({"handle": handle})).await;
    assert_eq!(shown["item"]["status"], "open", "{shown}");
}

/// A first page ends at 8 MiB of letters: 140 tasks of 60 KiB do not fit,
/// and the event after them used to be out of every fold.
#[tokio::test]
async fn an_event_past_the_byte_capped_first_page_is_folded_by_every_tool() {
    let long = long_project("long-bytes", 0, 140, 60 * 1024).await;
    assert_every_tool_folds_the_late_event(&long).await;
}

/// A first page ends at 500 letters: 2,600 of them, with the event last.
/// Writing them is the cost (one durable append each, about a minute and a
/// half), so it runs on request, and reports how long each fold took:
///
/// ```text
/// cargo test --release --test mcp -- --ignored --nocapture long_project
/// ```
///
/// `VIVI_FOLD_LETTERS` overrides the letter count.
#[tokio::test]
#[ignore = "writes thousands of letters; run with --ignored"]
async fn an_event_past_the_record_capped_first_page_is_folded_in_a_long_project() {
    let letters: u64 = std::env::var("VIVI_FOLD_LETTERS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(2_600);
    // Pairs of a task and its close, and one task left open.
    let long = long_project("long-records", (letters - 2) / 2, 1, 1024).await;
    assert_every_tool_folds_the_late_event(&long).await;
}

#[tokio::test]
async fn list_responses_carry_a_stable_epoch_across_calls_and_restarts() {
    let fx = fixture("epoch").await;
    let url = format!("/v1/projects/{}/items?since=0", fx.project);
    let (status, first) = send_json(&fx.app, "GET", &url, None, Some(&fx.token)).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let epoch = first["epoch"].as_u64().expect("list carries an epoch");
    assert!((1..=u64::from(u32::MAX)).contains(&epoch), "{epoch}");

    let (_, again) = send_json(&fx.app, "GET", &url, None, Some(&fx.token)).await;
    assert_eq!(again["epoch"].as_u64(), Some(epoch));
    // Past the head the page is empty and still carries the epoch.
    let past = format!("/v1/projects/{}/items?since=999999", fx.project);
    let (_, empty) = send_json(&fx.app, "GET", &past, None, Some(&fx.token)).await;
    assert_eq!(empty["epoch"].as_u64(), Some(epoch));

    // A restart: a new router over the same data directory.
    let restarted = api_router(fx.dir.clone(), Some(store_key()), None);
    let (status, after) = send_json(&restarted, "GET", &url, None, Some(&fx.token)).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(after["epoch"].as_u64(), Some(epoch));

    // A fresh project mints its own.
    let other = fixture("epoch-other").await;
    let url = format!("/v1/projects/{}/items?since=0", other.project);
    let (_, theirs) = send_json(&other.app, "GET", &url, None, Some(&other.token)).await;
    assert_ne!(theirs["epoch"].as_u64(), Some(epoch));
}

#[tokio::test]
async fn api_key_identity_is_optional_stored_and_validated() {
    let fx = fixture("key-identity").await;
    let path = format!("/v1/projects/{}/keys", fx.project);
    let (status, plain) = send_json(
        &fx.app,
        "POST",
        &path,
        Some(json!({"label": "plain"})),
        Some(&fx.token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{plain}");
    assert!(plain.get("identity").is_none(), "{plain}");

    let (status, acting) = send_json(
        &fx.app,
        "POST",
        &path,
        Some(json!({"label": "ops", "identity": "operator"})),
        Some(&fx.token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{acting}");
    assert_eq!(acting["identity"], "operator");
    // The key still authenticates as its minter.
    let key = acting["key"].as_str().unwrap();
    let overview = fx.call(key, "overview", json!({})).await;
    assert_eq!(overview["you"]["name"], "creator");

    let (_, listed) = send_json(&fx.app, "GET", &path, None, Some(&fx.token)).await;
    let keys = listed["keys"].as_array().unwrap();
    assert!(keys[0].get("identity").is_none(), "{listed}");
    assert_eq!(keys[1]["identity"], "operator");

    let (status, bad) = send_json(
        &fx.app,
        "POST",
        &path,
        Some(json!({"label": "bad", "identity": "has space"})),
        Some(&fx.token),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
}
