//! `create_project` on `POST /v1/mcp`: the host operator token, a created
//! project that is served at once, and the failure modes. `POST /v1/host/mcp`
//! is gone.

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
    data: PathBuf,
}

fn fixture(name: &str, token: Option<&str>, configured: bool) -> Fx {
    isolate_registry();
    let base = std::env::temp_dir().join(format!("vivi-server-host-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (projects, lanes) = (base.join("projects"), base.join("lanes"));
    let data = base.join("data");
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::create_dir_all(&lanes).unwrap();
    let mut state = app_state(data.clone(), None, None);
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
        data,
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
        self.post("/v1/mcp", bearer, body).await
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
async fn the_host_route_is_gone_and_mcp_has_no_built_in_host_without_the_token() {
    let fx = fixture("disabled", None, true);
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}});
    for bearer in [None, Some(TOKEN), Some("")] {
        let (status, value) = fx.post("/v1/host/mcp", bearer, body.clone()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{bearer:?} {value}");
        assert!(value.is_null(), "{value}");
    }
    let (status, value) = fx.host(None, "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
    assert_eq!(value["error"]["message"], "bearer token required");
    for bearer in [TOKEN, ""] {
        let (status, value) = fx.host(Some(bearer), "tools/list", json!({})).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{bearer:?} {value}");
        let message = value["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("identity tokens are not accepted on /v1/mcp"),
            "{message}"
        );
        assert!(message.contains("server key"), "{message}");
    }
    assert!(std::fs::read_dir(&fx.projects).unwrap().next().is_none());
}

#[tokio::test]
async fn a_missing_or_wrong_token_is_refused() {
    let fx = fixture("wrong", Some(TOKEN), true);
    let (status, gone) = fx
        .post(
            "/v1/host/mcp",
            Some(TOKEN),
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{gone}");
    assert!(gone.is_null(), "{gone}");

    let (status, value) = fx.host(None, "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
    assert_eq!(value["error"]["message"], "bearer token required");
    for wrong in ["nope", "host-operator-secre", "host-operator-secrets", ""] {
        let (status, value) = fx.host(Some(wrong), "tools/list", json!({})).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{wrong}: {value}");
        let message = value["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("identity tokens are not accepted on /v1/mcp"),
            "{wrong}: {message}"
        );
        assert!(message.contains("server key"), "{wrong}: {message}");
    }
    let call = json!({"name": "create_project", "arguments": {"name": "refused"}});
    let (status, _) = fx.host(Some("nope"), "tools/call", call).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
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
    let create = tools
        .iter()
        .find(|tool| tool["name"] == "create_project")
        .expect("create_project is in the catalog");
    assert_eq!(create["inputSchema"]["required"], json!(["name"]));
    let properties = &create["inputSchema"]["properties"];
    assert!(properties.get("rungs").is_some(), "{listed}");
    assert!(properties.get("gate").is_none(), "{listed}");
    assert!(properties.get("project").is_none(), "{listed}");
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
    assert_eq!(made["git_url"], format!("{PREFIX}fresh/fresh.git"));
    assert_eq!(made["target"], "fresh");
    assert!(fx.lanes.join("fresh/fresh.git").join("HEAD").exists());
    assert!(root.join("fresh").join(".git").is_dir());
    assert!(root.join(".vivi").join("mailspace.toml").exists());
    assert!(made["initial_commit"].is_string(), "{made}");
    assert!(made["mailspace_id"].is_string());
    assert!(made["readiness"]["missing"].is_array());
    let registry = vivi::registry::load().unwrap();
    let flags = registry.get(&root).expect("registered");
    assert!(flags.serve && flags.pump);

    assert_eq!(made["served"], true, "{made}");
    let id = made["project_id"].as_str().unwrap().to_owned();
    assert_eq!(made["mcp_path"], "/v1/mcp");
    let key = made["operator_key"]["key"].as_str().unwrap().to_owned();
    assert!(key.starts_with("vivi_"));
    assert_eq!(made["operator_key"]["identity"], "operator");

    // Served without a restart or a watcher: /v1/mcp answers with `project`,
    // and the minted key is the operator's (pump_list needs one).
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "pump_list", "arguments": {"project": id}}});
    let (status, value) = fx.post("/v1/mcp", Some(&key), body).await;
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
    let key = made["operator_key"]["key"].as_str().unwrap().to_owned();
    let (status, listed) = fx.host(Some(&key), "tools/list", json!({})).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an operator server key lists tools: {listed}"
    );
    let (status, value) = fx
        .host(
            Some(&key),
            "tools/call",
            json!({"name": "create_project", "arguments": {"name": "not-host"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["result"]["isError"], true, "{value}");
    let text = value["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("not a host principal"), "{text}");
    assert!(!fx.projects.join("not-host").exists());

    let id = made["project_id"].as_str().unwrap();
    let legacy = fx
        .state
        .keys
        .mint(id, "bob", "legacy", Some("operator"))
        .unwrap()
        .key;
    let (status, value) = fx.host(Some(&legacy), "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
    assert_eq!(value["error"]["message"], "invalid bearer token");

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
    let (status, value) = fx.host(Some(&token), "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
    let message = value["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("identity tokens are not accepted on /v1/mcp"),
        "{message}"
    );
    assert!(message.contains("server key"), "{message}");
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
    assert!(fx.lanes.join("twice/twice.git").exists());
    assert!(fx.projects.join("twice").join(".vivi").exists());
}

#[tokio::test]
async fn missing_host_configuration_is_a_clear_error() {
    let fx = fixture("unconfigured", Some(TOKEN), false);
    let (errored, value) = fx.create(json!({"name": "nowhere"})).await;
    assert!(errored, "{value}");
    let message = value["error"].as_str().unwrap();
    assert!(
        message.contains("HOST_PROJECTS_ROOT, HOST_LANES_ROOT and HOST_GIT_URL_PREFIX are not set"),
        "every unset variable is named at once: {message}"
    );

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
            .contains("HOST_GIT_URL_PREFIX is not set"),
        "{value}"
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

/// A parent base defining `name`, so a project's `[pump.rungs] pools` can
/// name a real pool. A name that is not a pool is a config error and the
/// guarded write rolls it back.
fn base_pool(projects: &std::path::Path, name: &str) {
    let vivi = projects.join(".vivi");
    std::fs::create_dir_all(&vivi).unwrap();
    std::fs::write(
        vivi.join("mailspace.toml"),
        format!("base = true\n\n[pools.{name}]\nprovider = \"local\"\nslots = 1\n"),
    )
    .unwrap();
}

#[tokio::test]
async fn create_project_seeds_the_given_rungs_and_answers_the_readiness_after() {
    let fx = fixture("rungs", Some(TOKEN), true);
    base_pool(&fx.projects, "rung");
    let (errored, made) = fx
        .create(json!({
            "name": "laddered",
            "rungs": {"need": "make need", "main": "make main", "pools": ["rung"]}
        }))
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
    base_pool(&fx.projects, "rung");
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
async fn create_project_with_rungs_and_pools_lands_every_command() {
    let fx = fixture("rungsandpools", Some(TOKEN), true);
    base_pool(&fx.projects, "rung");
    let (errored, made) = fx
        .create(json!({
            "name": "complete",
            "rungs": {
                "task": "make task",
                "need": "make need",
                "main": "make main",
                "release": "make release",
                "pools": ["rung"]
            }
        }))
        .await;
    assert!(!errored, "{made}");
    assert!(made.get("rungs_error").is_none(), "{made}");

    let root = fx.projects.join("complete").canonicalize().unwrap();
    let space = vivi::mailspace::Mailspace::open_root(root).unwrap();
    let issues = vivi::pump::readiness::assess(&space);
    let rungs = space.config.pump.unwrap().rungs;
    assert_eq!(rungs.task.as_deref(), Some("make task"));
    assert_eq!(rungs.need.as_deref(), Some("make need"));
    assert_eq!(rungs.main.as_deref(), Some("make main"));
    assert_eq!(rungs.release.as_deref(), Some("make release"));
    assert_eq!(rungs.pools, vec!["rung".to_owned()]);
    assert_eq!(
        made["readiness"]["ready"],
        vivi::pump::readiness::is_ready(&issues)
    );
    let listed: Vec<_> = vivi::routing::not_ready(&issues)
        .map(|issue| issue.item.clone().unwrap_or_default())
        .collect();
    let answered: Vec<_> = made["readiness"]["missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["item"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(answered, listed, "{made}");
    assert!(
        answered.iter().all(|item| !item.starts_with("pump.rungs")),
        "set rungs are not missing: {answered:?}"
    );
    let details: Vec<_> = made["readiness"]["missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["detail"].as_str().unwrap_or(""))
        .collect();
    assert!(
        details
            .iter()
            .all(|detail| !detail.contains("effective pools")),
        "rung pools are set, so they are not missing: {details:?}"
    );
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

// --------------------------------------------------------------- lane_create

impl Fx {
    async fn call_tool(&self, name: &str, arguments: Value) -> (bool, Value) {
        let (status, value) = self
            .host(
                Some(TOKEN),
                "tools/call",
                json!({"name": name, "arguments": arguments}),
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

fn audit_lines(data: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(data.join("audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn dir_names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn config_backups(project: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(project.join(".vivi"))
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("mailspace.toml.bak.")
        })
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    paths
}

fn file_prefix(lanes: &std::path::Path) -> String {
    format!("file://{}/", lanes.display())
}

fn lane_arguments(prefix: &str) -> Value {
    json!({
        "name": "laneapp",
        "target": "sandbox",
        "git_url_prefix": prefix,
        "seed": true,
    })
}

async fn project_for_lane(label: &str) -> Fx {
    let fx = fixture(label, Some(TOKEN), true);
    let (errored, made) = fx.create(json!({"name": "laneapp"})).await;
    assert!(!errored, "{made}");
    fx
}

fn assert_host_audit(line: &Value, tool: &str, outcome: &str) {
    assert_eq!(line["tool"], tool);
    assert_eq!(line["outcome"], outcome);
    assert!(line["project_id"].is_null(), "{line}");
    assert_eq!(line["principal"], "host");
}

fn tool_lines(data: &std::path::Path, tool: &str) -> Vec<Value> {
    audit_lines(data)
        .into_iter()
        .filter(|line| line["tool"] == tool)
        .collect()
}

#[tokio::test]
async fn lane_create_is_listed_for_the_host_without_a_project_argument() {
    let fx = fixture("lane-schema", Some(TOKEN), true);
    let (_, listed) = fx.host(Some(TOKEN), "tools/list", json!({})).await;
    let tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "lane_create")
        .expect("lane_create is in the catalog");
    assert!(tool["inputSchema"]["properties"].get("project").is_none());
    assert_eq!(tool["inputSchema"]["required"], json!(["name", "target"]));
    let (errored, made) = fx.create(json!({"name": "lanehost"})).await;
    assert!(!errored, "{made}");
    let key = made["operator_key"]["key"].as_str().unwrap();
    let (status, value) = fx
        .host(
            Some(key),
            "tools/call",
            json!({"name": "lane_create", "arguments": {"name": "lanehost", "target": "sandbox"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["result"]["isError"], true, "{value}");
    let text = value["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("not a host principal"), "{text}");
    let lines = tool_lines(&fx.data, "lane_create");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["outcome"], "refused");
    // A project operator key is not the host principal. The audit names
    // that key: `{member}@{project_id}`, the same principal create_project minted.
    assert_eq!(
        lines[0]["principal"],
        format!(
            "{}@{}",
            made["member"].as_str().unwrap(),
            made["project_id"].as_str().unwrap()
        )
    );
    assert_eq!(lines[0]["key_id"], made["operator_key"]["key_id"]);
    assert!(!fx.lanes.join("lanehost/sandbox.git").exists());
}

#[tokio::test]
async fn lane_create_makes_the_lane_the_checkout_and_the_target() {
    let fx = project_for_lane("lane-add").await;
    let root = fx.projects.join("laneapp").canonicalize().unwrap();
    let prefix = file_prefix(&fx.lanes);
    let (errored, made) = fx.call_tool("lane_create", lane_arguments(&prefix)).await;
    assert!(!errored, "{made}");

    let url = format!("{prefix}laneapp/sandbox.git");
    assert_eq!(made["name"], "laneapp");
    assert_eq!(made["target_id"], "sandbox");
    assert_eq!(made["git_url"], url);
    assert!(made["initial_commit"].is_string(), "{made}");
    assert_eq!(
        made["checkout_path"],
        root.join("sandbox").to_str().unwrap()
    );
    assert!(fx.lanes.join("laneapp/sandbox.git/HEAD").is_file());
    let origin = std::process::Command::new("git")
        .args(["-C", root.join("sandbox").to_str().unwrap()])
        .args(["remote", "get-url", "origin"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&origin.stdout).trim(), url);

    let live = std::fs::read_to_string(root.join(".vivi/mailspace.toml")).unwrap();
    assert!(live.contains("[pump.targets.sandbox]"), "{live}");
    assert!(live.contains(&format!("remote_url = \"{url}\"")), "{live}");
    let ids: Vec<_> = vivi::pump::config::targets::list(&root)
        .unwrap()
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(ids, ["laneapp", "sandbox"]);
    assert!(made["readiness"]["missing"].is_array(), "{made}");

    let lines = tool_lines(&fx.data, "lane_create");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_host_audit(&lines[0], "lane_create", "ok");
    assert!(
        lines[0]["summary"].as_str().unwrap().contains("sandbox"),
        "{lines:?}"
    );
}

#[tokio::test]
async fn a_second_lane_create_for_the_same_target_is_refused() {
    let fx = project_for_lane("lane-again").await;
    let project = fx.projects.join("laneapp");
    let prefix = file_prefix(&fx.lanes);
    let (errored, first) = fx.call_tool("lane_create", lane_arguments(&prefix)).await;
    assert!(!errored, "{first}");
    let backups = config_backups(&project);
    let lanes = dir_names(&fx.lanes.join("laneapp"));
    let (errored, again) = fx.call_tool("lane_create", lane_arguments(&prefix)).await;
    assert!(errored, "{again}");
    let message = again["error"].as_str().unwrap();
    assert!(message.contains("already exists"), "{message}");
    assert!(project.join("sandbox").join(".git").is_dir());
    assert_eq!(
        config_backups(&project),
        backups,
        "a refusal writes nothing"
    );
    assert_eq!(dir_names(&fx.lanes.join("laneapp")), lanes);
    let lines = tool_lines(&fx.data, "lane_create");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_host_audit(&lines[0], "lane_create", "ok");
    assert_host_audit(&lines[1], "lane_create", "refused");
}

#[tokio::test]
async fn delete_project_removes_the_project_and_stops_serving_it() {
    let fx = fixture("delete", Some(TOKEN), true);
    let (errored, made) = fx.create(json!({"name": "doomed"})).await;
    assert!(!errored, "{made}");
    let id = made["project_id"].as_str().unwrap().to_owned();
    let key = made["operator_key"]["key"].as_str().unwrap().to_owned();
    let root = fx.projects.join("doomed").canonicalize().unwrap();

    let (errored, refused) = fx
        .call_tool(
            "delete_project",
            json!({"name": "doomed", "confirm": "nope"}),
        )
        .await;
    assert!(errored, "{refused}");
    assert!(root.is_dir() && vivi::registry::load().unwrap().get(&root).is_some());

    let (errored, deleted) = fx
        .call_tool(
            "delete_project",
            json!({"name": "doomed", "confirm": "doomed"}),
        )
        .await;
    assert!(!errored, "{deleted}");
    assert_eq!(deleted["name"], "doomed");
    assert_eq!(deleted["unregistered"], true);
    assert!(!root.exists() && !fx.lanes.join("doomed").exists());
    assert!(vivi::registry::load().unwrap().get(&root).is_none());
    assert_eq!(deleted["project_id"], id);
    assert_eq!(deleted["keys_revoked"], 1, "{deleted}");
    assert_eq!(deleted["ungranted"].as_array().map(Vec::len), Some(1));
    let (_, listed) = fx
        .host(
            Some(TOKEN),
            "tools/call",
            json!({"name": "project_list", "arguments": {}}),
        )
        .await;
    let text = listed["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        !text.contains(&id),
        "a deleted project is not listed: {text}"
    );

    // No longer served: the project's own operator key reaches nothing.
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "pump_list", "arguments": {"project": id}}});
    let (_, value) = fx.post("/v1/mcp", Some(&key), body).await;
    assert!(
        value["result"]["isError"] == true || value.get("error").is_some(),
        "{value}"
    );

    let lines = tool_lines(&fx.data, "delete_project");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_host_audit(&lines[0], "delete_project", "refused");
    assert_host_audit(&lines[1], "delete_project", "ok");
    assert!(
        lines[1]["summary"].as_str().unwrap().contains("doomed"),
        "{lines:?}"
    );
}

/// A fixture whose lane URLs are `file://` paths, so a target's lane is
/// readable without the host's SSH prefix.
fn file_fixture(name: &str) -> Fx {
    let fx = fixture(name, Some(TOKEN), true);
    let mut state = fx.state.clone();
    state.host.git_url_prefix = Some(file_prefix(&fx.lanes.canonicalize().unwrap()));
    Fx {
        app: router(state.clone()),
        state,
        ..fx
    }
}

async fn call_as(fx: &Fx, key: &str, name: &str, arguments: Value) -> (bool, Value) {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": name, "arguments": arguments}});
    let (status, value) = fx.post("/v1/mcp", Some(key), body).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let text = value["result"]["content"][0]["text"].as_str().unwrap();
    (
        value["result"]["isError"] == true,
        serde_json::from_str(text).unwrap_or(Value::String(text.to_owned())),
    )
}

#[tokio::test]
async fn target_log_and_show_read_the_landed_lane() {
    let fx = file_fixture("tlog");
    let (errored, made) = fx
        .create(json!({"name": "logged", "initial_commit": true}))
        .await;
    assert!(!errored, "{made}");
    let key = made["operator_key"]["key"].as_str().unwrap().to_owned();
    let id = made["project_id"].as_str().unwrap().to_owned();

    let (errored, log) = call_as(&fx, &key, "target_log", json!({"project": id})).await;
    assert!(!errored, "{log}");
    assert_eq!(log["target"], "logged");
    assert_eq!(log["branch"], "vivi/main");
    let commits = log["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 1, "{log}");
    assert_eq!(commits[0]["commit"], made["initial_commit"]);
    assert_eq!(commits[0]["subject"], "Initial commit");

    let (errored, shown) = call_as(
        &fx,
        &key,
        "target_show",
        json!({"project": id, "commit": "main"}),
    )
    .await;
    assert!(!errored, "{shown}");
    assert_eq!(shown["commit"], made["initial_commit"]);
    assert_eq!(shown["patch_truncated"], false);

    let (errored, refused) = call_as(
        &fx,
        &key,
        "target_show",
        json!({"project": id, "commit": "--all"}),
    )
    .await;
    assert!(errored, "an option is not a revision: {refused}");
}

#[tokio::test]
async fn create_project_imports_a_source_with_its_history() {
    let fx = fixture("source", Some(TOKEN), true);
    let src = fx.lanes.parent().unwrap().join("upstream");
    std::fs::create_dir_all(&src).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&src)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    git(&["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("a.txt"), "a").unwrap();
    git(&["add", "a.txt"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "--quiet",
        "-m",
        "a",
    ]);
    let tip = git(&["rev-parse", "HEAD"]);

    let (errored, made) = fx
        .create(json!({"name": "imported", "source": src.to_str().unwrap()}))
        .await;
    assert!(!errored, "{made}");
    assert_eq!(made["imported"], tip);
    let root = fx.projects.join("imported").canonicalize().unwrap();
    assert!(root.join("imported/a.txt").is_file());
}
