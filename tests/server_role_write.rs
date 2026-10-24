//! Role and charter write tools on `/v1/mcp`: each calls the vivi
//! functions the CLI's `vivi role add`, `vivi role set` and `vivi role
//! charter set` call, is refused while the pump runs, and writes one
//! audit line per call.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi::mailspace::config_edit;
use vivi::mailspace::{Mailspace, RoleUpdate};
use vivi_server::grants::{GrantLevel, GrantRecord};
use vivi_server::{AppState, app_state, hosted, router};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-role-write-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

struct Fx {
    app: Router,
    state: AppState,
    data: PathBuf,
    root: PathBuf,
    project: String,
    member: String,
}

/// The same hosted fixture the config write tests use, with one addition:
/// `[pump] to = "pump"`, so the CLI's default-kind rule (the pump's hand
/// role is a hand) is reachable through a role that does not exist yet.
fn fixture(name: &str) -> Fx {
    let base = scratch(name);
    let root = base.join("hosts").join("app");
    let mut ms = Mailspace::init(Some(&root)).unwrap();
    fs::create_dir_all(root.join("web")).unwrap();
    fs::create_dir_all(root.join("api")).unwrap();
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
    let mut text = fs::read_to_string(&toml).unwrap();
    text.push_str(
        r#"
[pump]
tick = 20
to = "pump"

[pump.targets.web]
path = "web"

[pools.glm]
provider = "local"
slots = 1

[pools.glm.harness]
name = "stub"
run = "echo {model}"
models = ["old/m"]

[pump.rungs]
pools = ["glm"]
"#,
    );
    fs::write(&toml, text).unwrap();
    let data = base.join("data");
    let state = app_state(data.clone(), None, None);
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
        let level = match identity {
            None | Some("operator") => GrantLevel::Operator,
            Some(_) => GrantLevel::Member,
        };
        let tag = identity.unwrap_or("default");
        self.state
            .server_keys
            .mint(
                &format!("{member}.{tag}@{}", self.project),
                false,
                vec![GrantRecord {
                    project_id: self.project.clone(),
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

    async fn call(&self, token: &str, name: &str, arguments: Value) -> Value {
        let value = self.rpc(token, name, arguments).await;
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        assert_ne!(value["result"]["isError"], true, "{text}");
        serde_json::from_str(text).unwrap()
    }

    async fn refused(&self, token: &str, name: &str, arguments: Value) -> String {
        let value = self.rpc(token, name, arguments).await;
        assert_eq!(value["result"]["isError"], true, "{value}");
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str::<Value>(text).unwrap()["error"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn rpc(&self, token: &str, name: &str, mut arguments: Value) -> Value {
        arguments
            .as_object_mut()
            .unwrap()
            .entry("project")
            .or_insert_with(|| json!(self.project.clone()));
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        });
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
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn config_text(&self) -> String {
        fs::read_to_string(self.root.join(".vivi/mailspace.toml")).unwrap()
    }
}

fn config_text(root: &Path) -> String {
    fs::read_to_string(root.join(".vivi/mailspace.toml")).unwrap()
}

/// Each role's current charter, by role name (a role has at most one
/// `.md` file under `.vivi/charters/`; backups are skipped).
fn charter_texts(root: &Path) -> BTreeMap<String, String> {
    let dir = root.join(".vivi/charters");
    let mut out = BTreeMap::new();
    let Ok(entries) = fs::read_dir(&dir) else {
        return out;
    };
    for entry in entries {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".md") && !name.contains(".bak.") {
            out.insert(name, fs::read_to_string(entry.path()).unwrap());
        }
    }
    out
}

fn twin(root: &Path) -> PathBuf {
    let dst = std::env::temp_dir().join(format!(
        "vivi-server-role-write-twin-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    copy_dir(root, &dst);
    dst
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), &to).unwrap();
        }
    }
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

#[tokio::test]
async fn the_tools_are_listed_with_their_arguments() {
    let fx = fixture("list");
    let key = fx.key_by(&fx.member, None);
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}});
    let response = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/mcp")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let listed: Value = serde_json::from_slice(&bytes).unwrap();
    let tools = listed["result"]["tools"].as_array().unwrap();
    let schema = |name: &str| {
        tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} is listed"))["inputSchema"]
            .clone()
    };
    assert_eq!(schema("role_add")["required"], json!(["project", "role"]));
    assert_eq!(schema("role_add")["properties"]["models"]["type"], "array");
    assert_eq!(schema("role_set")["required"], json!(["project", "role"]));
    assert_eq!(
        schema("role_set")["properties"]["clear_pool"]["type"],
        "boolean"
    );
    assert_eq!(
        schema("charter_set")["required"],
        json!(["project", "role", "charter"])
    );
    assert_eq!(
        schema("charter_set")["properties"]["charter"]["type"],
        "string"
    );
}

#[tokio::test]
async fn role_add_matches_the_cli() {
    let fx = fixture("add");
    let key = fx.key_by(&fx.member, None);
    let twin_root = twin(&fx.root);

    // The CLI's calls on the twin: the same RoleUpdate, the guarded
    // `add_role`, then the adoption pass.
    let mut lib = Mailspace::open_root(twin_root.clone()).unwrap();
    let full = RoleUpdate {
        kind: Some(Some("auditor".into())),
        status: Some("parked".into()),
        harness: Some(Some("subagent".into())),
        provider: Some(Some("local".into())),
        model: Some(Some("old/m".into())),
        thinking: Some(Some("xhigh".into())),
        cadence: Some(Some("1h".into())),
        add_labels: vec!["w3".into()],
        pool: Some(Some("glm".into())),
        read_only: Some(Some(false)),
        models: Some(vec!["old/m".into()]),
        ..RoleUpdate::default()
    };
    let file = lib.config_path();
    let (address, _) =
        config_edit::guard_file(&twin_root, &file, || lib.add_role("auditor", full)).unwrap();
    let adopted = lib
        .adopt_held_letters(&lib.resolve_identity("auditor").unwrap())
        .unwrap();

    let got = fx
        .call(
            &key,
            "role_add",
            json!({
                "role": "auditor",
                "kind": "auditor",
                "status": "parked",
                "harness": "subagent",
                "provider": "local",
                "model": "old/m",
                "thinking": "xhigh",
                "cadence": "1h",
                "labels": ["w3"],
                "pool": "glm",
                "read_only": false,
                "models": ["old/m"]
            }),
        )
        .await;
    assert_eq!(got["added"], address);
    assert_eq!(got["adopted"], adopted);
    assert_eq!(
        got["role"],
        serde_json::to_value(lib.role_view("auditor").unwrap()).unwrap()
    );
    assert_eq!(fx.config_text(), config_text(&twin_root));
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["tool"], "role_add");
    assert_eq!(lines[0]["outcome"], "ok");
    assert_eq!(lines[0]["project_id"], fx.project);

    // The CLI's default: the pump's hand role (`[pump] to`) gets kind
    // hand when no kind is given.
    let hand = lib
        .config
        .pump
        .as_ref()
        .map_or("hand", |pump| pump.to.as_str());
    assert_eq!(hand, "pump", "the fixture names the pump's hand role");
    let update = RoleUpdate {
        kind: Some(Some("hand".into())),
        ..RoleUpdate::default()
    };
    let file = lib.config_path();
    let (address, _) =
        config_edit::guard_file(&twin_root, &file, || lib.add_role("pump", update)).unwrap();
    lib.adopt_held_letters(&lib.resolve_identity("pump").unwrap())
        .unwrap();
    let got = fx.call(&key, "role_add", json!({"role": "pump"})).await;
    assert_eq!(got["added"], address);
    assert_eq!(got["role"]["kind"], "hand");
    assert_eq!(fx.config_text(), config_text(&twin_root));
    assert_eq!(audit_lines(&fx.data).len(), 2);

    // A duplicate name is refused with vivi's message, and audited.
    let message = fx.refused(&key, "role_add", json!({"role": "hand"})).await;
    assert!(message.contains("already exists"), "{message}");
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[2]["outcome"], "refused");
    let _ = fs::remove_dir_all(&twin_root);
}

#[tokio::test]
async fn role_set_matches_the_cli() {
    let fx = fixture("set");
    let key = fx.key_by(&fx.member, None);
    let twin_root = twin(&fx.root);
    let mut lib = Mailspace::open_root(twin_root.clone()).unwrap();
    let update = RoleUpdate {
        kind: Some(Some("steward".into())),
        pool: Some(Some("glm".into())),
        status: Some("parked".into()),
        model: Some(Some("old/m".into())),
        thinking: Some(Some("xhigh".into())),
        cadence: Some(Some("30m".into())),
        add_labels: vec!["night".into()],
        read_only: Some(Some(true)),
        models: Some(vec!["old/m".into()]),
        ..RoleUpdate::default()
    };
    let file = lib.config_path();
    let (view, _) =
        config_edit::guard_file(&twin_root, &file, || lib.set_role("hand", update)).unwrap();
    let got = fx
        .call(
            &key,
            "role_set",
            json!({
                "role": "hand",
                "kind": "steward",
                "pool": "glm",
                "status": "parked",
                "model": "old/m",
                "thinking": "xhigh",
                "cadence": "30m",
                "labels": ["night"],
                "read_only": true,
                "models": ["old/m"]
            }),
        )
        .await;
    assert_eq!(got, serde_json::to_value(&view).unwrap());
    assert_eq!(fx.config_text(), config_text(&twin_root));
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["tool"], "role_set");
    assert_eq!(lines[0]["outcome"], "ok");

    // The clear forms, on a fresh open (the CLI is one process per call).
    let mut lib = Mailspace::open_root(twin_root.clone()).unwrap();
    let clear = RoleUpdate {
        kind: Some(None),
        pool: Some(None),
        cadence: Some(None),
        clear_labels: vec!["night".into()],
        read_only: Some(None),
        models: Some(Vec::new()),
        ..RoleUpdate::default()
    };
    let file = lib.config_path();
    let (view, _) =
        config_edit::guard_file(&twin_root, &file, || lib.set_role("hand", clear)).unwrap();
    let got = fx
        .call(
            &key,
            "role_set",
            json!({
                "role": "hand",
                "clear_kind": true,
                "clear_pool": true,
                "clear_cadence": true,
                "clear_labels": ["night"],
                "clear_read_only": true,
                "clear_models": true
            }),
        )
        .await;
    assert_eq!(got, serde_json::to_value(&view).unwrap());
    assert_eq!(fx.config_text(), config_text(&twin_root));
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[1]["outcome"], "ok");

    // The CLI's rule: at least one field, else refused (and audited).
    let message = fx.refused(&key, "role_set", json!({"role": "hand"})).await;
    assert!(message.contains("requires at least one field"), "{message}");
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[2]["outcome"], "refused");
    let _ = fs::remove_dir_all(&twin_root);
}

#[tokio::test]
async fn charter_set_matches_the_cli() {
    let fx = fixture("charter");
    let key = fx.key_by(&fx.member, None);
    let twin_root = twin(&fx.root);
    let mut lib = Mailspace::open_root(twin_root.clone()).unwrap();
    let canonical = lib.resolve_identity("hand").unwrap();
    let file = lib.charter_path(&canonical);
    config_edit::guard_file(&twin_root, &file, || {
        lib.set_charter("hand", "ship the wave\n")
    })
    .unwrap();

    let got = fx
        .call(
            &key,
            "charter_set",
            json!({"role": "hand", "charter": "ship the wave\n"}),
        )
        .await;
    assert_eq!(got["name"], "hand");
    assert_eq!(got["address"], lib.address_for("hand"));
    assert_eq!(got["has_charter"], true);
    assert_eq!(got["charter"], "ship the wave\n");
    assert_eq!(got["bytes"], "ship the wave\n".len());
    assert_eq!(charter_texts(&fx.root), charter_texts(&twin_root));
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["tool"], "charter_set");
    assert_eq!(lines[0]["outcome"], "ok");
    assert_eq!(lines[0]["project_id"], fx.project);

    // An unknown role is refused with vivi's message, and audited.
    let message = fx
        .refused(&key, "charter_set", json!({"role": "nope", "charter": "x"}))
        .await;
    assert!(message.contains("nope"), "{message}");
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[1]["outcome"], "refused");
    let _ = fs::remove_dir_all(&twin_root);
}

#[tokio::test]
async fn writes_are_refused_while_the_pump_runs_and_a_paused_pump_is_writable() {
    let fx = fixture("running");
    let key = fx.key_by(&fx.member, None);
    let before = fx.config_text();
    let charters_before = charter_texts(&fx.root);
    vivi::pump::control::set_project_mode(&fx.root, vivi::pump::control::PumpMode::Running)
        .unwrap();
    let lock = hold_pump_lock(&fx.root);
    let writes = [
        ("role_add", json!({"role": "auditor", "kind": "auditor"})),
        ("role_set", json!({"role": "hand", "pool": "glm"})),
        (
            "charter_set",
            json!({"role": "hand", "charter": "do the work"}),
        ),
    ];
    for (name, arguments) in writes {
        let message = fx.refused(&key, name, arguments).await;
        assert!(message.contains("the pump is running"), "{name}: {message}");
        assert_eq!(fx.config_text(), before, "{name} changed the config");
        assert_eq!(
            charter_texts(&fx.root),
            charters_before,
            "{name} changed a charter"
        );
    }
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines.iter().all(|line| line["outcome"] == "refused"));
    drop(lock);

    vivi::pump::control::set_project_mode(&fx.root, vivi::pump::control::PumpMode::Paused).unwrap();
    let _lock = hold_pump_lock(&fx.root);
    // `scribe` is no pump seat, so its charter needs no v20 reply envelope
    // (a seat role's plain-text charter is refused as a new config error).
    let got = fx
        .call(&key, "role_add", json!({"role": "scribe", "kind": "hand"}))
        .await;
    assert_eq!(got["role"]["name"], "scribe");
    let got = fx
        .call(
            &key,
            "role_set",
            json!({"role": "scribe", "status": "parked"}),
        )
        .await;
    assert_eq!(got["status"], "parked");
    let got = fx
        .call(
            &key,
            "charter_set",
            json!({"role": "scribe", "charter": "do the work"}),
        )
        .await;
    assert_eq!(got["bytes"], "do the work".len());
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 6, "{lines:?}");
    assert!(lines[3..].iter().all(|line| line["outcome"] == "ok"));
}

#[tokio::test]
async fn a_key_that_is_not_the_operators_is_refused() {
    let fx = fixture("hand");
    let hand = fx.key_by(&fx.member, Some("hand"));
    let before = fx.config_text();
    let writes = [
        ("role_add", json!({"role": "x", "kind": "hand"})),
        ("role_set", json!({"role": "hand", "pool": "glm"})),
        ("charter_set", json!({"role": "hand", "charter": "x"})),
    ];
    for (name, arguments) in writes {
        let message = fx.refused(&hand, name, arguments).await;
        assert!(message.contains("kind operator"), "{name}: {message}");
    }
    assert_eq!(fx.config_text(), before);
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines.iter().all(|line| line["outcome"] == "refused"));
}
