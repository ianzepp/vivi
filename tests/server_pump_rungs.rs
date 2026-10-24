//! The `pump_rungs` MCP tool: operator-only show and set of a hosted
//! project's pump rung commands, per rung and per target, through the vivi
//! library's rung config.

use std::collections::BTreeSet;
use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi::mailspace::{Mailspace, RoleUpdate};
use vivi_server::grants::{GrantLevel, GrantRecord};
use vivi_server::{AppState, app_state, hosted, router};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-pump-rungs-{}-{name}",
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
    std::fs::create_dir_all(root.join("web")).unwrap();
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
    text.push_str("\n[pump]\ntick = 20\n\n[pump.targets.web]\npath = \"web\"\n");
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
        server_key(&self.state, &self.project, member, identity)
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

    async fn rungs(&self, token: &str, arguments: Value) -> Value {
        let value = self
            .rpc(
                token,
                "tools/call",
                json!({"name": "pump_rungs", "arguments": arguments}),
            )
            .await;
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        assert_ne!(value["result"]["isError"], true, "{text}");
        serde_json::from_str(text).unwrap()
    }

    fn config_text(&self) -> String {
        std::fs::read_to_string(self.root.join(".vivi").join("mailspace.toml")).unwrap()
    }

    /// A real pool and workspace rung pools. A rung command is rolled back
    /// while the rung table has no effective pools.
    fn seed_rung_pools(&self) {
        let path = self.root.join(".vivi").join("mailspace.toml");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(
            "\n[pools.p]\nprovider = \"local\"\nslots = 1\n\n[pump.rungs]\npools = [\"p\"]\n",
        );
        std::fs::write(&path, text).unwrap();
    }

    async fn refused(&self, token: &str, arguments: Value) -> String {
        let value = self
            .rpc(
                token,
                "tools/call",
                json!({"name": "pump_rungs", "arguments": arguments}),
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

fn rung_table(task: Value, need: Value, target: Value, pools: Value) -> Value {
    json!({
        "target": target, "task": task, "need": need, "main": null,
        "release": null, "timeout": 1800, "pools": pools
    })
}

#[tokio::test]
async fn show_on_a_fresh_project_is_unset_and_set_round_trips_per_rung() {
    let fx = fixture("roundtrip");
    let key = fx.key_by(&fx.member, None);
    let listed = fx.rpc(&key, "tools/list", json!({})).await;
    let tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "pump_rungs")
        .expect("pump_rungs is listed");
    let properties = &tool["inputSchema"]["properties"];
    assert_eq!(properties["action"]["enum"], json!(["show", "set"]));
    assert_eq!(
        properties["rung"]["enum"],
        json!(["task", "need", "main", "release"])
    );
    assert_eq!(
        tool["inputSchema"]["required"],
        json!(["project", "action"])
    );

    // The answer is a parsed structure, not the `{text}` of RungsConfig::show.
    let shown = fx.rungs(&key, json!({"action": "show"})).await;
    assert!(shown.get("text").is_none(), "{shown}");
    assert_eq!(
        shown,
        rung_table(Value::Null, Value::Null, Value::Null, json!([]))
    );

    fx.seed_rung_pools();
    let set = fx
        .rungs(
            &key,
            json!({"action": "set", "rung": "need", "command": "cargo test --quiet"}),
        )
        .await;
    assert_eq!(
        set,
        rung_table(
            Value::Null,
            json!("cargo test --quiet"),
            Value::Null,
            json!(["p"])
        )
    );
    let shown = fx.rungs(&key, json!({"action": "show"})).await;
    assert_eq!(shown, set);
    let text = fx.config_text();
    assert!(text.contains("[pump.rungs]"), "{text}");
    assert!(text.contains("cargo test --quiet"), "{text}");
    assert!(text.contains("tick = 20"), "other config kept: {text}");

    let replaced = fx
        .rungs(
            &key,
            json!({"action": "set", "rung": "need", "command": "make gate"}),
        )
        .await;
    assert_eq!(replaced["need"], "make gate");
    let other = fx
        .rungs(
            &key,
            json!({"action": "set", "rung": "task", "command": "make unit"}),
        )
        .await;
    assert_eq!(other["task"], "make unit");
    assert_eq!(other["need"], "make gate");
}

#[tokio::test]
async fn a_target_set_writes_the_target_table_and_falls_back_to_the_workspace() {
    let fx = fixture("target");
    let key = fx.key_by(&fx.member, None);
    fx.seed_rung_pools();
    fx.rungs(
        &key,
        json!({"action": "set", "rung": "task", "command": "make unit"}),
    )
    .await;
    let set = fx
        .rungs(
            &key,
            json!({"action": "set", "rung": "need", "command": "npm test", "target": "web"}),
        )
        .await;
    assert_eq!(
        set,
        rung_table(
            json!("make unit"),
            json!("npm test"),
            json!("web"),
            json!(["p"])
        )
    );
    assert!(fx.config_text().contains("[pump.targets.web.rungs]"));

    let shown = fx
        .rungs(&key, json!({"action": "show", "target": "web"}))
        .await;
    assert_eq!(shown, set);
    // No target: the workspace value, untouched by the target set.
    let workspace = fx.rungs(&key, json!({"action": "show"})).await;
    assert_eq!(
        workspace,
        rung_table(json!("make unit"), Value::Null, Value::Null, json!(["p"]))
    );
}

#[tokio::test]
async fn an_unknown_target_is_refused_and_changes_nothing() {
    let fx = fixture("badtarget");
    let key = fx.key_by(&fx.member, None);
    let before = fx.config_text();
    let message = fx
        .refused(&key, json!({"action": "show", "target": "nope"}))
        .await;
    assert!(message.contains("unknown pump target `nope`"), "{message}");
    let message = fx
        .refused(
            &key,
            json!({"action": "set", "rung": "need", "command": "x", "target": "nope"}),
        )
        .await;
    assert!(message.contains("unknown pump target `nope`"), "{message}");
    assert_eq!(fx.config_text(), before);
}

#[tokio::test]
async fn a_key_acting_as_the_operator_role_by_name_is_accepted() {
    let fx = fixture("named");
    let key = fx.key_by(&fx.member, Some("operator"));
    fx.seed_rung_pools();
    let set = fx
        .rungs(
            &key,
            json!({"action": "set", "rung": "main", "command": "true"}),
        )
        .await;
    assert_eq!(set["main"], "true");
}

#[tokio::test]
async fn a_bad_rung_or_command_is_refused_and_changes_nothing() {
    let fx = fixture("blank");
    let key = fx.key_by(&fx.member, None);
    fx.seed_rung_pools();
    fx.rungs(
        &key,
        json!({"action": "set", "rung": "need", "command": "keep me"}),
    )
    .await;
    let before = fx.config_text();
    let message = fx
        .refused(
            &key,
            json!({"action": "set", "rung": "need", "command": "   "}),
        )
        .await;
    assert!(message.contains("must not be empty"), "{message}");
    let message = fx
        .refused(
            &key,
            json!({"action": "set", "rung": "deploy", "command": "x"}),
        )
        .await;
    assert!(message.contains("unknown rung `deploy`"), "{message}");
    let message = fx
        .refused(&key, json!({"action": "set", "command": "x"}))
        .await;
    assert!(message.contains("rung"), "{message}");
    let message = fx
        .refused(&key, json!({"action": "set", "rung": "need"}))
        .await;
    assert!(message.contains("command"), "{message}");
    assert_eq!(fx.config_text(), before);
    let shown = fx.rungs(&key, json!({"action": "show"})).await;
    assert_eq!(shown["need"], "keep me");
}

#[tokio::test]
async fn a_set_with_no_effective_rung_pools_is_refused_with_the_guard_error() {
    let fx = fixture("nopools");
    let key = fx.key_by(&fx.member, None);
    let before = fx.config_text();
    let message = fx
        .refused(
            &key,
            json!({"action": "set", "rung": "need", "command": "cargo test"}),
        )
        .await;
    assert!(message.contains("rolled back"), "{message}");
    assert!(message.contains("effective pools are empty"), "{message}");
    assert!(
        !message.contains("cargo test"),
        "the command is not echoed as accepted"
    );
    assert_eq!(fx.config_text(), before);
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
        json!({"action": "set", "rung": "need", "command": "evil"}),
        json!({"action": "show"}),
    ] {
        let (status, value) = fx
            .post(
                &bearer,
                "tools/call",
                json!({"name": "pump_rungs", "arguments": arguments}),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
        let message = value["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("identity tokens are not accepted on /v1/mcp"),
            "{message}"
        );
        assert!(message.contains("server key"), "{message}");
    }
    assert!(!fx.config_text().contains("evil"));
}

#[tokio::test]
async fn a_key_that_is_not_the_operators_is_refused() {
    let fx = fixture("others");
    // A key acting as a role that is not of kind operator.
    let hand = fx.key_by(&fx.member, Some("hand"));
    let message = fx
        .refused(
            &hand,
            json!({"action": "set", "rung": "need", "command": "evil"}),
        )
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
        json!({"action": "set", "rung": "need", "command": "evil"}),
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
    let key = server_key(&state, &project, "alice", Some("operator"));
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
