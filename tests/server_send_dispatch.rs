//! The hosted `send` with `dispatch true`: the operator-authorized shortcut
//! that files the need through the library and hands it to the pump with one
//! open record ([`Mailspace::pump_send`]), plus an optional priority level.
//!
//! Fixture pattern borrowed from `server_pump_records.rs`: one served host
//! root with `operator` and `hand` roles and a configured pump home.
//!
//! The ordinary sends are covered too: a non-operator hosted key with the
//! dispatch options absent or false keeps the pre-dispatch contract, and a
//! dispatch-true send on a project the server owns end to end (durable or
//! chat) is refused instead of falling through to the transport send.

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi::mailspace::{Mailspace, PumpState, RoleUpdate};
use vivi::pump::priority;
use vivi::server::grants::{GrantLevel, GrantRecord};
use vivi::server::identities::IdentityRecord;
use vivi::server::projects::{Scan, Viewer};
use vivi::server::{AppState, app_state, hosted, router};
use vivi::storage::StoredMessageView;

/// The non-default pump home the fixture configures, relative to the project
/// root, so tests can tell the configured home from the default one.
const PUMP_HOME: &str = "pump-home";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-send-dispatch-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Fx {
    app: Router,
    state: AppState,
    root: PathBuf,
    project: String,
    /// The host's own member, which the operator's key authenticates as.
    member: String,
}

/// A served host root with `operator` and `hand` roles and a `[pump]` table
/// whose home is the non-default [`PUMP_HOME`]. `pump` selects whether the
/// table exists at all: dispatch without a priority must not need one.
fn fixture(name: &str, pump: bool) -> Fx {
    let base = scratch(name);
    let root = base.join("hosts").join("app");
    let mut ms = Mailspace::init(Some(&root)).unwrap();
    ms.ensure_operator_role().unwrap();
    ms.add_identity("mind").unwrap();
    ms.add_role(
        "hand",
        RoleUpdate {
            kind: Some(Some("hand".into())),
            ..RoleUpdate::default()
        },
    )
    .unwrap();
    let toml = root.join(".vivi").join("mailspace.toml");
    let mut text = std::fs::read_to_string(&toml).unwrap();
    if pump {
        text.push_str(&format!("\n[pump]\ntick = 20\nhome = \"{PUMP_HOME}\"\n"));
    }
    std::fs::write(&toml, text).unwrap();
    let state = app_state(base.join("data"), None, None).expect("app state");
    let report = hosted::reconcile(
        &state.projects,
        &state.identities,
        &BTreeSet::from([root.clone()]),
    );
    assert!(report.refused.is_empty(), "{report:?}");
    Fx {
        app: router(state.clone()),
        root,
        project: report.served[0].project_id.clone(),
        member: report.served[0].member.clone(),
        state,
    }
}

impl Fx {
    fn key_by(&self, member: &str, identity: Option<&str>) -> String {
        server_key(&self.state, &self.project, member, identity)
    }

    fn home(&self) -> PathBuf {
        self.root.join(PUMP_HOME)
    }

    fn default_home_db(&self) -> PathBuf {
        self.root.join(".vivi").join("pump").join("pump.db")
    }

    fn ms(&self) -> Mailspace {
        Mailspace::open_root(self.root.clone()).unwrap()
    }

    /// Every stored view, for counting what a call actually filed.
    fn stored_roles(&self) -> VecDeque<String> {
        self.ms()
            .storage()
            .unwrap()
            .list_messages()
            .unwrap()
            .into_iter()
            .map(|view| view.local_role)
            .collect()
    }

    fn records(&self, state: Option<PumpState>) -> Vec<vivi::mailspace::PumpRecord> {
        self.ms().pump_list(None, state).unwrap()
    }

    fn priorities(&self) -> Vec<(String, i64)> {
        priority::list(&self.home()).unwrap()
    }

    /// Every stored message copy in the mailspace, sent copies included.
    fn stored_views(&self) -> Vec<StoredMessageView> {
        self.ms().storage().unwrap().list_messages().unwrap()
    }

    /// A key whose stored grant is operator level whatever identity it
    /// names: the strongest stored authority a principal can hold, to prove
    /// the dispatch authorization reads current membership and role kind,
    /// never grant rows. The principal tag keeps each grant alone (the
    /// first grant for a project on a principal wins).
    fn operator_grant_key(&self, member: &str, identity: Option<&str>) -> String {
        let tag = identity.unwrap_or("default");
        self.state
            .server_keys
            .mint(
                &format!("{member}.grant-{tag}@{}", self.project),
                false,
                vec![GrantRecord {
                    project_id: self.project.clone(),
                    member: member.to_owned(),
                    identity: identity.map(str::to_owned),
                    level: GrantLevel::Operator,
                }],
                "test",
                member,
            )
            .unwrap()
            .key
    }

    /// The need-level arguments of a dispatch send: one recipient, one
    /// `done_when`, prose. `extra` carries the dispatch options under test.
    fn need_arguments(extra: Value) -> Value {
        let arguments = json!({
            "kind": "need",
            "to": ["mind"],
            "subject": "fix auth",
            "body": "please fix login",
            "done_when": "it works",
        });
        let mut merged = arguments.as_object().unwrap().clone();
        for (key, value) in extra.as_object().unwrap() {
            merged.insert(key.clone(), value.clone());
        }
        Value::Object(merged)
    }

    async fn post(&self, token: &str, method: &str, mut params: Value) -> (StatusCode, Value) {
        if method == "tools/call" {
            let args = params
                .as_object_mut()
                .unwrap()
                .entry("arguments")
                .or_insert_with(|| json!({}));
            if let Some(args) = args.as_object_mut() {
                args.entry("project")
                    .or_insert_with(|| json!(self.project.clone()));
            }
        }
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/mcp")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn rpc(&self, token: &str, method: &str, params: Value) -> Value {
        let (status, value) = self.post(token, method, params).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value
    }

    async fn call(&self, token: &str, tool: &str, arguments: Value) -> Value {
        let value = self
            .rpc(
                token,
                "tools/call",
                json!({"name": tool, "arguments": arguments}),
            )
            .await;
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        assert_ne!(value["result"]["isError"], true, "{tool}: {text}");
        serde_json::from_str(text).unwrap()
    }

    async fn refused(&self, token: &str, tool: &str, arguments: Value) -> String {
        let value = self
            .rpc(
                token,
                "tools/call",
                json!({"name": tool, "arguments": arguments}),
            )
            .await;
        assert_eq!(value["result"]["isError"], true, "{value}");
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str::<Value>(text).unwrap()["error"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

fn server_key(state: &AppState, project: &str, member: &str, identity: Option<&str>) -> String {
    let level = match identity {
        None | Some("operator") => GrantLevel::Operator,
        Some(_) => GrantLevel::Member,
    };
    // Grants live on the principal, and the first grant for a project wins.
    // A separate principal per acting identity keeps each key's grant alone.
    let tag = identity.unwrap_or("default");
    state
        .server_keys
        .mint(
            &format!("{member}.{tag}@{project}"),
            false,
            vec![GrantRecord {
                project_id: project.to_owned(),
                member: member.to_owned(),
                identity: identity.map(str::to_owned),
                level,
            }],
            "test",
            member,
        )
        .unwrap()
        .key
}

/// An ED25519 public key PEM for registering another member, from a fixed
/// seed so the test needs no entropy.
fn spki_pem(seed: u8) -> String {
    use base64::Engine;
    const PREFIX: &[u8] = &[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut der = PREFIX.to_vec();
    der.extend_from_slice(
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .as_bytes(),
    );
    format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
        base64::engine::general_purpose::STANDARD.encode(der)
    )
}

/// One app state with two server-owned projects — a durable one and a live
/// chat — each with the member `alice` registered. This is the transport
/// half of the send surface: `/v1/mcp` with a server key, and the legacy
/// `/v1/rooms/{id}/mcp` route with the member's identity token.
struct Transport {
    app: Router,
    state: AppState,
    durable: String,
    chat: String,
    /// The member's identity token on each project.
    durable_token: String,
    chat_token: String,
    /// A server key with an operator grant on each project.
    durable_key: String,
    chat_key: String,
}

fn transport(name: &str) -> Transport {
    let base = scratch(name);
    let state = app_state(base.join("data"), None, None).expect("app state");
    let durable = state
        .projects
        .create_project_with_creator(&spki_pem(11), "alice")
        .unwrap()
        .project_id;
    let durable_token = state
        .identities
        .register(
            &durable,
            "alice",
            &spki_pem(11),
            "0a0a0a0a-1111-4222-8333-444444444444",
        )
        .unwrap()
        .token;
    let chat = state
        .projects
        .create_chat_project(3_600)
        .unwrap()
        .project_id;
    let chat_token = state
        .identities
        .register(
            &chat,
            "alice",
            &spki_pem(12),
            "0b0b0b0b-1111-4222-8333-444444444444",
        )
        .unwrap()
        .token;
    Transport {
        app: router(state.clone()),
        durable_key: server_key(&state, &durable, "alice", Some("operator")),
        chat_key: server_key(&state, &chat, "alice", Some("operator")),
        state,
        durable,
        chat,
        durable_token,
        chat_token,
    }
}

impl Transport {
    /// The registered member of `project`.
    fn member(&self, project: &str) -> IdentityRecord {
        self.state.identities.by_name(project, "alice").unwrap()
    }

    /// Every letter in `project`'s transport index the member may see: the
    /// whole local record a fall-through would have to grow.
    fn letters(&self, project: &str) -> Vec<String> {
        let mut ids = Vec::new();
        self.state
            .projects
            .scan_visible(
                project,
                &Viewer::of(&self.member(project)),
                Scan::All,
                |record| {
                    ids.push(record.message_id);
                },
            )
            .unwrap();
        ids
    }

    /// One `tools/call` POST to `uri` with a Bearer token.
    async fn post(
        &self,
        uri: &str,
        token: &str,
        tool: &str,
        arguments: Value,
    ) -> (StatusCode, Value) {
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": tool, "arguments": arguments},
        });
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// A successful server-key tool call on `/v1/mcp`.
    async fn server_call(&self, key: &str, project: &str, tool: &str, arguments: Value) -> Value {
        let mut arguments = arguments;
        arguments["project"] = json!(project);
        let (status, value) = self.post("/v1/mcp", key, tool, arguments).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        assert_ne!(value["result"]["isError"], true, "{tool}: {text}");
        serde_json::from_str(text).unwrap()
    }

    /// A refused server-key tool call on `/v1/mcp`: HTTP 200 with isError
    /// and the decoded error text.
    async fn server_refused(
        &self,
        key: &str,
        project: &str,
        tool: &str,
        arguments: Value,
    ) -> String {
        let mut arguments = arguments;
        arguments["project"] = json!(project);
        let (status, value) = self.post("/v1/mcp", key, tool, arguments).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["result"]["isError"], true, "{value}");
        let decoded: Value =
            serde_json::from_str(value["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        decoded["error"].as_str().unwrap().to_owned()
    }

    /// A successful member-token tool call on the legacy rooms route.
    async fn rooms_call(&self, project: &str, token: &str, tool: &str, arguments: Value) -> Value {
        let uri = format!("/v1/rooms/{project}/mcp");
        let (status, value) = self.post(&uri, token, tool, arguments).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        assert_ne!(value["result"]["isError"], true, "{tool}: {text}");
        serde_json::from_str(text).unwrap()
    }

    /// A refused member-token tool call on the legacy rooms route.
    async fn rooms_refused(
        &self,
        project: &str,
        token: &str,
        tool: &str,
        arguments: Value,
    ) -> String {
        let uri = format!("/v1/rooms/{project}/mcp");
        let (status, value) = self.post(&uri, token, tool, arguments).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["result"]["isError"], true, "{value}");
        let decoded: Value =
            serde_json::from_str(value["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        decoded["error"].as_str().unwrap().to_owned()
    }
}

/// A valid transport need whose dispatch-true variant is under test: the
/// ordinary control drops `dispatch` alone.
fn transport_need() -> Value {
    json!({
        "kind": "need",
        "to": ["alice@0a0a0a0a.local"],
        "subject": "fix the login",
        "body": "please fix login",
        "done_when": "it works",
    })
}

/// The dispatch arguments for `project`: the same need, addressed to the
/// member's own prefix address, with dispatch on.
fn transport_dispatch_need(t: &Transport, project: &str) -> Value {
    let mailspace_id = t.member(project).mailspace_id.unwrap();
    let mut arguments = transport_need();
    arguments["to"] = json!([format!("alice@{}.local", &mailspace_id[..8])]);
    arguments["dispatch"] = json!(true);
    arguments
}

/// No pump database was created anywhere under `project`'s directory: a
/// refused dispatch leaves no pump record and no priority entry behind.
fn assert_no_pump_state(state: &AppState, project: &str) {
    let mut stack = vec![state.projects.project_dir(project).unwrap()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            assert!(!name.starts_with("pump.db"), "{path:?}");
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
}

/// The persisted state a refusal must preserve: every stored message copy
/// in the mailspace (sent ones included), every pump record, and the
/// priority rows at the configured home. The first priority listing creates
/// its database, so the before-side of a comparison initializes the store
/// the after-side then reads.
#[derive(Debug, PartialEq)]
struct Snapshot {
    views: Vec<StoredMessageView>,
    records: Vec<Value>,
    priorities: Vec<(String, i64)>,
}

fn snapshot(fx: &Fx) -> Snapshot {
    Snapshot {
        views: fx.stored_views(),
        records: fx
            .records(None)
            .into_iter()
            .map(|record| serde_json::to_value(&record).unwrap())
            .collect(),
        priorities: fx.priorities(),
    }
}

#[tokio::test]
async fn operator_dispatch_returns_need_and_one_open_record() {
    // The explicit operator identity and the host-default one (a key with no
    // named identity acts as the server default) both dispatch.
    for (name, identity) in [("explicit", Some("operator")), ("default", None)] {
        let fx = fixture(name, true);
        let key = fx.key_by(&fx.member, identity);

        let plain = fx.call(&key, "send", Fx::need_arguments(json!({}))).await;
        let sent = fx
            .call(&key, "send", Fx::need_arguments(json!({"dispatch": true})))
            .await;
        // HTTP 200 and isError false are asserted by `call`. The delivery
        // fields are exactly the ordinary send's, plus `pump`.
        let plain_keys: BTreeSet<String> = plain.as_object().unwrap().keys().cloned().collect();
        let sent_keys: BTreeSet<String> = sent.as_object().unwrap().keys().cloned().collect();
        let extra: Vec<_> = sent_keys.difference(&plain_keys).collect();
        assert_eq!(extra, ["pump"], "{sent}");
        assert_eq!(sent["kind"], "need", "{sent}");
        assert_eq!(sent["from"], "operator", "{sent}");
        assert_eq!(sent["delivered"][0]["identity"], "mind", "{sent}");
        assert_eq!(sent["remote"], plain["remote"], "{sent}");

        let need = sent["handle"].as_str().unwrap().to_owned();
        let pump = sent["pump"].as_object().expect("pump record");
        assert_eq!(pump["status"], "open", "{sent}");
        assert_eq!(pump["kind"], "pump", "{sent}");
        assert_eq!(pump["need"], need, "{sent}");
        assert_eq!(pump["handle"], sent["pump"]["handle"], "{sent}");
        // Both handles resolve in the mailspace store, and exactly one open
        // library record exists for the canonical need.
        let ms = fx.ms();
        let storage = ms.storage().unwrap();
        storage.resolve_message_token(&need).unwrap();
        storage
            .resolve_message_token(pump["handle"].as_str().unwrap())
            .unwrap();
        let open = fx.records(Some(PumpState::Open));
        assert_eq!(open.len(), 1, "{open:?}");
        assert_eq!(open[0].handle, pump["handle"].as_str().unwrap());
        assert_eq!(open[0].need, need);

        // A second dispatch files exactly one open record for its own
        // canonical need, alongside the first.
        let mut other = Fx::need_arguments(json!({"dispatch": true}));
        other["subject"] = json!("fix billing");
        let second = fx.call(&key, "send", other).await;
        let second_need = second["handle"].as_str().unwrap().to_owned();
        let open = fx.records(Some(PumpState::Open));
        assert_eq!(open.len(), 2, "{open:?}");
        let needs: Vec<&str> = open.iter().map(|r| r.need.as_str()).collect();
        assert_eq!(needs, [need.as_str(), second_need.as_str()]);
        assert_ne!(need, second_need);
    }
}

#[tokio::test]
async fn a_requested_priority_lands_at_the_configured_home_and_zero_clears() {
    let fx = fixture("priority", true);
    let key = fx.key_by(&fx.member, Some("operator"));

    let sent = fx
        .call(
            &key,
            "send",
            Fx::need_arguments(json!({"dispatch": true, "priority": 7})),
        )
        .await;
    let need = sent["handle"].as_str().unwrap().to_owned();
    assert_eq!(sent["priority"], 7, "{sent}");
    assert_eq!(sent["pump"]["need"], need.as_str(), "{sent}");
    // The row is keyed by the record's canonical need, in the configured
    // home, and the default home stayed unused.
    assert_eq!(fx.priorities(), vec![(need.clone(), 7)]);
    assert!(!fx.default_home_db().exists());

    // An explicit zero keeps the clearing semantics: the dispatch succeeds,
    // the answer carries the level, and the zero-level need acquires no row
    // while the first need's row stays.
    let mut cleared = Fx::need_arguments(json!({"dispatch": true, "priority": 0}));
    cleared["subject"] = json!("fix billing");
    let second = fx.call(&key, "send", cleared).await;
    assert_eq!(second["priority"], 0, "{second}");
    assert_eq!(second["pump"]["need"], second["handle"], "{second}");
    assert_eq!(fx.priorities(), vec![(need, 7)]);
    assert_eq!(fx.records(Some(PumpState::Open)).len(), 2);
}

/// The canonical need of a multi-recipient dispatch: the content copy with
/// the lowest message id, which is the key the priority row is written
/// under — not necessarily the first delivered copy handle in the response.
#[tokio::test]
async fn dispatch_priority_uses_the_canonical_need_and_configured_home() {
    let fx = fixture("canonical-home", true);
    let operator = fx.key_by(&fx.member, Some("operator"));

    // A dispatched need to two local recipients files one content copy per
    // recipient plus the sender's own; `handle` is only the first delivered
    // copy, while the record and the priority row key the canonical copy.
    let sent = fx
        .call(
            &operator,
            "send",
            Fx::need_arguments(json!({
                "dispatch": true,
                "priority": 7,
                "to": ["mind", "hand"],
            })),
        )
        .await;
    assert_eq!(sent["priority"], 7, "{sent}");
    let need = sent["handle"].as_str().unwrap().to_owned();
    let pump_need = sent["pump"]["need"].as_str().unwrap().to_owned();
    let pump_handle = sent["pump"]["handle"].as_str().unwrap().to_owned();
    let copies: Vec<String> = sent["delivered"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["handle"].as_str().unwrap().to_owned())
        .chain(std::iter::once(sent["sent"].as_str().unwrap().to_owned()))
        .collect();
    assert_eq!(copies.len(), 3, "{sent}");

    // Every returned handle resolves in the mailspace store.
    let ms = fx.ms();
    let storage = ms.storage().unwrap();
    let need_id = storage.resolve_message_token(&need).unwrap();
    for handle in &copies {
        storage.resolve_message_token(handle).unwrap();
    }
    storage.resolve_message_token(&pump_handle).unwrap();

    // The record's need is the canonical copy: the lowest message id among
    // the content's copies, whatever handle the response led with.
    let content_id = storage.message_by_id(&need_id).unwrap().unwrap().content_id;
    let canonical = storage
        .message_ids_by_content(&content_id)
        .unwrap()
        .into_iter()
        .min()
        .unwrap();
    assert_eq!(pump_need, storage.display_handle(&canonical).unwrap());

    // The library record agrees with the response, and the entire priority
    // listing carries exactly the canonical need at the configured home; no
    // priority state reached the default home.
    let open = fx.records(Some(PumpState::Open));
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0].handle, pump_handle);
    assert_eq!(open[0].need, pump_need);
    assert_eq!(fx.priorities(), vec![(pump_need.clone(), 7)]);
    assert!(!fx.default_home_db().exists());

    // A requested zero files the record and confirms the level but leaves
    // no row; an omitted priority is absent from the response. Neither
    // adds a row, and the first need's row stays alone.
    let mut zero = Fx::need_arguments(json!({"dispatch": true, "priority": 0}));
    zero["subject"] = json!("fix billing");
    let zero = fx.call(&operator, "send", zero).await;
    assert_eq!(zero["priority"], 0, "{zero}");
    assert_eq!(zero["pump"]["need"], zero["handle"], "{zero}");

    let mut plain = Fx::need_arguments(json!({"dispatch": true}));
    plain["subject"] = json!("fix search");
    let plain = fx.call(&operator, "send", plain).await;
    assert!(plain["priority"].is_null(), "{plain}");
    assert_eq!(plain["pump"]["need"], plain["handle"], "{plain}");

    assert_eq!(fx.records(Some(PumpState::Open)).len(), 3);
    assert_eq!(fx.priorities(), vec![(pump_need, 7)]);
    assert!(!fx.default_home_db().exists());

    // A hosted root without [pump] refuses a requested priority — zero
    // included — before any need or sent copy is filed; the refusal names
    // the missing table in its decoded error.
    let bare = fixture("canonical-bare", false);
    let bare_operator = bare.key_by(&bare.member, Some("operator"));
    for level in [3, 0] {
        let mut arguments = Fx::need_arguments(json!({"dispatch": true, "priority": level}));
        arguments["subject"] = json!(format!("bare refusal {level}"));
        let message = bare.refused(&bare_operator, "send", arguments).await;
        assert!(message.contains("[pump]"), "{message}");
        assert!(bare.records(None).is_empty(), "level {level}");
        assert!(bare.stored_views().is_empty(), "level {level}");
        assert!(!bare.home().join("pump.db").exists());
        assert!(!bare.default_home_db().exists());
    }
    // The missing table is a precondition of a requested priority only: a
    // dispatch without one keeps the library defaults and files normally.
    let plain = bare
        .call(
            &bare_operator,
            "send",
            Fx::need_arguments(json!({"dispatch": true})),
        )
        .await;
    assert_eq!(bare.records(Some(PumpState::Open)).len(), 1);
    assert_eq!(plain["pump"]["need"], plain["handle"], "{plain}");
}

/// Dispatch partial failures: the shortcut files the need, hands it to the
/// pump with one open record, and writes an optional priority row — and
/// mail.sqlite and pump.db are not one transaction. A failure after the
/// send answers as a tool error that names the durable work so it can be
/// recovered; nothing is resent, deleted, or rolled back.
#[tokio::test]
async fn dispatch_reports_partial_failure_without_resending() {
    pump_record_failure_names_the_retained_need().await;
    priority_failure_names_the_filed_need_record_and_canonical_need().await;
}

/// The substring of `text` between the first `open` and the next `close`,
/// trimmed: the recovery handles a partial-failure error carries are
/// located by their markers, never by the exact sentence.
fn marker_span(text: &str, open: &str, close: &str) -> String {
    let start = text
        .find(open)
        .unwrap_or_else(|| panic!("{open:?} missing from {text:?}"))
        + open.len();
    let rest = &text[start..];
    let end = rest.find(close).unwrap_or(rest.len());
    rest[..end].trim().to_owned()
}

/// A `[pump] from` that names no local role: the need send never reads it,
/// the pump record send fails to resolve its sender, and the error names
/// the filed need — which still resolves, holding exactly its delivered
/// and sent copies, with no record and no priority anywhere.
async fn pump_record_failure_names_the_retained_need() {
    let fx = fixture("record-fail", true);
    let operator = fx.key_by(&fx.member, Some("operator"));
    // `[pump]` is the fixture's last table, so the line lands inside it.
    let toml = fx.root.join(".vivi").join("mailspace.toml");
    let mut text = std::fs::read_to_string(&toml).unwrap();
    text.push_str("from = \"ghost\"\n");
    std::fs::write(&toml, text).unwrap();

    let error = fx
        .refused(
            &operator,
            "send",
            Fx::need_arguments(json!({"dispatch": true})),
        )
        .await;
    let need = marker_span(&error, "need ", " was filed");
    let ms = fx.ms();
    let storage = ms.storage().unwrap();
    let need_id = storage.resolve_message_token(&need).unwrap();
    assert_eq!(
        storage.message_by_id(&need_id).unwrap().unwrap().local_role,
        "needs",
        "{error}"
    );

    // The retained copies: one delivered need plus its sent copy, one
    // content — no duplicate send, no pump record, no priority row and no
    // database at either home.
    let views = fx.stored_views();
    let mut roles: Vec<&str> = views.iter().map(|view| view.local_role.as_str()).collect();
    roles.sort_unstable();
    assert_eq!(roles, ["needs", "sent"], "{views:?}");
    // One content group holding both copies: the send happened once.
    let mut contents: Vec<&str> = views.iter().map(|view| view.content_id.as_str()).collect();
    contents.sort_unstable();
    contents.dedup();
    assert_eq!(contents, [views[0].content_id.as_str()], "{views:?}");
    let delivered = views
        .iter()
        .find(|view| view.local_role == "needs")
        .unwrap();
    assert_eq!(storage.display_handle(&delivered.message_id).unwrap(), need);
    assert!(fx.records(None).is_empty(), "{error}");
    assert!(!fx.home().join("pump.db").exists());
    assert!(!fx.default_home_db().exists());
    assert!(fx.priorities().is_empty());
}

/// A directory occupying the configured home's `pump.db`: the need and its
/// pump record file normally, and the priority write — the shortcut's last
/// step — fails to open the store. The error names the filed need, the open
/// record and the record's canonical need; all three resolve, the record is
/// the only one, and nothing was deleted, retried, or prioritized.
async fn priority_failure_names_the_filed_need_record_and_canonical_need() {
    let fx = fixture("priority-fail", true);
    let operator = fx.key_by(&fx.member, Some("operator"));
    let obstacle = fx.home().join("pump.db");
    std::fs::create_dir_all(&obstacle).unwrap();

    let error = fx
        .refused(
            &operator,
            "send",
            Fx::need_arguments(json!({"dispatch": true, "priority": 7})),
        )
        .await;
    let need = marker_span(&error, "need ", " was filed");
    let record = marker_span(&error, "pump record ", ",");
    let canonical = marker_span(&error, " on ", " failed");

    // All three recovery handles resolve independently, and the durable
    // state agrees: exactly one open record, its handle and canonical need
    // the ones the error named.
    let ms = fx.ms();
    let storage = ms.storage().unwrap();
    let need_id = storage.resolve_message_token(&need).unwrap();
    storage.resolve_message_token(&record).unwrap();
    let canonical_id = storage.resolve_message_token(&canonical).unwrap();
    let open = fx.records(Some(PumpState::Open));
    assert_eq!(open.len(), 1, "{error}");
    assert_eq!(open[0].handle, record);
    assert_eq!(open[0].need, canonical);
    let (shown, body) = ms.pump_show(&record).unwrap();
    assert_eq!(shown.handle, record);
    assert_eq!(shown.need, canonical);
    assert!(body.contains(&need), "{body}");

    // Exactly one need delivery plus its sent copy and the record's own
    // two copies: no retry, no extra or done record, nothing deleted.
    assert_eq!(fx.records(None).len(), 1, "{error}");
    let views = fx.stored_views();
    let mut roles: Vec<&str> = views.iter().map(|view| view.local_role.as_str()).collect();
    roles.sort_unstable();
    assert_eq!(roles, ["needs", "pumps", "sent", "sent"], "{views:?}");
    // Two content groups of two copies each — the need's and the record's:
    // the send and the record happened once, and no third letter exists.
    let mut contents: Vec<&str> = views.iter().map(|view| view.content_id.as_str()).collect();
    contents.sort_unstable();
    contents.dedup();
    assert_eq!(contents.len(), 2, "{views:?}");
    for content in &contents {
        assert_eq!(
            views
                .iter()
                .filter(|view| view.content_id == *content)
                .count(),
            2,
            "{views:?}"
        );
    }
    assert_eq!(storage.display_handle(&need_id).unwrap(), need);
    assert_eq!(canonical_id, need_id);

    // Removing only the deliberate obstacle: no priority row was committed.
    std::fs::remove_dir_all(&obstacle).unwrap();
    assert!(fx.priorities().is_empty());
    assert!(!fx.default_home_db().exists());
}

/// The library record contract (mailspace/pump_record.rs): an open record
/// alone admits a need — the need's own addressee plays no part. A need
/// addressed only to the hand role dispatches like any other.
#[tokio::test]
async fn dispatch_preserves_library_recipient_rules() {
    let fx = fixture("hand-recipient", true);
    let operator = fx.key_by(&fx.member, Some("operator"));

    let sent = fx
        .call(
            &operator,
            "send",
            Fx::need_arguments(json!({"dispatch": true, "to": ["hand"]})),
        )
        .await;
    assert_eq!(sent["kind"], "need", "{sent}");
    assert_eq!(sent["delivered"][0]["identity"], "hand", "{sent}");
    let need = sent["handle"].as_str().unwrap().to_owned();
    assert_eq!(sent["pump"]["status"], "open", "{sent}");
    assert_eq!(sent["pump"]["need"], need.as_str(), "{sent}");

    // Exactly one open record, agreeing with the response's handles.
    let open = fx.records(Some(PumpState::Open));
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0].need, need);
    assert_eq!(open[0].handle, sent["pump"]["handle"].as_str().unwrap());
}

#[tokio::test]
async fn dispatch_without_priority_needs_no_pump_table_and_files_no_row() {
    let fx = fixture("no-pump-table", false);
    let key = fx.key_by(&fx.member, Some("operator"));

    let sent = fx
        .call(&key, "send", Fx::need_arguments(json!({"dispatch": true})))
        .await;
    let need = sent["handle"].as_str().unwrap().to_owned();
    assert_eq!(sent["pump"]["need"], need.as_str(), "{sent}");
    assert_eq!(fx.records(Some(PumpState::Open)).len(), 1);
    // No priority was requested: no pump home was resolved, so no database
    // was created anywhere, configured or default.
    assert!(!fx.home().join("pump.db").exists());
    assert!(!fx.default_home_db().exists());
}

#[tokio::test]
async fn a_priority_without_a_pump_table_is_refused_before_anything_is_filed() {
    let fx = fixture("pump-refusal", false);
    let key = fx.key_by(&fx.member, Some("operator"));

    let message = fx
        .refused(
            &key,
            "send",
            Fx::need_arguments(json!({"dispatch": true, "priority": 3})),
        )
        .await;
    assert!(message.contains("[pump]"), "{message}");
    // The refusal came before the send: no need, no record, no database.
    assert!(fx.records(None).is_empty());
    assert!(
        fx.stored_roles()
            .iter()
            .all(|role| role != "needs" && role != "pumps")
    );
    assert!(!fx.default_home_db().exists());
}

#[tokio::test]
async fn a_non_operator_key_is_refused_and_nothing_is_filed() {
    let fx = fixture("hand", true);
    let hand = fx.key_by(&fx.member, Some("hand"));
    let message = fx
        .refused(&hand, "send", Fx::need_arguments(json!({"dispatch": true})))
        .await;
    assert!(message.contains("kind operator"), "{message}");
    assert!(fx.records(None).is_empty());
    assert!(fx.stored_roles().iter().all(|role| role != "needs"));

    // An invalid dispatch combination is refused before routing, too, and
    // files nothing.
    let operator = fx.key_by(&fx.member, Some("operator"));
    let message = fx
        .refused(
            &operator,
            "send",
            Fx::need_arguments(json!({"dispatch": true, "kind": "mail"})),
        )
        .await;
    assert!(message.contains("kind need"), "{message}");
    let message = fx
        .refused(
            &operator,
            "send",
            Fx::need_arguments(json!({"priority": 1})),
        )
        .await;
    assert!(message.contains("priority requires dispatch"), "{message}");
    assert!(fx.records(None).is_empty());
    assert!(fx.stored_roles().iter().all(|role| role != "needs"));
}

#[tokio::test]
async fn dispatch_requires_pump_send_authority_before_filing() {
    let fx = fixture("authority", true);
    let operator = fx.key_by(&fx.member, Some("operator"));

    // Seed real persisted state — one dispatched need with a priority — so
    // the snapshots below have message copies, an open record, and a row to
    // preserve across every refusal.
    let seeded = fx
        .call(
            &operator,
            "send",
            Fx::need_arguments(json!({"dispatch": true, "priority": 7})),
        )
        .await;
    assert_eq!(seeded["priority"], 7, "{seeded}");
    assert_eq!(fx.records(Some(PumpState::Open)).len(), 1);
    assert_eq!(fx.priorities().len(), 1);
    let before = snapshot(&fx);
    assert!(!before.views.is_empty());

    // The host member acting as the `hand` role: right member, wrong role
    // kind. The key carries an operator-level grant, and the stored grant
    // does not stand in for the role kind the mailspace has now.
    let hand = fx.operator_grant_key(&fx.member, Some("hand"));
    let message = fx
        .refused(&hand, "send", Fx::need_arguments(json!({"dispatch": true})))
        .await;
    assert!(message.contains("kind operator"), "{message}");
    assert_eq!(snapshot(&fx), before);

    // A different registered member, whose key holds a real operator grant
    // on this project and acts as the operator role: stored grant authority
    // is not membership of the hosted mailspace.
    fx.state
        .identities
        .register(
            &fx.project,
            "bob",
            &spki_pem(9),
            "0b0b0b0b-1111-4222-8333-444444444444",
        )
        .unwrap();
    let bobs = fx.operator_grant_key("bob", Some("operator"));
    let message = fx
        .refused(&bobs, "send", Fx::need_arguments(json!({"dispatch": true})))
        .await;
    assert!(message.contains("host's own member"), "{message}");
    assert_eq!(snapshot(&fx), before);

    // The same other member with no acting identity: the key would fall
    // back to the hosted default (the operator role) and is still refused
    // on membership before anything is filed.
    let bobs_default = fx.operator_grant_key("bob", None);
    let message = fx
        .refused(
            &bobs_default,
            "send",
            Fx::need_arguments(json!({"dispatch": true})),
        )
        .await;
    assert!(message.contains("host's own member"), "{message}");
    assert_eq!(snapshot(&fx), before);
}

#[tokio::test]
async fn dispatch_options_are_validated_before_filing() {
    let fx = fixture("options", false);
    let operator = fx.key_by(&fx.member, Some("operator"));

    // Control for the established optional-boolean convention: an accepted
    // null dispatch is an ordinary send — it files its letter and no pump
    // record, which is exactly the state the refusals must keep still.
    let filed = fx
        .call(
            &operator,
            "send",
            Fx::need_arguments(json!({"dispatch": null})),
        )
        .await;
    assert_eq!(filed["kind"], "need", "{filed}");
    assert!(filed["pump"].is_null(), "{filed}");
    assert!(fx.records(None).is_empty());
    assert!(!fx.stored_views().is_empty());

    let before = snapshot(&fx);
    for arguments in option_refusal_cases() {
        // `refused` asserts HTTP 200, isError true, and a decoded error
        // object; the exact prose is not pinned. Nothing may file.
        let message = fx.refused(&operator, "send", arguments.clone()).await;
        assert!(!message.is_empty(), "{arguments}");
        assert_eq!(snapshot(&fx), before, "{arguments}");
    }
}

/// The refused dispatch option combinations, each starting from an
/// otherwise-valid need send so that an ignored option would file a
/// letter: dispatch with a kind the pump never settles, malformed dispatch
/// values, priority levels of every invalid type, and a priority — zero
/// included — whose dispatch is omitted, false, or null. The numeric
/// boundaries themselves are additionally covered by the shared parser
/// tests in `send_options_test.rs`.
fn option_refusal_cases() -> Vec<Value> {
    let mut cases = Vec::new();
    for kind in ["mail", "task", "want", "chat"] {
        cases.push(Fx::need_arguments(json!({"dispatch": true, "kind": kind})));
    }
    for dispatch in [json!("yes"), json!(1), json!(1.5), json!([]), json!({})] {
        cases.push(Fx::need_arguments(json!({"dispatch": dispatch})));
    }
    for priority in [
        Value::Null,
        json!("7"),
        json!(false),
        json!(1.0),
        json!(1.5),
        json!(-1),
        json!(9_223_372_036_854_775_808u64),
        json!([]),
        json!({}),
    ] {
        cases.push(Fx::need_arguments(json!({
            "dispatch": true,
            "priority": priority,
        })));
    }
    for (priority, dispatch) in [
        (json!(2), None),
        (json!(2), Some(json!(false))),
        (json!(2), Some(Value::Null)),
        (json!(0), None),
        (json!(0), Some(json!(false))),
        (json!(0), Some(Value::Null)),
    ] {
        let mut arguments = json!({"priority": priority});
        if let Some(dispatch) = dispatch {
            arguments["dispatch"] = dispatch;
        }
        cases.push(Fx::need_arguments(arguments));
    }
    cases
}

/// The exact response of an ordinary hosted send: the pre-dispatch fields
/// only, delivered as the acting role, with no pump or priority anywhere.
fn assert_hosted_plain_shape(sent: &Value) {
    let keys: BTreeSet<String> = sent.as_object().unwrap().keys().cloned().collect();
    let expected: BTreeSet<String> = [
        "delivered",
        "from",
        "handle",
        "kind",
        "message_id",
        "remote",
        "sent",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(keys, expected, "{sent}");
    assert_eq!(sent["kind"], "need", "{sent}");
    assert_eq!(sent["from"], "hand", "{sent}");
    assert_eq!(sent["delivered"][0]["identity"], "mind", "{sent}");
    assert_eq!(sent["delivered"][0]["handle"], sent["handle"], "{sent}");
    assert!(sent["sent"].is_string(), "{sent}");
    assert!(!sent["message_id"].is_null(), "{sent}");
}

/// An ordinary send from a non-operator hosted key keeps the pre-dispatch
/// contract: dispatch omitted or false changes nothing — permission,
/// composition, validation, delivery and the response fields stay the
/// ordinary send's, and nothing reaches the pump. The durable-project
/// transport send keeps its own established response shape.
#[tokio::test]
async fn plain_send_preserves_permissions_delivery_and_response() {
    let fx = fixture("plain", true);
    let hand = fx.key_by(&fx.member, Some("hand"));
    plain_hosted_sends(&fx, &hand).await;
    plain_hosted_rules(&fx, &hand).await;
    durable_transport_shape().await;
}

/// The omitted and false dispatch sends of the hand role: identical
/// ordinary needs carrying the hosted fields only, and no pump state at
/// either home.
async fn plain_hosted_sends(fx: &Fx, hand: &str) {
    let plain = fx.call(hand, "send", Fx::need_arguments(json!({}))).await;
    assert_hosted_plain_shape(&plain);
    let explicit = fx
        .call(hand, "send", Fx::need_arguments(json!({"dispatch": false})))
        .await;
    assert_hosted_plain_shape(&explicit);
    assert_ne!(plain["handle"], explicit["handle"], "{plain}");
    // Neither touched the pump: no record, no priority row, no database at
    // the configured or the default home (checked before the first
    // priority listing, which would create one).
    assert!(fx.records(None).is_empty(), "{plain}");
    assert!(!fx.home().join("pump.db").exists());
    assert!(!fx.default_home_db().exists());
    assert!(fx.priorities().is_empty());
}

/// The ordinary rules are unchanged for the non-operator key: dispatch
/// stays operator-only and the option parser still guards plain sends;
/// the need letter gate refuses a missing done_when; the composer refuses
/// sugar beside a whole letter; a dependency named at send rides in the
/// front matter. Every refusal files nothing.
async fn plain_hosted_rules(fx: &Fx, hand: &str) {
    let need = fx.call(hand, "send", Fx::need_arguments(json!({}))).await;
    let need_handle = need["handle"].as_str().unwrap().to_owned();
    let before = snapshot(fx);

    let message = fx
        .refused(hand, "send", Fx::need_arguments(json!({"dispatch": true})))
        .await;
    assert!(message.contains("kind operator"), "{message}");
    let message = fx
        .refused(hand, "send", Fx::need_arguments(json!({"priority": 1})))
        .await;
    assert!(message.contains("priority requires dispatch"), "{message}");
    let message = fx
        .refused(
            hand,
            "send",
            json!({"kind": "need", "to": ["mind"], "subject": "s", "body": "just do it"}),
        )
        .await;
    assert!(message.contains("need has no done_when"), "{message}");
    let message = fx
        .refused(
            hand,
            "send",
            json!({
                "kind": "need", "to": ["mind"], "subject": "s",
                "body": "+++\ndone_when = [\"y\"]\n+++\n\nx",
                "target": "repo",
            }),
        )
        .await;
    assert!(message.contains("whole letter"), "{message}");
    assert!(
        message.contains("put target in its front matter"),
        "{message}"
    );
    assert_eq!(snapshot(fx), before, "every refusal filed nothing");

    // A dependent task files as an ordinary task, its front matter naming
    // the filed need.
    let dependent = fx
        .call(
            hand,
            "send",
            json!({
                "kind": "task", "to": ["mind"], "subject": "after",
                "body": "once it works", "depends_on": need_handle,
            }),
        )
        .await;
    assert_eq!(dependent["kind"], "task", "{dependent}");
    let ms = fx.ms();
    let storage = ms.storage().unwrap();
    let id = storage
        .resolve_message_token(dependent["handle"].as_str().unwrap())
        .unwrap();
    let raw = String::from_utf8_lossy(&storage.read_message(&id).unwrap()).into_owned();
    assert!(
        raw.contains(&format!(r#"depends_on = ["{need_handle}"]"#)),
        "{raw}"
    );
    assert!(fx.records(None).is_empty());
    assert!(fx.priorities().is_empty());
}

/// The transport send of a durable project keeps its established shape:
/// message_id, handle, sequence, kind, duplicate — no hosted delivery
/// fields are imposed on it, and the letter is filed.
async fn durable_transport_shape() {
    let t = transport("plain-transport");
    let sent = t
        .server_call(&t.durable_key, &t.durable, "send", transport_need())
        .await;
    let keys: BTreeSet<String> = sent.as_object().unwrap().keys().cloned().collect();
    let expected: BTreeSet<String> = ["duplicate", "handle", "kind", "message_id", "sequence"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(keys, expected, "{sent}");
    assert_eq!(sent["kind"], "need", "{sent}");
    assert_eq!(sent["duplicate"], false, "{sent}");
    assert!(sent.get("delivered").is_none(), "{sent}");
    assert!(sent.get("sent").is_none(), "{sent}");
    assert_eq!(t.letters(&t.durable).len(), 1, "{sent}");
}

/// A dispatch-true send on a project the server owns end to end — durable
/// or chat — is refused as a tool error at HTTP 200 and never falls
/// through to the transport send. On the legacy rooms route, which accepts
/// the member's token and would transport-send the very same call without
/// dispatch, the refusal is the dispatch guard itself. Nothing is filed:
/// no transport letter, no local copy, no pump record, no priority entry.
#[tokio::test]
async fn unsupported_dispatch_does_not_fall_through_to_transport_send() {
    let t = transport("unsupported");
    ordinary_transport_controls(&t).await;
    server_keys_refuse_dispatch(&t).await;
    member_tokens_refuse_dispatch_on_rooms(&t).await;
    identity_tokens_refused_on_v1_mcp(&t).await;
}

/// Controls: with dispatch absent, both fixtures file letters — the server
/// key on `/v1/mcp` for the durable project, the member token on the
/// legacy rooms route for the chat — so the refusals below are the
/// dispatch guard, not a broken fixture.
async fn ordinary_transport_controls(t: &Transport) {
    let sent = t
        .server_call(&t.durable_key, &t.durable, "send", transport_need())
        .await;
    assert_eq!(sent["kind"], "need", "{sent}");
    assert!(sent["message_id"].is_string(), "{sent}");
    assert_eq!(t.letters(&t.durable).len(), 1, "{sent}");
    let chat_sent = t
        .rooms_call(
            &t.chat,
            &t.chat_token,
            "send",
            json!({"kind": "chat", "subject": "hi", "body": "from the room"}),
        )
        .await;
    assert_eq!(chat_sent["kind"], "chat", "{chat_sent}");
    assert_eq!(t.letters(&t.chat).len(), 1, "{chat_sent}");
}

/// Server keys holding operator grants on both server-owned projects: a
/// dispatch-true need is refused at HTTP 200 with isError, and no
/// transport item, local copy, pump record or priority entry appears.
async fn server_keys_refuse_dispatch(t: &Transport) {
    for (project, key) in [(&t.durable, &t.durable_key), (&t.chat, &t.chat_key)] {
        let before = t.letters(project);
        let message = t
            .server_refused(key, project, "send", transport_dispatch_need(t, project))
            .await;
        assert!(message.contains("hosted project"), "{message}");
        assert_eq!(t.letters(project), before, "{project}");
        assert_no_pump_state(&t.state, project);
    }
}

/// The legacy rooms route accepts each member token — the controls file
/// letters through it — so a dispatch-true call there that answers as a
/// tool error is the legacy dispatch guard itself, and it files nothing.
async fn member_tokens_refuse_dispatch_on_rooms(t: &Transport) {
    for (project, token) in [(&t.durable, &t.durable_token), (&t.chat, &t.chat_token)] {
        let before = t.letters(project);
        let message = t
            .rooms_refused(project, token, "send", transport_dispatch_need(t, project))
            .await;
        assert!(message.contains("hosted project"), "{message}");
        assert_eq!(t.letters(project), before, "{project}");
        assert_no_pump_state(&t.state, project);
    }
}

/// `/v1/mcp` refuses an identity token at the door with its own status: a
/// 401 naming the server key. That is authentication, not the dispatch
/// guard — the guard is proven above on routes that accept the caller —
/// and it files nothing either.
async fn identity_tokens_refused_on_v1_mcp(t: &Transport) {
    for (project, token) in [(&t.durable, &t.durable_token), (&t.chat, &t.chat_token)] {
        let before = t.letters(project);
        let (status, value) = t
            .post(
                "/v1/mcp",
                token,
                "send",
                transport_dispatch_need(t, project),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
        assert_eq!(value["error"]["code"], "unauthorized", "{value}");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("server key"),
            "{value}"
        );
        assert_eq!(t.letters(project), before, "{project}");
    }
}
