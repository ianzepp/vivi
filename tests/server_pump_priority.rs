//! The `pump_priority` MCP tool: operator-only per-need dispatch priority on
//! a hosted project, through the vivi library's `pump::priority` verbs.
//!
//! Fixture pattern borrowed from `server_pump_records.rs` and
//! `server_send_dispatch.rs`: one served host root with `operator` and
//! `hand` roles and a configured non-default pump home, so persisted state
//! is compared against `priority::list` at the configured home, never the
//! default one. Priority keys are literal: the needs under test do not
//! resolve to letters.

use std::collections::BTreeSet;
use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi::mailspace::{Mailspace, RoleUpdate};
use vivi::pump::priority;
use vivi::server::grants::{GrantLevel, GrantRecord};
use vivi::server::{AppState, app_state, hosted, router};

/// The non-default pump home the fixture configures, relative to the project
/// root, so tests can tell the configured home from the default one.
const PUMP_HOME: &str = "pump-home";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-pump-priority-{}-{name}",
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

fn fixture(name: &str) -> Fx {
    fixture_with(name, true)
}

/// A served host root with `operator` and `hand` roles and, when `pump`, a
/// `[pump]` table whose home is the non-default [`PUMP_HOME`].
fn fixture_with(name: &str, pump: bool) -> Fx {
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
    if pump {
        let toml = root.join(".vivi").join("mailspace.toml");
        let mut text = std::fs::read_to_string(&toml).unwrap();
        text.push_str(&format!("\n[pump]\ntick = 20\nhome = \"{PUMP_HOME}\"\n"));
        std::fs::write(&toml, text).unwrap();
    }
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

    /// The configured pump home, where priority rows live.
    fn pump_home(&self) -> PathBuf {
        self.root.join(PUMP_HOME)
    }

    /// The library's own view of the persisted rows at the configured home.
    fn priorities(&self) -> Vec<(String, i64)> {
        priority::list(&self.pump_home()).unwrap()
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

    /// The full tool result of a refused call, for checks beyond the message.
    async fn refused_result(&self, token: &str, tool: &str, arguments: Value) -> Value {
        let value = self
            .rpc(
                token,
                "tools/call",
                json!({"name": tool, "arguments": arguments}),
            )
            .await;
        assert_eq!(value["result"]["isError"], true, "{value}");
        value["result"].clone()
    }

    async fn refused(&self, token: &str, tool: &str, arguments: Value) -> String {
        let result = self.refused_result(token, tool, arguments).await;
        let text = result["content"][0]["text"].as_str().unwrap();
        serde_json::from_str::<Value>(text).unwrap()["error"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// Set one need's level and check the receipt and the persisted row.
    async fn set(&self, key: &str, need: &str, level: Value) -> Value {
        let answer = self
            .call(
                key,
                "pump_priority",
                json!({"action": "set", "need": need, "level": level}),
            )
            .await;
        assert_eq!(
            answer,
            json!({"action": "set", "need": need, "level": level}),
            "{answer}"
        );
        answer
    }

    /// List through the tool and require it to match `priority::list` at the
    /// configured home.
    async fn listed(&self, key: &str) -> Vec<(String, i64)> {
        let answer = self
            .call(key, "pump_priority", json!({"action": "list"}))
            .await;
        let stored = self.priorities();
        let expected: Vec<Value> = stored
            .iter()
            .map(|(need, level)| json!({"need": need, "level": level}))
            .collect();
        assert_eq!(answer, json!({"priorities": expected}), "{answer}");
        stored
    }
}

/// A key granted at operator level on `project` whose identity names
/// `identity` even when that role is not an operator: the stored grant level
/// must not matter, because authorization is re-derived per call.
fn operator_grant_key(state: &AppState, project: &str, member: &str, identity: &str) -> String {
    state
        .server_keys
        .mint(
            &format!("{member}.{identity}-operator@{project}"),
            false,
            vec![GrantRecord {
                project_id: project.to_owned(),
                member: member.to_owned(),
                identity: Some(identity.to_owned()),
                level: GrantLevel::Operator,
            }],
            "test",
            member,
        )
        .unwrap()
        .key
}

fn server_key(state: &AppState, project: &str, member: &str, identity: Option<&str>) -> String {
    let level = match identity {
        None | Some("operator") => GrantLevel::Operator,
        Some(_) => GrantLevel::Member,
    };
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

#[tokio::test]
async fn pump_priority_set_clear_and_list_match_the_library() {
    let fx = fixture("flow");
    let key = fx.key_by(&fx.member, None);

    // An untouched home lists empty, through the tool and the library.
    assert_eq!(fx.listed(&key).await, Vec::new());

    // Positive set on a literal key that resolves to no letter at all.
    fx.set(&key, "fix-auth", json!(5)).await;
    assert_eq!(fx.listed(&key).await, vec![("fix-auth".into(), 5)]);

    // Replacement: the same key keeps one row at the new level.
    fx.set(&key, "fix-auth", json!(9)).await;
    assert_eq!(fx.listed(&key).await, vec![("fix-auth".into(), 9)]);

    // More literal keys. i64::MAX sorts first; a tie at 7 sorts by handle.
    fx.set(&key, "z-canary", json!(1)).await;
    fx.set(&key, "b-canary", json!(7)).await;
    fx.set(&key, "a-canary", json!(7)).await;
    fx.set(&key, "top-canary", json!(i64::MAX)).await;
    assert_eq!(
        fx.listed(&key).await,
        vec![
            ("top-canary".to_owned(), i64::MAX),
            ("fix-auth".to_owned(), 9),
            ("a-canary".to_owned(), 7),
            ("b-canary".to_owned(), 7),
            ("z-canary".to_owned(), 1),
        ]
    );

    // A zero set removes the row and still answers level 0.
    fx.set(&key, "fix-auth", json!(0)).await;
    assert_eq!(
        fx.listed(&key).await,
        vec![
            ("top-canary".to_owned(), i64::MAX),
            ("a-canary".to_owned(), 7),
            ("b-canary".to_owned(), 7),
            ("z-canary".to_owned(), 1),
        ]
    );

    // An explicit clear removes one row, answering level 0.
    let answer = fx
        .call(
            &key,
            "pump_priority",
            json!({"action": "clear", "need": "b-canary"}),
        )
        .await;
    assert_eq!(
        answer,
        json!({"action": "clear", "need": "b-canary", "level": 0}),
        "{answer}"
    );
    assert_eq!(
        fx.listed(&key).await,
        vec![
            ("top-canary".to_owned(), i64::MAX),
            ("a-canary".to_owned(), 7),
            ("z-canary".to_owned(), 1),
        ]
    );

    // Clearing an absent literal key — never set, or already cleared —
    // succeeds and changes nothing.
    for need in ["never-set", "b-canary"] {
        let answer = fx
            .call(
                &key,
                "pump_priority",
                json!({"action": "clear", "need": need}),
            )
            .await;
        assert_eq!(
            answer,
            json!({"action": "clear", "need": need, "level": 0}),
            "{answer}"
        );
    }
    assert_eq!(
        fx.listed(&key).await,
        vec![
            ("top-canary".to_owned(), i64::MAX),
            ("a-canary".to_owned(), 7),
            ("z-canary".to_owned(), 1),
        ]
    );
}

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

#[tokio::test]
async fn pump_priority_validates_actions_and_requires_operator_for_every_action() {
    let fx = fixture("guard");
    let operator = fx.key_by(&fx.member, None);

    // Seed known literal-key priorities through the library and snapshot the
    // rows; the needs under test resolve to no letters and no pump records.
    priority::set(&fx.pump_home(), "seed-a", 4).unwrap();
    priority::set(&fx.pump_home(), "seed-b", 9).unwrap();
    priority::set(&fx.pump_home(), "seed-c", 1).unwrap();
    let before = fx.priorities();
    assert_eq!(
        before,
        vec![
            ("seed-b".to_owned(), 9),
            ("seed-a".to_owned(), 4),
            ("seed-c".to_owned(), 1),
        ]
    );

    // Argument validation, table-tested with an operator key: every row
    // keeps its other arguments valid so each refusal isolates one
    // condition, and nothing is ever written.
    let invalid: Vec<Value> = vec![
        // The action: missing, null, non-string, unknown.
        json!({}),
        json!({"action": null}),
        json!({"action": 7}),
        json!({"action": "wipe"}),
        // set: a need is required and must be a non-blank string.
        json!({"action": "set", "level": 2}),
        json!({"action": "set", "need": "", "level": 2}),
        json!({"action": "set", "need": "   ", "level": 2}),
        json!({"action": "set", "need": 7, "level": 2}),
        // set: a level is required and must be a whole number of 0 or more.
        json!({"action": "set", "need": "guard"}),
        json!({"action": "set", "need": "guard", "level": null}),
        json!({"action": "set", "need": "guard", "level": true}),
        json!({"action": "set", "need": "guard", "level": "2"}),
        json!({"action": "set", "need": "guard", "level": -1}),
        json!({"action": "set", "need": "guard", "level": 1.5}),
        json!({"action": "set", "need": "guard", "level": 9_223_372_036_854_775_808_u64}),
        // clear: a need is required, and any level is refused.
        json!({"action": "clear"}),
        json!({"action": "clear", "need": ""}),
        json!({"action": "clear", "need": 7}),
        json!({"action": "clear", "need": "guard", "level": 1}),
        json!({"action": "clear", "need": "guard", "level": null}),
        json!({"action": "clear", "need": "guard", "level": false}),
        // list: any need or level is refused, null fields included.
        json!({"action": "list", "need": "guard"}),
        json!({"action": "list", "need": null}),
        json!({"action": "list", "level": 1}),
        json!({"action": "list", "level": null}),
        json!({"action": "list", "need": null, "level": null}),
    ];
    for arguments in &invalid {
        let message = fx
            .refused(&operator, "pump_priority", arguments.clone())
            .await;
        assert!(!message.is_empty(), "{arguments}: {message}");
    }
    assert_eq!(fx.priorities(), before);

    // Authorization, per action, with otherwise-valid arguments: the shared
    // pump authorization must hold for set, clear, and list alike.
    let set_arguments = json!({"action": "set", "need": "guard", "level": 2});
    let clear_arguments = json!({"action": "clear", "need": "guard"});
    let list_arguments = json!({"action": "list"});

    // The host's hand key, and a misleading stored grant: the hand
    // identity's key carries an operator-level grant, but authorization is
    // re-derived from the mailspace's roles, never read from the grant.
    let hand = fx.key_by(&fx.member, Some("hand"));
    let hand_as_operator = operator_grant_key(&fx.state, &fx.project, &fx.member, "hand");
    // Another registered member, acting as the operator role or with no
    // acting identity at all, both at operator grant level.
    fx.state
        .identities
        .register(
            &fx.project,
            "bob",
            &spki_pem(9),
            "0b0b0b0b-1010-4222-8333-444444444444",
        )
        .unwrap();
    let bob_operator = fx.key_by("bob", Some("operator"));
    let bob_default = fx.key_by("bob", None);

    for (token, expected) in [
        (&hand, "kind operator"),
        (&hand_as_operator, "kind operator"),
        (&bob_operator, "host's own member"),
        (&bob_default, "host's own member"),
    ] {
        for arguments in [&set_arguments, &clear_arguments, &list_arguments] {
            let result = fx
                .refused_result(token, "pump_priority", arguments.clone())
                .await;
            let text = result["content"][0]["text"].as_str().unwrap();
            if arguments["action"] == "list" {
                // A refused list answers the refusal only; it must not
                // disclose the stored priorities.
                assert!(!text.contains("priorities"), "{text}");
                assert!(!text.contains("seed-"), "{text}");
            }
            let decoded = serde_json::from_str::<Value>(text).unwrap();
            let message = decoded["error"].as_str().unwrap();
            assert!(message.contains(expected), "{arguments}: {message}");
        }
    }
    assert_eq!(fx.priorities(), before);

    // Non-hosted projects the server owns end to end, one durable and one
    // chat, refuse every action by name even with an operator-level key.
    let durable = fx
        .state
        .projects
        .create_project_with_creator(&spki_pem(7), "carol")
        .unwrap()
        .project_id;
    fx.state
        .identities
        .register(
            &durable,
            "carol",
            &spki_pem(7),
            "0c0c0c0c-1010-4222-8333-444444444444",
        )
        .ok();
    let chat = fx
        .state
        .projects
        .create_chat_project(3600)
        .unwrap()
        .project_id;
    fx.state
        .identities
        .register(
            &chat,
            "chatter",
            &spki_pem(8),
            "0d0d0d0d-1010-4222-8333-444444444444",
        )
        .ok();
    let durable_key = server_key(&fx.state, &durable, "carol", Some("operator"));
    let chat_key = server_key(&fx.state, &chat, "chatter", Some("operator"));
    for (token, project) in [(&durable_key, &durable), (&chat_key, &chat)] {
        for arguments in [&set_arguments, &clear_arguments, &list_arguments] {
            let mut arguments = arguments.clone();
            arguments["project"] = json!(project);
            let message = fx.refused(token, "pump_priority", arguments).await;
            assert!(
                message.contains("only available on a hosted project"),
                "{message}"
            );
        }
    }
    assert_eq!(fx.priorities(), before);

    // A member (identity) token is refused at /v1/mcp itself, with the
    // proper HTTP status; this is not a substitute for the per-action
    // operator refusals above.
    let dana = fx
        .state
        .identities
        .register(
            &fx.project,
            "dana",
            &spki_pem(10),
            "0e0e0e0e-1010-4222-8333-444444444444",
        )
        .unwrap();
    let (status, value) = fx
        .post(
            &dana.token,
            "tools/call",
            json!({"name": "pump_priority", "arguments": {"action": "list"}}),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
    let message = value["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("identity tokens are not accepted on /v1/mcp"),
        "{message}"
    );
    assert_eq!(fx.priorities(), before);

    // A project without a [pump] table has no priority home at all: every
    // action is an error rather than an empty successful result, and no
    // database appears at either the configured or the default home.
    let bare = fixture_with("no-pump", false);
    let bare_key = bare.key_by(&bare.member, None);
    for arguments in [&set_arguments, &clear_arguments, &list_arguments] {
        let message = bare
            .refused(&bare_key, "pump_priority", arguments.clone())
            .await;
        assert!(message.contains("[pump]"), "{arguments}: {message}");
    }
    assert!(!bare.pump_home().join("pump.db").exists());
    assert!(
        !bare
            .root
            .join(".vivi")
            .join("pump")
            .join("pump.db")
            .exists()
    );

    // A genuine priority-store opening failure stays an error for every
    // action too: the configured home is a plain file, so the store at
    // `<home>/pump.db` cannot open, and the file itself is left alone.
    let broken = fixture("broken-store");
    std::fs::write(broken.pump_home(), "not a pump home").unwrap();
    let corrupt = std::fs::read(broken.pump_home()).unwrap();
    let broken_key = broken.key_by(&broken.member, None);
    for arguments in [&set_arguments, &clear_arguments, &list_arguments] {
        let message = broken
            .refused(&broken_key, "pump_priority", arguments.clone())
            .await;
        assert!(!message.is_empty(), "{arguments}: {message}");
    }
    assert_eq!(std::fs::read(broken.pump_home()).unwrap(), corrupt);
}

#[tokio::test]
async fn pump_priority_refuses_bad_actions_and_arguments_before_writing() {
    let fx = fixture("refusals");
    let key = fx.key_by(&fx.member, None);
    let bad: Vec<Value> = vec![
        json!({"action": "wipe"}),
        json!({}),
        json!({"action": "set"}),
        json!({"action": "set", "need": "fix-auth"}),
        json!({"action": "set", "need": "fix-auth", "level": "5"}),
        json!({"action": "set", "need": "fix-auth", "level": -1}),
        json!({"action": "set", "need": "fix-auth", "level": 1.5}),
        json!({"action": "set", "need": "fix-auth", "level": null}),
        json!({"action": "set", "need": "", "level": 3}),
        json!({"action": "set", "need": "   ", "level": 3}),
        json!({"action": "clear"}),
        json!({"action": "clear", "need": "fix-auth", "level": 1}),
        json!({"action": "clear", "need": "fix-auth", "level": null}),
        json!({"action": "list", "need": "fix-auth"}),
        json!({"action": "list", "level": null}),
    ];
    for arguments in &bad {
        let message = fx.refused(&key, "pump_priority", arguments.clone()).await;
        assert!(!message.is_empty(), "{arguments}: {message}");
    }
    // Nothing above was written, and a non-operator key is refused too.
    assert_eq!(fx.priorities(), Vec::new());
    let hand = fx.key_by(&fx.member, Some("hand"));
    let message = fx
        .refused(
            &hand,
            "pump_priority",
            json!({"action": "set", "need": "fix-auth", "level": 5}),
        )
        .await;
    assert!(message.contains("kind operator"), "{message}");
    assert_eq!(fx.priorities(), Vec::new());
    // A good set still works after the refusals.
    fx.set(&key, "fix-auth", json!(2)).await;
    assert_eq!(fx.listed(&key).await, vec![("fix-auth".into(), 2)]);
}
