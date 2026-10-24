//! Config write tools on `/v1/mcp`: each calls the vivi function the CLI
//! calls, is refused while the pump runs, and writes one audit line.
//!
//! `pool_allow_set` writes `pools.<pool>.harness.models`, the allow list
//! `pool_bands` checks. There is no `[pools.<pool>] allow` key.

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
use vivi::pump::config::targets::{self, TargetSpec};
use vivi_server::grants::{GrantLevel, GrantRecord};
use vivi_server::{AppState, app_state, hosted, router};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-config-write-{}-{name}",
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

fn backup_names(root: &Path) -> Vec<String> {
    fs::read_dir(root.join(".vivi"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".bak."))
        .collect()
}

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
        "vivi-server-config-write-twin-{}-{}",
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
    assert!(schema("config_migrate")["properties"]["write"].is_object());
    assert_eq!(
        schema("config_set")["required"],
        json!(["project", "key", "value"])
    );
    assert_eq!(
        schema("config_unset")["required"],
        json!(["project", "key"])
    );
    assert_eq!(
        schema("pool_allow_set")["properties"]["models"]["type"],
        "array"
    );
    assert_eq!(
        schema("target_add")["required"],
        json!(["project", "id", "path"])
    );
    assert_eq!(
        schema("target_remove")["required"],
        json!(["project", "id"])
    );
}

#[tokio::test]
async fn config_set_and_unset_match_the_library() {
    let fx = fixture("set");
    let key = fx.key_by(&fx.member, None);
    let twin_root = twin(&fx.root);
    let lib = config_edit::set_key(&twin_root, "pump.tick", "30").unwrap();
    let got = fx
        .call(
            &key,
            "config_set",
            json!({"key": "pump.tick", "value": "30"}),
        )
        .await;
    assert_eq!(got["key"], "pump.tick");
    assert_eq!(got["value"], lib.value);
    assert_eq!(got["kind"], lib.kind);
    assert_eq!(fx.config_text(), config_text(&twin_root));
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["tool"], "config_set");
    assert_eq!(lines[0]["outcome"], "ok");
    assert_eq!(lines[0]["project_id"], fx.project);

    config_edit::unset_key(&twin_root, "pump.tick").unwrap();
    let got = fx
        .call(&key, "config_unset", json!({"key": "pump.tick"}))
        .await;
    assert_eq!(got, json!({"key": "pump.tick", "unset": true}));
    assert_eq!(fx.config_text(), config_text(&twin_root));
    assert!(!fx.config_text().contains("tick"));
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[1]["tool"], "config_unset");
    assert_eq!(lines[1]["outcome"], "ok");
    let _ = fs::remove_dir_all(&twin_root);
}

#[tokio::test]
async fn config_migrate_dry_run_is_a_read_and_write_matches_the_library() {
    let fx = fixture("migrate");
    let key = fx.key_by(&fx.member, None);
    let before = fx.config_text();
    let dry_twin = twin(&fx.root);
    let dry = vivi::pump::config::migrate(Some(&dry_twin), false).unwrap();
    let got = fx.call(&key, "config_migrate", json!({})).await;
    assert_eq!(got["dry_run"], true);
    assert_eq!(got["plan"], dry.plan);
    assert_eq!(got["diff"], dry.diff);
    assert_eq!(got["charters"], json!([]));
    assert_eq!(fx.config_text(), before);
    assert!(audit_lines(&fx.data).is_empty(), "a dry run is not audited");
    assert!(
        backup_names(&fx.root).is_empty(),
        "a dry run keeps no backup"
    );

    let write_twin = twin(&fx.root);
    let wrote = vivi::pump::config::migrate(Some(&write_twin), true).unwrap();
    let got = fx
        .call(&key, "config_migrate", json!({"write": true}))
        .await;
    assert_eq!(got["dry_run"], false);
    assert_eq!(got["plan"], wrote.plan);
    assert_eq!(got["diff"], wrote.diff);
    let charters: Vec<Value> = wrote
        .charters
        .iter()
        .map(|charter| {
            json!({"role": charter.role, "origin": charter.origin, "bytes": charter.bytes})
        })
        .collect();
    assert_eq!(got["charters"], json!(charters));
    assert_eq!(fx.config_text(), config_text(&write_twin));
    assert_eq!(charter_texts(&fx.root), charter_texts(&write_twin));
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["tool"], "config_migrate");
    assert_eq!(lines[0]["outcome"], "ok");
    let _ = fs::remove_dir_all(&dry_twin);
    let _ = fs::remove_dir_all(&write_twin);
}

#[tokio::test]
async fn pool_allow_set_matches_the_library_and_refuses_a_bad_type() {
    let fx = fixture("allow");
    let key = fx.key_by(&fx.member, None);
    let before = fx.config_text();
    let message = fx
        .refused(
            &key,
            "pool_allow_set",
            json!({"pool": "glm", "models": "m1/a"}),
        )
        .await;
    assert!(message.contains("array of strings"), "{message}");
    let message = fx
        .refused(
            &key,
            "pool_allow_set",
            json!({"pool": "glm", "models": [1, "m1/a"]}),
        )
        .await;
    assert!(message.contains("array of strings"), "{message}");
    // The library's own checks, before anything is written.
    let message = fx
        .refused(
            &key,
            "pool_allow_set",
            json!({"pool": "glm", "models": ["m1/a", "m1/a"]}),
        )
        .await;
    assert!(message.contains("duplicate"), "{message}");
    let message = fx
        .refused(
            &key,
            "pool_allow_set",
            json!({"pool": "glm", "models": ["not a slug!"]}),
        )
        .await;
    assert!(message.contains("characters outside"), "{message}");
    let message = fx
        .refused(
            &key,
            "pool_allow_set",
            json!({"pool": "nope", "models": ["m1/a"]}),
        )
        .await;
    assert!(message.contains("unknown pool"), "{message}");
    assert_eq!(fx.config_text(), before, "every refusal wrote nothing");

    let models: Vec<String> = ["m1/a", "m2/b"]
        .iter()
        .map(|model| model.to_string())
        .collect();
    let twin_root = twin(&fx.root);
    vivi::pool::config_allow::set_allow(&twin_root, "glm", &models).unwrap();
    let got = fx
        .call(
            &key,
            "pool_allow_set",
            json!({"pool": "glm", "models": models}),
        )
        .await;
    assert_eq!(got["pool"], "glm");
    assert_eq!(got["key"], "pools.glm.harness.models");
    assert_eq!(got["models"], json!(["m1/a", "m2/b"]));
    assert_eq!(fx.config_text(), config_text(&twin_root));
    assert!(fx.config_text().contains("m1/a"), "{}", fx.config_text());
    assert!(!fx.config_text().contains("old/m"), "{}", fx.config_text());
    // Five refusals and the successful write: six lines, the last ok.
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 6, "{lines:?}");
    assert!(lines.iter().all(|line| line["tool"] == "pool_allow_set"));
    assert_eq!(lines[5]["outcome"], "ok");
    let _ = fs::remove_dir_all(&twin_root);
}

#[tokio::test]
async fn target_add_and_remove_match_the_library() {
    let fx = fixture("target");
    let key = fx.key_by(&fx.member, None);
    let spec = TargetSpec {
        id: "api".into(),
        path: "api".into(),
        remote_url: Some("https://example.test/api.git".into()),
        main: Some("trunk".into()),
        rungs: vec![("task".into(), "cargo test".into())],
        merge: vec![("remote_limit".into(), "600".into())],
        ..TargetSpec::default()
    };
    let twin_root = twin(&fx.root);
    targets::add(&twin_root, &spec).unwrap();
    let got = fx
        .call(
            &key,
            "target_add",
            json!({
                "id": "api",
                "path": "api",
                "remote_url": "https://example.test/api.git",
                "main": "trunk",
                "rungs": ["task=cargo test"],
                "merge": ["remote_limit=600"]
            }),
        )
        .await;
    assert_eq!(
        got,
        json!({"id": "api", "path": "api", "main": "trunk", "rungs": ["task"]})
    );
    assert_eq!(fx.config_text(), config_text(&twin_root));
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["tool"], "target_add");
    assert_eq!(lines[0]["outcome"], "ok");

    targets::remove(&twin_root, "api").unwrap();
    let got = fx.call(&key, "target_remove", json!({"id": "api"})).await;
    assert_eq!(got, json!({"id": "api", "removed": true}));
    assert_eq!(fx.config_text(), config_text(&twin_root));
    assert!(!fx.config_text().contains("[pump.targets.api]"));
    assert!(fx.config_text().contains("[pump.targets.web]"));
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[1]["tool"], "target_remove");
    assert_eq!(lines[1]["outcome"], "ok");
    let _ = fs::remove_dir_all(&twin_root);
}

#[tokio::test]
async fn writes_are_refused_while_the_pump_runs_and_a_paused_pump_is_writable() {
    let fx = fixture("running");
    let key = fx.key_by(&fx.member, None);
    let before = fx.config_text();
    vivi::pump::control::set_project_mode(&fx.root, vivi::pump::control::PumpMode::Running)
        .unwrap();
    let lock = hold_pump_lock(&fx.root);
    let writes = [
        ("config_migrate", json!({"write": true})),
        ("config_set", json!({"key": "pump.tick", "value": "30"})),
        ("config_unset", json!({"key": "pump.tick"})),
        ("pool_allow_set", json!({"pool": "glm", "models": ["m1/a"]})),
        ("target_add", json!({"id": "api", "path": "api"})),
        ("target_remove", json!({"id": "web"})),
    ];
    for (name, arguments) in writes {
        let message = fx.refused(&key, name, arguments).await;
        assert!(message.contains("the pump is running"), "{name}: {message}");
        assert_eq!(fx.config_text(), before, "{name} changed the config");
    }
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 6, "{lines:?}");
    assert!(lines.iter().all(|line| line["outcome"] == "refused"));
    drop(lock);

    vivi::pump::control::set_project_mode(&fx.root, vivi::pump::control::PumpMode::Paused).unwrap();
    let _lock = hold_pump_lock(&fx.root);
    let got = fx
        .call(
            &key,
            "config_set",
            json!({"key": "pump.tick", "value": "25"}),
        )
        .await;
    assert_eq!(got["value"], "25");
    assert!(fx.config_text().contains("tick = 25"));
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 7, "{lines:?}");
    assert_eq!(lines[6]["tool"], "config_set");
    assert_eq!(lines[6]["outcome"], "ok");
    let got = fx
        .call(
            &key,
            "pool_allow_set",
            json!({"pool": "glm", "models": ["m9/z"]}),
        )
        .await;
    assert_eq!(got["models"], json!(["m9/z"]));
    assert!(fx.config_text().contains("m9/z"));
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 8, "{lines:?}");
    assert_eq!(lines[7]["tool"], "pool_allow_set");
    assert_eq!(lines[7]["outcome"], "ok");
}

#[tokio::test]
async fn a_key_that_is_not_the_operators_is_refused() {
    let fx = fixture("hand");
    let hand = fx.key_by(&fx.member, Some("hand"));
    let before = fx.config_text();
    let message = fx
        .refused(
            &hand,
            "config_set",
            json!({"key": "pump.tick", "value": "9"}),
        )
        .await;
    assert!(message.contains("kind operator"), "{message}");
    assert_eq!(fx.config_text(), before);
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["tool"], "config_set");
    assert_eq!(lines[0]["outcome"], "refused");
}
