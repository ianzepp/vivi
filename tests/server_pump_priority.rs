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

/// A served host root with `operator` and `hand` roles and a `[pump]` table
/// whose home is the non-default [`PUMP_HOME`].
fn fixture(name: &str) -> Fx {
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
    text.push_str(&format!("\n[pump]\ntick = 20\nhome = \"{PUMP_HOME}\"\n"));
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
