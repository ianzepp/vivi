//! The D4 read-only MCP tools: hosted-project views (pump log, flow, config,
//! pools, roles, charters, pump targets) and the host reads (server_version,
//! project_list). Every project tool answers what its vivi library function
//! returns on the same fixture — the CLI's `--json` body.

use std::collections::BTreeSet;
use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi::mailspace::{Mailspace, RoleUpdate};
use vivi::pool::config as pool_config;
use vivi::pump::config;
use vivi::pump::config::targets;
use vivi::pump::flow::def::FlowKind;
use vivi::pump::flow::view as flow_view;
use vivi::pump::logview::{self, LogFile, LogFilter};
use vivi_server::grants::{GrantLevel, GrantRecord};
use vivi_server::{AppState, app_state, hosted, router};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-mcp-views-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const CONFIG: &str = r#"
[pump]
tick = 20

[pump.targets.web]
path = "web"
main = "main"

[pump.targets.web.rungs]
task = "cargo test --quiet"

[pools.main]
provider = "local"
slots = 4
[[pools.main.bands.P1]]
model = "zai/glm-5.3-flash"
reasoning = "low"
[pools.main.harness]
name = "pi"
run = "pi --model {model}"
models = ["zai/glm-5.3-flash", "other/model"]
"#;

/// One served host root: operator, mind and hand roles, a charter for the
/// operator, pump config with one target and one pool, and pump log files.
struct Fx {
    app: Router,
    state: AppState,
    root: PathBuf,
    project: String,
    /// The host's own member, which the operator's key authenticates as.
    member: String,
}

fn fixture(name: &str) -> Fx {
    let base = scratch(name);
    let root = base.join("hosts").join("app");
    let mut ms = Mailspace::init(Some(&root)).unwrap();
    ms.ensure_operator_role().unwrap();
    for (role, kind) in [("mind", "mind"), ("hand", "hand")] {
        ms.add_role(
            role,
            RoleUpdate {
                kind: Some(Some(kind.into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    }
    ms.set_charter("operator", "guide the pump\n").unwrap();
    let toml = root.join(".vivi").join("mailspace.toml");
    let mut text = std::fs::read_to_string(&toml).unwrap();
    text.push_str(CONFIG);
    std::fs::write(&toml, text).unwrap();
    // The pump home the log tools read: `<root>/.vivi/pump` by default.
    let home = root.join(".vivi").join("pump");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("log.txt"), LOG).unwrap();
    std::fs::write(home.join("exceptions.txt"), EXCEPTIONS).unwrap();
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

const LOG: &str = "tick 1 unit aabbccdd ok\ntick 2 unit aabbccdd failed\ntick 3 unit \
11223344 ok\nskip this line\ntick 4 warm\n";
const EXCEPTIONS: &str = "boom 1 aabbccdd\nboom 2 deadbeef\n";

/// A server key minted as `identity` on the fixture's project.
fn key(fx: &Fx, member: &str, identity: Option<&str>) -> String {
    let level = match identity {
        None | Some("operator") => GrantLevel::Operator,
        Some(_) => GrantLevel::Member,
    };
    let tag = identity.unwrap_or("default");
    fx.state
        .server_keys
        .mint(
            &format!("{member}.{tag}@{}", fx.project),
            false,
            vec![GrantRecord {
                project_id: fx.project.clone(),
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

impl Fx {
    /// POST /v1/mcp as `bearer`; `tools/call` gains the fixture's project.
    async fn post(&self, bearer: &str, method: &str, mut params: Value) -> (StatusCode, Value) {
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
                    .header("authorization", format!("Bearer {bearer}"))
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

    async fn rpc(&self, bearer: &str, method: &str, params: Value) -> Value {
        let (status, value) = self.post(bearer, method, params).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value
    }

    async fn call(&self, bearer: &str, tool: &str, arguments: Value) -> Value {
        let value = self
            .rpc(
                bearer,
                "tools/call",
                json!({"name": tool, "arguments": arguments}),
            )
            .await;
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        assert_ne!(value["result"]["isError"], true, "{tool}: {text}");
        serde_json::from_str(text).unwrap()
    }

    async fn refused(&self, bearer: &str, tool: &str, arguments: Value) -> String {
        let value = self
            .rpc(
                bearer,
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

const PROJECT_TOOLS: [&str; 10] = [
    "pump_log",
    "flow_show",
    "config_show",
    "config_validate",
    "pool_list",
    "pool_show",
    "role_list",
    "role_show",
    "charter_show",
    "target_list",
];

#[tokio::test]
async fn the_tools_are_listed_with_a_required_project() {
    let fx = fixture("listed");
    let key = key(&fx, &fx.member, None);
    let listed = fx.rpc(&key, "tools/list", json!({})).await;
    let tools = listed["result"]["tools"].as_array().unwrap().clone();
    for name in PROJECT_TOOLS {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} is listed"));
        let description = tool["description"].as_str().unwrap();
        assert!(
            description.contains("operator only"),
            "{name}: {description}"
        );
        assert!(
            tool["inputSchema"]["required"]
                .as_array()
                .unwrap()
                .contains(&json!("project")),
            "{name} requires project"
        );
    }
    for name in ["server_version", "project_list"] {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} is listed"));
        assert!(
            !tool["inputSchema"]["required"]
                .as_array()
                .is_some_and(|required| required.contains(&json!("project"))),
            "{name} takes no project"
        );
    }
}

#[tokio::test]
async fn pump_log_returns_what_logview_returns() {
    let fx = fixture("pump-log");
    let key = key(&fx, &fx.member, None);
    // The filtered tail the CLI prints, built by the same function.
    let expected = logview::view(
        &fx.root,
        LogFile::Log,
        2,
        &LogFilter::new(None, Some("aabbccdd")),
    )
    .unwrap();
    assert_eq!(expected.len(), 2);
    let answer = fx
        .call(
            &key,
            "pump_log",
            json!({"file": "log", "lines": 2, "filter": "aabbccdd"}),
        )
        .await;
    assert_eq!(answer["file"], "log");
    assert_eq!(answer["lines"], json!(expected));

    let exceptions = fx
        .call(&key, "pump_log", json!({"file": "exceptions"}))
        .await;
    let expected = logview::view(
        &fx.root,
        LogFile::Exceptions,
        50,
        &LogFilter::new(None, None),
    )
    .unwrap();
    assert_eq!(exceptions["lines"], json!(expected));

    let message = fx.refused(&key, "pump_log", json!({"file": "stale"})).await;
    assert!(message.contains("log or exceptions"), "{message}");
}

#[tokio::test]
async fn flow_show_answers_the_flow_view() {
    let fx = fixture("flow");
    let key = key(&fx, &fx.member, None);
    let flow = flow_view(&fx.root, FlowKind::Build, None).unwrap();
    let answer = fx.call(&key, "flow_show", json!({"kind": "build"})).await;
    assert_eq!(answer["kind"], "build");
    assert_eq!(answer["mermaid"], json!(flow.mermaid));
    assert_eq!(answer["version"], vivi::pump::flow::registry::FLOW_VERSION);
    assert!(answer.get("runs").is_none(), "{answer}");

    // A need with no pump.db yet: runs is null, as the CLI's --json prints.
    let answer = fx
        .call(
            &key,
            "flow_show",
            json!({"kind": "repair", "need": "aabbccdd"}),
        )
        .await;
    assert_eq!(answer["runs"], Value::Null, "{answer}");

    let message = fx
        .refused(&key, "flow_show", json!({"kind": "stale"}))
        .await;
    assert!(
        message.contains("build, research, release, or repair"),
        "{message}"
    );
}

#[tokio::test]
async fn config_show_and_validate_answer_the_config_functions() {
    let fx = fixture("config");
    let key = key(&fx, &fx.member, None);
    // The CLI's `config show --json` body, built from the same function.
    let report = config::show(Some(&fx.root), false).unwrap();
    let expected = serde_json::to_value(&report.overview).unwrap();
    let answer = fx.call(&key, "config_show", json!({})).await;
    assert_eq!(answer, expected);

    let report = config::validate(Some(&fx.root)).unwrap();
    let expected = config::issues_json(&report.issues);
    assert!(!expected.as_array().unwrap().is_empty());
    let answer = fx.call(&key, "config_validate", json!({})).await;
    assert_eq!(answer, expected);

    let message = fx
        .refused(&key, "config_show", json!({"resolved": true}))
        .await;
    assert!(message.contains("free slots"), "{message}");
    let message = fx
        .refused(&key, "pool_show", json!({"name": "main", "resolved": true}))
        .await;
    assert!(message.contains("free slots"), "{message}");
}

#[tokio::test]
async fn pool_list_and_pool_show_answer_the_pool_views() {
    let fx = fixture("pools");
    let key = key(&fx, &fx.member, None);
    let expected = serde_json::to_value(&pool_config::list_view(Some(&fx.root)).unwrap()).unwrap();
    let answer = fx.call(&key, "pool_list", json!({})).await;
    assert_eq!(answer, expected);
    assert_eq!(answer.as_array().unwrap().len(), 1);

    let view = pool_config::show_view(Some(&fx.root), "main", false).unwrap();
    assert_eq!(view.row.free, None);
    let answer = fx.call(&key, "pool_show", json!({"name": "main"})).await;
    assert_eq!(answer, serde_json::to_value(&view.row).unwrap());
    assert_eq!(answer["free"], Value::Null);
    assert_eq!(answer["bands"]["P1"][0]["model"], "zai/glm-5.3-flash");

    let message = fx.refused(&key, "pool_show", json!({"name": "nope"})).await;
    assert!(message.contains("unknown pool 'nope'"), "{message}");
}

#[tokio::test]
async fn role_and_charter_views_answer_the_mailspace() {
    let fx = fixture("roles");
    let key = key(&fx, &fx.member, None);
    let ms = Mailspace::open_root(fx.root.clone()).unwrap();
    let expected = serde_json::to_value(&ms.list_role_views().unwrap()).unwrap();
    let answer = fx.call(&key, "role_list", json!({})).await;
    assert_eq!(answer, expected);

    let expected = serde_json::to_value(&ms.role_view("hand").unwrap()).unwrap();
    let answer = fx.call(&key, "role_show", json!({"role": "hand"})).await;
    assert_eq!(answer, expected);

    // The charter envelope `role charter show --json` prints.
    let charter = ms.read_charter("operator").unwrap();
    let answer = fx
        .call(&key, "charter_show", json!({"role": "operator"}))
        .await;
    assert_eq!(answer["name"], "operator");
    assert_eq!(answer["address"], ms.address_for("operator"));
    assert_eq!(answer["has_charter"], true);
    assert_eq!(answer["charter"], json!(charter));

    let message = fx
        .refused(&key, "role_show", json!({"role": "nobody"}))
        .await;
    assert!(message.contains("unknown"), "{message}");
}

#[tokio::test]
async fn target_list_answers_the_configured_targets() {
    let fx = fixture("targets");
    let key = key(&fx, &fx.member, None);
    let rows = targets::list(&fx.root).unwrap();
    assert_eq!(rows.len(), 1);
    let answer = fx.call(&key, "target_list", json!({})).await;
    assert_eq!(
        answer,
        json!([{
            "id": "web",
            "path": "web",
            "main": "main",
            "rungs": ["task"],
        }]),
        "{answer}; rows: {rows:?}"
    );
}

#[tokio::test]
async fn the_host_read_tools_answer_version_and_hosted_projects() {
    let mut fx = fixture("host");
    let key = key(&fx, &fx.member, None);
    // A granted key is not a host principal.
    for name in ["server_version", "project_list"] {
        let message = fx.refused(&key, name, json!({})).await;
        assert!(message.contains("host tool"), "{name}: {message}");
    }
    fx.state.host_operator_token = Some("host-tok".into());
    let app = router(fx.state.clone());
    let fx = Fx { app, ..fx };
    let version = fx.call("host-tok", "server_version", json!({})).await;
    assert_eq!(
        version,
        json!({ "name": "vivi-server", "version": env!("CARGO_PKG_VERSION") })
    );
    let projects = fx.call("host-tok", "project_list", json!({})).await;
    assert_eq!(
        projects,
        json!([{ "project_id": fx.project, "name": "app", "mode": "hosted" }]),
        "{projects}"
    );
}

#[tokio::test]
async fn keys_without_operator_authority_are_refused() {
    let fx = fixture("authority");
    let hand = key(&fx, &fx.member, Some("hand"));
    for name in PROJECT_TOOLS {
        let arguments = match name {
            "pump_log" => json!({"file": "log"}),
            "flow_show" => json!({"kind": "build"}),
            "pool_show" => json!({"name": "main"}),
            "role_show" | "charter_show" => json!({"role": "hand"}),
            _ => json!({}),
        };
        let message = fx.refused(&hand, name, arguments).await;
        assert!(message.contains("kind operator"), "{name}: {message}");
    }
    fx.state
        .identities
        .register(
            &fx.project,
            "bob",
            &spki_pem(9),
            "0c0c0c0c-1111-4222-8333-444444444444",
        )
        .unwrap();
    let bobs = key(&fx, "bob", Some("operator"));
    let message = fx.refused(&bobs, "role_list", json!({})).await;
    assert!(message.contains("host's own member"), "{message}");
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
            "0d0d0d0d-1111-4222-8333-444444444444",
        )
        .ok();
    let owned = state
        .server_keys
        .mint(
            &format!("alice@{project}"),
            false,
            vec![GrantRecord {
                project_id: project.clone(),
                member: "alice".into(),
                identity: Some("operator".into()),
                level: GrantLevel::Operator,
            }],
            "test",
            "alice",
        )
        .unwrap()
        .key;
    let fx = Fx {
        app: router(state.clone()),
        state,
        root: base,
        project,
        member: "alice".into(),
    };
    let message = fx.refused(&owned, "role_list", json!({})).await;
    assert!(message.contains("hosted project"), "{message}");
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
