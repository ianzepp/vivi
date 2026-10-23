//! The host MCP endpoint (`POST /v1/host/mcp`) and its `create_project`
//! tool: operator-token auth, a created project that is served at once, and
//! the failure modes.

use std::path::PathBuf;
use std::sync::Once;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi_server::mcp::host::HostConfig;
use vivi_server::{AppState, app_state, router};

const TOKEN: &str = "host-operator-secret";
const PREFIX: &str = "git@hub.example.ts.net:";

static ENV: Once = Once::new();

/// One scratch config directory for the whole test binary: the host registry
/// is process-global (`VIVI_CONFIG_DIR`). Every test uses its own names.
fn isolate_registry() {
    ENV.call_once(|| {
        let dir = std::env::temp_dir().join(format!("vivi-server-host-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: runs once, before any test reads the environment.
        unsafe { std::env::set_var("VIVI_CONFIG_DIR", &dir) };
    });
}

struct Fx {
    app: Router,
    state: AppState,
    projects: PathBuf,
    lanes: PathBuf,
}

fn fixture(name: &str, token: Option<&str>, configured: bool) -> Fx {
    isolate_registry();
    let base = std::env::temp_dir().join(format!("vivi-server-host-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (projects, lanes) = (base.join("projects"), base.join("lanes"));
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::create_dir_all(&lanes).unwrap();
    let mut state = app_state(base.join("data"), None, None);
    state.host_operator_token = token.map(Into::into);
    if configured {
        state.host = HostConfig {
            projects_root: Some(projects.clone()),
            lanes_root: Some(lanes.clone()),
            git_url_prefix: Some(PREFIX.into()),
            default_pool: None,
        };
    }
    Fx {
        app: router(state.clone()),
        state,
        projects,
        lanes,
    }
}

impl Fx {
    async fn post(&self, uri: &str, bearer: Option<&str>, body: Value) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(bearer) = bearer {
            request = request.header("authorization", format!("Bearer {bearer}"));
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn host(&self, bearer: Option<&str>, method: &str, params: Value) -> (StatusCode, Value) {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        self.post("/v1/host/mcp", bearer, body).await
    }

    /// Call `create_project`; the tool's own JSON and whether it errored.
    async fn create(&self, arguments: Value) -> (bool, Value) {
        let (status, value) = self
            .host(
                Some(TOKEN),
                "tools/call",
                json!({"name": "create_project", "arguments": arguments}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        (
            value["result"]["isError"] == true,
            serde_json::from_str(text).unwrap(),
        )
    }
}

#[tokio::test]
async fn the_endpoint_is_disabled_without_a_host_operator_token() {
    let fx = fixture("disabled", None, true);
    for bearer in [None, Some(TOKEN), Some("")] {
        let (status, value) = fx.host(bearer, "tools/list", json!({})).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{value}");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("HOST_OPERATOR_TOKEN"),
            "{value}"
        );
    }
    assert!(std::fs::read_dir(&fx.projects).unwrap().next().is_none());
}

#[tokio::test]
async fn a_missing_or_wrong_token_is_refused() {
    let fx = fixture("wrong", Some(TOKEN), true);
    let (status, _) = fx.host(None, "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for wrong in ["nope", "host-operator-secre", "host-operator-secrets", ""] {
        let (status, value) = fx.host(Some(wrong), "tools/list", json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{wrong}: {value}");
    }
    let call = json!({"name": "create_project", "arguments": {"name": "refused"}});
    let (status, _) = fx.host(Some("nope"), "tools/call", call).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!fx.projects.join("refused").exists());
}

#[tokio::test]
async fn the_operator_token_lists_and_initializes() {
    let fx = fixture("list", Some(TOKEN), true);
    let (status, init) = fx.host(Some(TOKEN), "initialize", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(init["result"]["serverInfo"]["name"], "vivi-server");
    let (_, listed) = fx.host(Some(TOKEN), "tools/list", json!({})).await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1, "{listed}");
    assert_eq!(tools[0]["name"], "create_project");
    assert_eq!(tools[0]["inputSchema"]["required"], json!(["name"]));
    let properties = &tools[0]["inputSchema"]["properties"];
    assert!(properties.get("rungs").is_some(), "{listed}");
    assert!(properties.get("gate").is_none(), "{listed}");
}

#[tokio::test]
async fn create_project_makes_the_repo_mailspace_and_registration_and_serves_it() {
    let fx = fixture("create", Some(TOKEN), true);
    let (errored, made) = fx
        .create(json!({"name": "fresh", "initial_commit": true}))
        .await;
    assert!(!errored, "{made}");

    let root = fx.projects.join("fresh").canonicalize().unwrap();
    assert_eq!(made["project_root"], root.to_str().unwrap());
    assert_eq!(made["git_url"], format!("{PREFIX}fresh.git"));
    assert!(fx.lanes.join("fresh.git").join("HEAD").exists());
    assert!(root.join(".vivi").join("mailspace.toml").exists());
    assert!(made["initial_commit"].is_string(), "{made}");
    assert!(made["mailspace_id"].is_string());
    assert!(made["readiness"]["missing"].is_array());
    let registry = vivi::registry::load().unwrap();
    let flags = registry.get(&root).expect("registered");
    assert!(flags.serve && flags.pump);

    assert_eq!(made["served"], true, "{made}");
    let id = made["project_id"].as_str().unwrap();
    assert_eq!(made["mcp_path"], format!("/v1/projects/{id}/mcp"));
    let key = made["operator_key"]["key"].as_str().unwrap();
    assert!(key.starts_with("vivi_"));
    assert_eq!(made["operator_key"]["identity"], "operator");

    // Served without a restart or a watcher: the project endpoint answers,
    // and the minted key is the operator's (pump_list needs one).
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "pump_list", "arguments": {}}});
    let (status, value) = fx
        .post(
            &made["mcp_path"].as_str().unwrap().to_owned(),
            Some(key),
            body,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_ne!(value["result"]["isError"], true, "{value}");
    let text = value["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(serde_json::from_str::<Value>(text).unwrap()["total"], 0);
}

#[tokio::test]
async fn a_project_token_or_key_is_not_the_host_operator() {
    let fx = fixture("member", Some(TOKEN), true);
    let (errored, made) = fx.create(json!({"name": "memberproj"})).await;
    assert!(!errored, "{made}");
    let key = made["operator_key"]["key"].as_str().unwrap();
    let (status, _) = fx.host(Some(key), "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a project API key");

    let id = made["project_id"].as_str().unwrap();
    let token = fx
        .state
        .identities
        .register(
            id,
            "bob",
            &spki_pem(9),
            "0b0b0b0b-1111-4222-8333-444444444444",
        )
        .unwrap()
        .token;
    let (status, _) = fx.host(Some(&token), "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a project member token");
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
async fn serve_false_registers_without_serving() {
    let fx = fixture("noserve", Some(TOKEN), true);
    let (errored, made) = fx.create(json!({"name": "quiet", "serve": false})).await;
    assert!(!errored, "{made}");
    assert_eq!(made["served"], false);
    assert!(made.get("operator_key").is_none() && made.get("project_id").is_none());
    let root = fx.projects.join("quiet").canonicalize().unwrap();
    assert!(!vivi::registry::load().unwrap().get(&root).unwrap().serve);
}

#[tokio::test]
async fn a_duplicate_name_is_refused_and_touches_nothing() {
    let fx = fixture("dup", Some(TOKEN), true);
    let (errored, first) = fx.create(json!({"name": "twice"})).await;
    assert!(!errored, "{first}");
    let (errored, second) = fx.create(json!({"name": "twice"})).await;
    assert!(errored, "{second}");
    assert!(
        second["error"].as_str().unwrap().contains("exists")
            || second["error"].as_str().unwrap().contains("already"),
        "{second}"
    );
    assert!(fx.lanes.join("twice.git").exists());
    assert!(fx.projects.join("twice").join(".vivi").exists());
}

#[tokio::test]
async fn missing_host_configuration_is_a_clear_error() {
    let fx = fixture("unconfigured", Some(TOKEN), false);
    let (errored, value) = fx.create(json!({"name": "nowhere"})).await;
    assert!(errored, "{value}");
    let message = value["error"].as_str().unwrap();
    assert!(message.contains("HOST_PROJECTS_ROOT"), "{message}");

    let mut partial = fx.state.clone();
    partial.host.projects_root = Some(fx.projects.clone());
    partial.host.lanes_root = Some(fx.lanes.clone());
    let fx = Fx {
        app: router(partial.clone()),
        state: partial,
        ..fx
    };
    let (_, value) = fx.create(json!({"name": "nowhere"})).await;
    assert!(
        value["error"]
            .as_str()
            .unwrap()
            .contains("HOST_GIT_URL_PREFIX")
    );
    assert!(!fx.projects.join("nowhere").exists());
}

#[tokio::test]
async fn bad_arguments_are_refused() {
    let fx = fixture("badargs", Some(TOKEN), true);
    let (errored, value) = fx.create(json!({})).await;
    assert!(
        errored && value["error"].as_str().unwrap().contains("name"),
        "{value}"
    );
    let (errored, value) = fx.create(json!({"name": "x", "serve": "yes"})).await;
    assert!(
        errored && value["error"].as_str().unwrap().contains("serve"),
        "{value}"
    );
    let (errored, _) = fx.create(json!({"name": "../escape"})).await;
    assert!(errored);
    assert!(!fx.projects.join("x").exists());
}

#[tokio::test]
async fn create_project_seeds_the_given_rungs_and_answers_the_readiness_after() {
    let fx = fixture("rungs", Some(TOKEN), true);
    let (errored, made) = fx
        .create(json!({"name": "laddered", "rungs": {"need": "make need", "main": "make main"}}))
        .await;
    assert!(!errored, "{made}");
    assert!(made.get("rungs_error").is_none(), "{made}");

    let root = fx.projects.join("laddered").canonicalize().unwrap();
    let space = vivi::mailspace::Mailspace::open_root(root).unwrap();
    let issues = vivi::pump::readiness::assess(&space);
    let rungs = space.config.pump.unwrap().rungs;
    assert_eq!(rungs.need.as_deref(), Some("make need"));
    assert_eq!(rungs.main.as_deref(), Some("make main"));
    assert!(rungs.task.is_none() && rungs.release.is_none());

    // The answer is the readiness as it stands after the write.
    assert_eq!(made["readiness"]["ready"], false, "{made}");
    let listed: Vec<_> = vivi::routing::not_ready(&issues)
        .map(|i| i.item.clone().unwrap_or_default())
        .collect();
    let answered: Vec<_> = made["readiness"]["missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["item"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(answered, listed, "{made}");
}

#[tokio::test]
async fn bad_rungs_are_refused_before_anything_is_created() {
    let fx = fixture("badrungs", Some(TOKEN), true);
    let cases = [
        (json!({"deploy": "make"}), "deploy"),
        (json!({"need": "  "}), "need"),
        (json!({"main": 7}), "main"),
        (json!("make need"), "rungs must be an object"),
    ];
    for (rungs, expect) in cases {
        let (errored, value) = fx.create(json!({"name": "norungs", "rungs": rungs})).await;
        assert!(errored, "{value}");
        let message = value["error"].as_str().unwrap();
        assert!(message.contains(expect), "{message}");
        assert!(!fx.projects.join("norungs").exists(), "{message}");
        assert!(!fx.lanes.join("norungs.git").exists(), "{message}");
    }
    let (errored, value) = fx
        .create(json!({"name": "norungs", "rungs": {"x": "y"}}))
        .await;
    assert!(
        errored
            && value["error"]
                .as_str()
                .unwrap()
                .contains("task, need, main, release"),
        "{value}"
    );
}

#[tokio::test]
async fn create_project_writes_rung_timeout_and_pools() {
    let fx = fixture("rungsettings", Some(TOKEN), true);
    let (errored, made) = fx
        .create(json!({
            "name": "timed",
            "rungs": {"need": "make need", "timeout": 900, "pools": ["rung"]}
        }))
        .await;
    assert!(!errored, "{made}");
    assert!(made.get("rungs_error").is_none(), "{made}");

    let root = fx.projects.join("timed").canonicalize().unwrap();
    let raw = std::fs::read_to_string(root.join(".vivi").join("mailspace.toml")).unwrap();
    assert!(raw.contains("timeout = 900"), "{raw}");
    assert!(raw.contains("pools = [\"rung\"]"), "{raw}");
    let space = vivi::mailspace::Mailspace::open_root(root).unwrap();
    let rungs = space.config.pump.unwrap().rungs;
    assert_eq!(rungs.need.as_deref(), Some("make need"));
    assert_eq!(rungs.timeout, Some(900));
    assert_eq!(rungs.pools, vec!["rung".to_owned()]);
}

#[tokio::test]
async fn create_project_writes_rung_timeout_alone() {
    let fx = fixture("rungtimeout", Some(TOKEN), true);
    let (errored, made) = fx
        .create(json!({"name": "onlytimeout", "rungs": {"timeout": 60}}))
        .await;
    assert!(!errored, "{made}");
    assert!(made.get("rungs_error").is_none(), "{made}");
    let root = fx.projects.join("onlytimeout").canonicalize().unwrap();
    let space = vivi::mailspace::Mailspace::open_root(root).unwrap();
    assert_eq!(space.config.pump.unwrap().rungs.timeout, Some(60));
}

#[tokio::test]
async fn bad_rung_timeout_and_pools_are_refused_before_anything_is_created() {
    let fx = fixture("badsettings", Some(TOKEN), true);
    let cases = [
        (json!({"timeout": 0}), "rungs.timeout"),
        (json!({"timeout": -5}), "rungs.timeout"),
        (json!({"timeout": "900"}), "rungs.timeout"),
        (json!({"timeout": 1.5}), "rungs.timeout"),
        (json!({"pools": "rung"}), "rungs.pools"),
        (json!({"pools": [""]}), "rungs.pools"),
        (json!({"pools": ["ok", 3]}), "rungs.pools"),
    ];
    for (rungs, expect) in cases {
        let (errored, value) = fx
            .create(json!({"name": "nosettings", "rungs": rungs}))
            .await;
        assert!(errored, "{value}");
        let message = value["error"].as_str().unwrap();
        assert!(message.contains(expect), "{message}");
        assert!(!fx.projects.join("nosettings").exists(), "{message}");
        assert!(!fx.lanes.join("nosettings.git").exists(), "{message}");
    }
}
