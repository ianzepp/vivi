//! The `pump_control` MCP tool: operator-only pause, resume, stop and start
//! of a hosted project's pump, through the vivi library's control state.

use std::collections::BTreeSet;
use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi::mailspace::{Mailspace, RoleUpdate};
use vivi::pump::control::{PumpMode, inspect, project_home, set_project_mode};
use vivi::server::grants::{GrantLevel, GrantRecord};
use vivi::server::{AppState, app_state, hosted, router};

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

    fn mode(&self) -> PumpMode {
        inspect(&project_home(&self.root).unwrap()).unwrap().mode
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

    async fn control(&self, token: &str, action: &str) -> Value {
        let value = self
            .rpc(
                token,
                "tools/call",
                json!({"name": "pump_control", "arguments": {"action": action}}),
            )
            .await;
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        assert_ne!(value["result"]["isError"], true, "{action}: {text}");
        serde_json::from_str(text).unwrap()
    }

    async fn refused(&self, token: &str, action: &str) -> String {
        let value = self
            .rpc(
                token,
                "tools/call",
                json!({"name": "pump_control", "arguments": {"action": action}}),
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
async fn each_action_changes_the_stored_control_state() {
    let fx = fixture("actions");
    let key = fx.key_by(&fx.member, None);
    let listed = fx.rpc(&key, "tools/list", json!({})).await;
    let tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "pump_control")
        .expect("pump_control is listed");
    assert_eq!(
        tool["inputSchema"]["properties"]["action"]["enum"],
        json!(["pause", "resume", "stop", "start"])
    );

    for (action, expected) in [
        ("pause", PumpMode::Paused),
        ("resume", PumpMode::Running),
        ("stop", PumpMode::Stopped),
        ("start", PumpMode::Running),
        ("pause", PumpMode::Paused),
    ] {
        let answer = fx.control(&key, action).await;
        assert_eq!(answer["action"], action);
        assert_eq!(answer["mode"], expected.to_string(), "{answer}");
        assert_eq!(answer["by"], "operator");
        assert_eq!(fx.mode(), expected, "stored state after {action}");
    }
}

#[tokio::test]
async fn a_key_acting_as_the_operator_role_by_name_is_accepted() {
    let fx = fixture("named");
    let key = fx.key_by(&fx.member, Some("operator"));
    let answer = fx.control(&key, "stop").await;
    assert_eq!(answer["mode"], "stopped", "{answer}");
}

#[tokio::test]
async fn a_member_token_is_refused_and_changes_nothing() {
    let fx = fixture("token");
    // vivi 21 starts a fresh pump stopped; the refusal must change nothing from running.
    set_project_mode(&fx.root, PumpMode::Running).unwrap();
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
    let (status, value) = fx
        .post(
            &bearer,
            "tools/call",
            json!({"name": "pump_control", "arguments": {"action": "stop"}}),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
    let message = value["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("identity tokens are not accepted on /v1/mcp"),
        "{message}"
    );
    assert!(message.contains("server key"), "{message}");
    assert_eq!(fx.mode(), PumpMode::Running);
}

#[tokio::test]
async fn a_key_that_is_not_the_operators_is_refused() {
    let fx = fixture("others");
    // vivi 21 starts a fresh pump stopped; the refusal must change nothing from running.
    set_project_mode(&fx.root, PumpMode::Running).unwrap();
    // A key acting as a role that is not of kind operator.
    let hand = fx.key_by(&fx.member, Some("hand"));
    let message = fx.refused(&hand, "pause").await;
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
    let message = fx.refused(&bobs, "pause").await;
    assert!(message.contains("host's own member"), "{message}");
    assert_eq!(fx.mode(), PumpMode::Running);
}

#[tokio::test]
async fn an_unknown_action_is_refused() {
    let fx = fixture("unknown");
    // vivi 21 starts a fresh pump stopped; the refusal must change nothing from running.
    set_project_mode(&fx.root, PumpMode::Running).unwrap();
    let key = fx.key_by(&fx.member, None);
    let message = fx.refused(&key, "restart").await;
    assert!(
        message.contains("pause, resume, stop, or start"),
        "{message}"
    );
    assert_eq!(fx.mode(), PumpMode::Running);
}

#[tokio::test]
async fn a_fresh_project_starts_stopped() {
    // vivi 21 changed the fresh pump default from running to stopped.
    let fx = fixture("fresh");
    assert_eq!(fx.mode(), PumpMode::Stopped);
}

#[tokio::test]
async fn a_project_that_is_not_hosted_is_refused() {
    let base = scratch("owned");
    let state = app_state(base.join("data"), None, None).expect("app state");
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
    let key = server_key(&state, &project, "alice", Some("operator"));
    let fx = Fx {
        app: router(state.clone()),
        state,
        root: base,
        project,
        member: "alice".into(),
    };
    let message = fx.refused(&key, "pause").await;
    assert!(message.contains("hosted project"), "{message}");
}
