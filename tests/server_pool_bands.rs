//! The `pool_bands` MCP tool: operator-only show, set and unset of one pool's
//! band table in a hosted project, through the vivi library's config editor.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi::mailspace::{Mailspace, RoleUpdate};
use vivi::server::grants::{GrantLevel, GrantRecord};
use vivi::server::{AppState, app_state, hosted, router};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-pool-bands-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Fx {
    app: Router,
    state: AppState,
    data: PathBuf,
    root: PathBuf,
    project: String,
    /// The host's own member, which the operator's key authenticates as.
    member: String,
}

const POOLS: &str = r#"
# the host's pools
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
[pools.main.local]
root = "~/lanes"

[pools.second]
provider = "local"
slots = 2
[pools.second.harness]
name = "pi"
run = "pi --model {model}"
models = ["second/model"]
[pools.second.local]
root = "~/lanes2"
"#;

/// A served host root with two pools and `operator` and `hand` roles.
fn fixture(name: &str) -> Fx {
    fixture_with(name, "")
}

/// The same fixture with extra config text appended, for tests that need a
/// `[pump]` table (the config write guard checks the pump's control mode).
fn fixture_with(name: &str, extra: &str) -> Fx {
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
    text.push_str(POOLS);
    text.push_str(extra);
    std::fs::write(&toml, text).unwrap();
    let data = base.join("data");
    let state = app_state(data.clone(), None, None).expect("app state");
    let report = hosted::reconcile(
        &state.projects,
        &state.identities,
        &BTreeSet::from([root.clone()]),
    );
    assert!(report.refused.is_empty(), "{report:?}");
    Fx {
        app: router(state.clone()),
        data,
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

    async fn bands(&self, token: &str, arguments: Value) -> Value {
        let value = self
            .rpc(
                token,
                "tools/call",
                json!({"name": "pool_bands", "arguments": arguments}),
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
                json!({"name": "pool_bands", "arguments": arguments}),
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

impl Fx {
    /// The mailspace config as a JSON value, with `pools.main.bands` removed.
    fn without_main_bands(&self) -> Value {
        let ms = Mailspace::open_root(self.root.clone()).unwrap();
        let mut value = serde_json::to_value(&ms.config).unwrap();
        value["pools"]["main"]
            .as_object_mut()
            .unwrap()
            .remove("bands");
        value
    }
}

#[tokio::test]
async fn show_returns_the_table_and_the_harness_models() {
    let fx = fixture("show");
    let key = fx.key_by(&fx.member, None);
    let listed = fx.rpc(&key, "tools/list", json!({})).await;
    let tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "pool_bands")
        .expect("pool_bands is listed");
    assert_eq!(
        tool["inputSchema"]["properties"]["action"]["enum"],
        json!(["show", "set", "unset"])
    );

    let shown = fx
        .bands(&key, json!({"action": "show", "pool": "main"}))
        .await;
    assert_eq!(
        shown,
        json!({
            "pool": "main",
            "bands": {"P1": [{"model": "zai/glm-5.3-flash", "reasoning": "low"}]},
            "harness_models": ["zai/glm-5.3-flash", "other/model"],
        })
    );
    let second = fx
        .bands(&key, json!({"action": "show", "pool": "second"}))
        .await;
    assert_eq!(second["bands"], json!({}));
    assert_eq!(second["harness_models"], json!(["second/model"]));
}

#[tokio::test]
async fn set_writes_only_the_bands_table_of_that_pool() {
    let fx = fixture("set");
    let key = fx.key_by(&fx.member, Some("operator"));
    let before_text = fx.config_text();
    let before = fx.without_main_bands();

    let set = fx
        .bands(
            &key,
            json!({"action": "set", "pool": "main", "band": "P2", "model": "other/model"}),
        )
        .await;
    assert_eq!(
        set["bands"],
        json!({
            "P1": [{"model": "zai/glm-5.3-flash", "reasoning": "low"}],
            "P2": [{"model": "other/model", "reasoning": "xhigh"}],
        })
    );
    // Replacing a band, with an explicit reasoning and a lowercase key.
    let set = fx
        .bands(
            &key,
            json!({
                "action": "set", "pool": "main", "band": "p1",
                "model": "other/model", "reasoning": "high"
            }),
        )
        .await;
    assert_eq!(
        set["bands"]["P1"],
        json!([{"model": "other/model", "reasoning": "high"}])
    );

    let shown = fx
        .bands(&key, json!({"action": "show", "pool": "main"}))
        .await;
    assert_eq!(shown["bands"], set["bands"]);
    let text = fx.config_text();
    assert!(text.contains("# the host's pools"), "comments kept: {text}");
    assert_ne!(text, before_text);
    assert_eq!(
        fx.without_main_bands(),
        before,
        "nothing but pools.main.bands changed"
    );
    let second = fx
        .bands(&key, json!({"action": "show", "pool": "second"}))
        .await;
    assert_eq!(second["bands"], json!({}));
}

#[tokio::test]
async fn set_refuses_a_model_outside_harness_models_an_unknown_pool_and_a_bad_band() {
    let fx = fixture("refuse");
    let key = fx.key_by(&fx.member, None);
    let before = fx.config_text();

    let message = fx
        .refused(
            &key,
            json!({"action": "set", "pool": "main", "band": "P2", "model": "evil/model"}),
        )
        .await;
    assert!(message.contains("harness.models"), "{message}");
    assert!(
        message.contains("zai/glm-5.3-flash, other/model"),
        "names the allowed list: {message}"
    );
    // A model of another pool's harness is not allowed here.
    let message = fx
        .refused(
            &key,
            json!({"action": "set", "pool": "main", "band": "P2", "model": "second/model"}),
        )
        .await;
    assert!(message.contains("harness.models"), "{message}");

    let message = fx
        .refused(
            &key,
            json!({"action": "set", "pool": "nope", "band": "P2", "model": "other/model"}),
        )
        .await;
    assert!(message.contains("unknown pool"), "{message}");
    assert!(
        message.contains("main, second"),
        "names the pools: {message}"
    );
    let message = fx
        .refused(&key, json!({"action": "show", "pool": "nope"}))
        .await;
    assert!(message.contains("main, second"), "{message}");

    for band in ["P6", "P2-S0", "7", "bands"] {
        let message = fx
            .refused(
                &key,
                json!({"action": "set", "pool": "main", "band": band, "model": "other/model"}),
            )
            .await;
        assert!(
            message.contains("P0, P1, P2, P3, P4, P5"),
            "{band}: {message}"
        );
    }
    let message = fx
        .refused(
            &key,
            json!({
                "action": "set", "pool": "main", "band": "P2",
                "model": "other/model", "reasoning": "not valid!"
            }),
        )
        .await;
    assert!(message.contains("reasoning"), "{message}");
    let message = fx
        .refused(&key, json!({"action": "set", "pool": "main", "band": "P2"}))
        .await;
    assert!(message.contains("model"), "{message}");
    let message = fx
        .refused(&key, json!({"action": "set", "pool": "main"}))
        .await;
    assert!(message.contains("band"), "{message}");
    let message = fx.refused(&key, json!({"action": "show"})).await;
    assert!(message.contains("pool"), "{message}");
    let message = fx
        .refused(&key, json!({"action": "clear", "pool": "main"}))
        .await;
    assert!(message.contains("show, set, or unset"), "{message}");
    assert_eq!(fx.config_text(), before, "every refusal changed nothing");
}

#[tokio::test]
async fn unset_removes_the_band_and_refuses_one_that_is_not_set() {
    let fx = fixture("unset");
    let key = fx.key_by(&fx.member, None);
    let before = fx.without_main_bands();
    let message = fx
        .refused(
            &key,
            json!({"action": "unset", "pool": "main", "band": "P3"}),
        )
        .await;
    assert!(message.contains("P3"), "{message}");
    let unset = fx
        .bands(
            &key,
            json!({"action": "unset", "pool": "main", "band": "P1"}),
        )
        .await;
    assert_eq!(unset["bands"], json!({}));
    let shown = fx
        .bands(&key, json!({"action": "show", "pool": "main"}))
        .await;
    assert_eq!(shown["bands"], json!({}));
    assert!(!fx.config_text().contains("zai/glm-5.3-flash\"\nreasoning"));
    assert_eq!(fx.without_main_bands(), before);
    // Nothing left to unset: refused, naming what is missing.
    let message = fx
        .refused(
            &key,
            json!({"action": "unset", "pool": "main", "band": "P1"}),
        )
        .await;
    assert!(message.contains("bands"), "{message}");
}

fn audit_lines(dir: &Path) -> Vec<Value> {
    fs::read_to_string(dir.join("audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn hold_pump_lock(root: &Path) -> std::fs::File {
    let home = root.join(".vivi/pump");
    fs::create_dir_all(&home).unwrap();
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("lock"))
        .unwrap();
    lock.try_lock().unwrap();
    lock
}

fn backup_names(root: &Path) -> Vec<String> {
    fs::read_dir(root.join(".vivi"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".bak."))
        .collect()
}

/// `set` and `unset` write under the config write guard: refused while the
/// pump runs with the guard's message, one refused audit line each, and
/// writable while the pump is paused, leaving a backup and one ok line.
#[tokio::test]
async fn set_and_unset_write_through_the_guard() {
    let fx = fixture_with("guard", "\n[pump]\ntick = 20\n");
    let key = fx.key_by(&fx.member, None);
    let before = fx.config_text();
    vivi::pump::control::set_project_mode(&fx.root, vivi::pump::control::PumpMode::Running)
        .unwrap();
    let lock = hold_pump_lock(&fx.root);
    for arguments in [
        json!({"action": "set", "pool": "main", "band": "P2", "model": "other/model"}),
        json!({"action": "unset", "pool": "main", "band": "P1"}),
    ] {
        let message = fx.refused(&key, arguments).await;
        assert!(message.contains("the pump is running"), "{message}");
        assert_eq!(fx.config_text(), before);
    }
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.iter().all(|line| line["tool"] == "pool_bands"));
    assert!(lines.iter().all(|line| line["outcome"] == "refused"));
    drop(lock);

    vivi::pump::control::set_project_mode(&fx.root, vivi::pump::control::PumpMode::Paused).unwrap();
    let _lock = hold_pump_lock(&fx.root);
    let set = fx
        .bands(
            &key,
            json!({"action": "set", "pool": "main", "band": "P2", "model": "other/model"}),
        )
        .await;
    assert_eq!(
        set["bands"]["P2"],
        json!([{"model": "other/model", "reasoning": "xhigh"}])
    );
    assert!(fx.config_text().contains("other/model"));
    assert!(
        !backup_names(&fx.root).is_empty(),
        "a guarded write keeps a backup"
    );
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[2]["tool"], "pool_bands");
    assert_eq!(lines[2]["outcome"], "ok");
}

#[tokio::test]
async fn a_member_token_is_refused_and_changes_nothing() {
    let fx = fixture("token");
    let before = fx.config_text();
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
        json!({"action": "set", "pool": "main", "band": "P2", "model": "other/model"}),
        json!({"action": "unset", "pool": "main", "band": "P1"}),
        json!({"action": "show", "pool": "main"}),
    ] {
        let (status, value) = fx
            .post(
                &bearer,
                "tools/call",
                json!({"name": "pool_bands", "arguments": arguments}),
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
    assert_eq!(fx.config_text(), before);
}

#[tokio::test]
async fn a_key_that_is_not_the_operators_is_refused() {
    let fx = fixture("others");
    let before = fx.config_text();
    // A key acting as a role that is not of kind operator.
    let hand = fx.key_by(&fx.member, Some("hand"));
    let message = fx
        .refused(
            &hand,
            json!({"action": "set", "pool": "main", "band": "P2", "model": "other/model"}),
        )
        .await;
    assert!(message.contains("kind operator"), "{message}");
    let message = fx
        .refused(&hand, json!({"action": "show", "pool": "main"}))
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
        json!({"action": "set", "pool": "main", "band": "P2", "model": "other/model"}),
        json!({"action": "unset", "pool": "main", "band": "P1"}),
        json!({"action": "show", "pool": "main"}),
    ] {
        let message = fx.refused(&bobs, arguments).await;
        assert!(message.contains("host's own member"), "{message}");
    }
    assert_eq!(fx.config_text(), before);
}

#[tokio::test]
async fn a_project_that_is_not_hosted_is_refused() {
    let base = scratch("owned");
    let data = base.join("data");
    let state = app_state(data.clone(), None, None).expect("app state");
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
        data,
        root: base,
        project,
        member: "alice".into(),
    };
    let message = fx
        .refused(&key, json!({"action": "show", "pool": "main"}))
        .await;
    assert!(message.contains("hosted project"), "{message}");
}
