//! The `pump_gate` MCP tool: operator-only show and set of a hosted
//! project's pump gate command, through the vivi library's gate config.

use std::collections::BTreeSet;
use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi::mailspace::{Mailspace, RoleUpdate};
use vivi_server::{AppState, app_state, hosted, router};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-pump-control-{}-{name}",
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

/// A served host root with a `[pump]` table and `operator` and `hand` roles.
fn fixture(name: &str) -> Fx {
    let base = scratch(name);
    let root = base.join("hosts").join("app");
    let mut ms = Mailspace::init(Some(&root)).unwrap();
    ms.ensure_operator_role().unwrap();
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
    text.push_str("\n[pump]\ntick = 20\n");
    std::fs::write(&toml, text).unwrap();
    let state = app_state(base.join("data"), None, None);
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
        self.state
            .keys
            .mint(&self.project, member, "test", identity)
            .unwrap()
            .key
    }

    async fn rpc(&self, token: &str, method: &str, params: Value) -> Value {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/projects/{}/mcp", self.project))
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn gate(&self, token: &str, arguments: Value) -> Value {
        let value = self
            .rpc(
                token,
                "tools/call",
                json!({"name": "pump_gate", "arguments": arguments}),
            )
            .await;
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        assert_ne!(value["result"]["isError"], true, "{text}");
        serde_json::from_str(text).unwrap()
    }

    fn config_text(&self) -> String {
        std::fs::read_to_string(self.root.join(".vivi").join("mailspace.toml")).unwrap()
    }

    async fn refused(&self, token: &str, arguments: Value) -> String {
        let value = self
            .rpc(
                token,
                "tools/call",
                json!({"name": "pump_gate", "arguments": arguments}),
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
async fn show_on_a_fresh_project_is_null_and_set_round_trips() {
    let fx = fixture("roundtrip");
    let key = fx.key_by(&fx.member, None);
    let listed = fx.rpc(&key, "tools/list", json!({})).await;
    let tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "pump_gate")
        .expect("pump_gate is listed");
    assert_eq!(
        tool["inputSchema"]["properties"]["action"]["enum"],
        json!(["show", "set"])
    );

    let shown = fx.gate(&key, json!({"action": "show"})).await;
    assert_eq!(shown, json!({"command": null}));

    let set = fx
        .gate(
            &key,
            json!({"action": "set", "command": "cargo test --quiet"}),
        )
        .await;
    assert_eq!(set, json!({"command": "cargo test --quiet"}));
    let shown = fx.gate(&key, json!({"action": "show"})).await;
    assert_eq!(shown, json!({"command": "cargo test --quiet"}));
    let text = fx.config_text();
    assert!(text.contains("cargo test --quiet"), "{text}");
    assert!(text.contains("tick = 20"), "other config kept: {text}");

    let replaced = fx
        .gate(&key, json!({"action": "set", "command": "make gate"}))
        .await;
    assert_eq!(replaced, json!({"command": "make gate"}));
}

#[tokio::test]
async fn a_key_acting_as_the_operator_role_by_name_is_accepted() {
    let fx = fixture("named");
    let key = fx.key_by(&fx.member, Some("operator"));
    let set = fx
        .gate(&key, json!({"action": "set", "command": "true"}))
        .await;
    assert_eq!(set["command"], "true");
}

#[tokio::test]
async fn a_blank_or_missing_command_is_refused_and_changes_nothing() {
    let fx = fixture("blank");
    let key = fx.key_by(&fx.member, None);
    fx.gate(&key, json!({"action": "set", "command": "keep me"}))
        .await;
    let message = fx
        .refused(&key, json!({"action": "set", "command": "   "}))
        .await;
    assert!(message.contains("must not be empty"), "{message}");
    let message = fx.refused(&key, json!({"action": "set"})).await;
    assert!(message.contains("command"), "{message}");
    let shown = fx.gate(&key, json!({"action": "show"})).await;
    assert_eq!(shown["command"], "keep me");
}

#[tokio::test]
async fn a_member_token_is_refused_and_changes_nothing() {
    let fx = fixture("token");
    let bearer = fx
        .state
        .identities
        .register(
            &fx.project,
            "bob",
            &spki_pem(9),
            "0b0b0b0b-1111-4222-8333-444444444444",
        )
        .unwrap()
        .token;
    for arguments in [
        json!({"action": "set", "command": "evil"}),
        json!({"action": "show"}),
    ] {
        let message = fx.refused(&bearer, arguments).await;
        assert!(message.contains("operator's API key"), "{message}");
    }
    assert!(!fx.config_text().contains("evil"));
}

#[tokio::test]
async fn a_key_that_is_not_the_operators_is_refused() {
    let fx = fixture("others");
    // A key acting as a role that is not of kind operator.
    let hand = fx.key_by(&fx.member, Some("hand"));
    let message = fx
        .refused(&hand, json!({"action": "set", "command": "evil"}))
        .await;
    assert!(message.contains("kind operator"), "{message}");
    // A key another member minted, even one acting as the operator role.
    fx.state
        .identities
        .register(
            &fx.project,
            "bob",
            &spki_pem(9),
            "0b0b0b0b-1111-4222-8333-444444444444",
        )
        .unwrap();
    let bobs = fx.key_by("bob", Some("operator"));
    for arguments in [
        json!({"action": "set", "command": "evil"}),
        json!({"action": "show"}),
    ] {
        let message = fx.refused(&bobs, arguments).await;
        assert!(message.contains("host's own member"), "{message}");
    }
    assert!(!fx.config_text().contains("evil"));
}

#[tokio::test]
async fn an_unknown_action_is_refused() {
    let fx = fixture("unknown");
    let key = fx.key_by(&fx.member, None);
    let message = fx.refused(&key, json!({"action": "clear"})).await;
    assert!(message.contains("show or set"), "{message}");
}

#[tokio::test]
async fn a_project_that_is_not_hosted_is_refused() {
    let base = scratch("owned");
    let state = app_state(base.join("data"), None, None);
    let meta = state
        .projects
        .create_project_with_creator(&spki_pem(7), "alice")
        .unwrap();
    let project = meta.project_id;
    state
        .identities
        .register(
            &project,
            "alice",
            &spki_pem(7),
            "0a0a0a0a-1111-4222-8333-444444444444",
        )
        .ok();
    let key = state
        .keys
        .mint(&project, "alice", "bot", Some("operator"))
        .unwrap()
        .key;
    let fx = Fx {
        app: router(state.clone()),
        state,
        root: base,
        project,
        member: "alice".into(),
    };
    let message = fx.refused(&key, json!({"action": "show"})).await;
    assert!(message.contains("hosted project"), "{message}");
}
