//! The Mind-only exception in `config_set`: a seat of kind `mind` may reduce
//! an already-configured `pools.<pool>.caps.<model>` integer, through the
//! operational guard; everything else stays operator-only. Every refusal
//! leaves the file byte-for-byte unchanged; every acceptance stores the
//! reduced cap.

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi::mailspace::{Mailspace, RoleUpdate};
use vivi::pump::control::{PumpMode, set_project_mode};
use vivi::server::grants::{GrantLevel, GrantRecord};
use vivi::server::{AppState, app_state, hosted, router};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-mind-caps-{}-{name}",
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
    /// The server data dir, whose `audit.jsonl` records the calls.
    data: PathBuf,
    project: String,
    /// The host's own member, which the operator's key authenticates as.
    member: String,
}

/// A served host root with two pools: `glm` carries `[pools.glm.caps]` with
/// a quoted model (`"m1/a"`) and a plain one (`m2`); `bare` has no `caps`
/// table at all.
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
    ms.add_role(
        "steward",
        RoleUpdate {
            kind: Some(Some("mind".into())),
            ..RoleUpdate::default()
        },
    )
    .unwrap();
    let toml = root.join(".vivi").join("mailspace.toml");
    let mut text = std::fs::read_to_string(&toml).unwrap();
    text.push_str(
        r#"
[pump]
tick = 20

[pools.glm]
provider = "local"
slots = 1

[pools.glm.caps]
"m1/a" = 5
m2 = 3

[pools.glm.harness]
name = "stub"
run = "echo {model}"
models = ["m1/a", "m2"]

[pools.bare]
provider = "local"
slots = 1

[pools.bare.harness]
name = "stub"
run = "echo {model}"

[pump.rungs]
pools = ["glm"]
"#,
    );
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
        state,
        root,
        data,
        project: report.served[0].project_id.clone(),
        member: report.served[0].member.clone(),
    }
}

impl Fx {
    /// The canonical name of the mailspace's one role of kind `kind`: the
    /// seat a Mind test acts as, found by kind and never by name.
    fn seat_of_kind(&self, kind: &str) -> String {
        let ms = Mailspace::open_root(self.root.clone()).unwrap();
        let seats: Vec<String> = ms
            .list_role_views()
            .unwrap()
            .into_iter()
            .filter(|role| {
                role.kind
                    .as_deref()
                    .is_some_and(|held| held.eq_ignore_ascii_case(kind))
            })
            .map(|role| role.name)
            .collect();
        assert_eq!(seats.len(), 1, "expected one role of kind {kind}");
        seats[0].clone()
    }

    fn key_by(&self, member: &str, identity: Option<&str>) -> String {
        server_key(&self.state, &self.project, member, identity)
    }

    fn config_text(&self) -> String {
        std::fs::read_to_string(self.root.join(".vivi/mailspace.toml")).unwrap()
    }

    fn backup_names(&self) -> Vec<String> {
        std::fs::read_dir(self.root.join(".vivi"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".bak."))
            .collect()
    }

    fn audit_lines(&self) -> Vec<Value> {
        std::fs::read_to_string(self.data.join("audit.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    async fn rpc(&self, token: &str, mut arguments: Value) -> Value {
        arguments
            .as_object_mut()
            .unwrap()
            .entry("project")
            .or_insert_with(|| json!(self.project.clone()));
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "config_set", "arguments": arguments}
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

    async fn call(&self, token: &str, key: &str, value: &str) -> Value {
        let value = self.rpc(token, json!({"key": key, "value": value})).await;
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        assert_ne!(value["result"]["isError"], true, "{text}");
        serde_json::from_str(text).unwrap()
    }

    /// Refuse with extra arguments beyond `key` and `value`.
    async fn refused_extra(&self, token: &str, arguments: Value) -> String {
        let value = self.rpc(token, arguments).await;
        assert_eq!(value["result"]["isError"], true, "{value}");
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str::<Value>(text).unwrap()["error"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn refused(&self, token: &str, key: &str, value: &str) -> String {
        self.refused_extra(token, json!({"key": key, "value": value}))
            .await
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

fn hold_pump_lock(root: &PathBuf) -> std::fs::File {
    let home = root.join(".vivi/pump");
    std::fs::create_dir_all(&home).unwrap();
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
async fn a_mind_seat_may_reduce_an_existing_cap() {
    let fx = fixture("reduce");
    let seat = fx.seat_of_kind("MiNd");
    let key = fx.key_by(&fx.member, Some(&seat));

    let got = fx.call(&key, "pools.glm.caps.m2", "1").await;
    assert_eq!(got["key"], "pools.glm.caps.m2");
    assert_eq!(got["value"], "1");
    assert_eq!(got["kind"], "integer");
    assert_eq!(got["by"], seat, "{got}");
    let text = fx.config_text();
    assert!(text.contains("m2 = 1"), "{text}");
    assert!(text.contains("\"m1/a\" = 5"), "{text}");
    assert_eq!(fx.backup_names().len(), 1, "the write kept a backup");

    // The reduction is this key's audited write, as the seat.
    let lines: Vec<Value> = fx
        .audit_lines()
        .into_iter()
        .filter(|line| line["tool"] == "config_set")
        .collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["outcome"], "ok");
    assert_eq!(
        lines[0]["principal"],
        format!("{}.{seat}@{}", fx.member, fx.project)
    );
}

#[tokio::test]
async fn a_quoted_model_name_stays_one_segment() {
    let fx = fixture("quoted");
    let seat = fx.seat_of_kind("mind");
    let key = fx.key_by(&fx.member, Some(&seat));

    let got = fx.call(&key, r#"pools.glm.caps."m1/a""#, "2").await;
    assert_eq!(got["value"], "2", "{got}");
    let text = fx.config_text();
    assert!(text.contains("\"m1/a\" = 2"), "{text}");

    // Unquoted, the dots split the model into extra segments: not a cap,
    // and so not writable by a Mind seat.
    let before = fx.config_text();
    let message = fx.refused(&key, "pools.glm.caps.m1.a", "1").await;
    assert!(message.contains("kind operator"), "{message}");
    assert_eq!(fx.config_text(), before, "the refusal changed nothing");
}

#[tokio::test]
async fn increases_new_caps_and_bad_values_are_refused_before_a_write() {
    let fx = fixture("reject");
    let seat = fx.seat_of_kind("mind");
    let key = fx.key_by(&fx.member, Some(&seat));
    let before = fx.config_text();

    let refusals: Vec<(&str, &str, &str)> = vec![
        ("pools.glm.caps.m2", "9", "may only be reduced"),
        ("pools.glm.caps.m2", "3", "may only be reduced"),
        ("pools.glm.caps.nope", "1", "not a configured integer cap"),
        ("pools.nope.caps.m2", "1", "is not set"),
        ("pools.bare.caps.m2", "1", "is not set"),
        ("pools.glm.caps.m2", "fast", "must be an integer"),
        ("pools.glm.caps.m2", "2.5", "must be an integer"),
        ("pools.glm.caps.m2", "-1", "between 0 and"),
        ("pools.glm.caps.m2", "5000000000", "between 0 and"),
    ];
    for (cap_key, cap_value, expected) in &refusals {
        let message = fx.refused(&key, cap_key, cap_value).await;
        assert!(
            message.contains(expected),
            "{cap_key} = {cap_value}: {message}"
        );
    }
    let extra = fx
        .refused_extra(
            &key,
            json!({"key": "pools.glm.caps.m2", "value": "1", "write": true}),
        )
        .await;
    assert!(extra.contains("only `key` and `value`"), "{extra}");
    let malformed = fx.refused(&key, "pools.glm.caps.\"m1/a", "1").await;
    assert!(malformed.contains("kind operator"), "{malformed}");
    assert_eq!(fx.config_text(), before, "no refusal changed the config");

    let lines: Vec<Value> = fx
        .audit_lines()
        .into_iter()
        .filter(|line| line["tool"] == "config_set")
        .collect();
    assert_eq!(lines.len(), refusals.len() + 2, "{lines:?}");
    assert!(lines.iter().all(|line| line["outcome"] == "refused"));
    assert!(fx.backup_names().is_empty(), "a refusal kept no backup");
}

#[tokio::test]
async fn every_other_key_stays_operator_only_for_a_mind_seat() {
    let fx = fixture("other-keys");
    let seat = fx.seat_of_kind("mind");
    let key = fx.key_by(&fx.member, Some(&seat));
    let before = fx.config_text();

    for (key_name, value) in [
        ("pump.tick", "30"),
        ("pools.glm.slots", "4"),
        ("pools.glm.provider", "remote.example"),
        ("pools.glm.caps", "5"),
        ("pools.glm.harness.models", "[\"m3/c\"]"),
    ] {
        let message = fx.refused(&key, key_name, value).await;
        assert!(message.contains("kind operator"), "{key_name}: {message}");
    }
    assert_eq!(fx.config_text(), before, "no refusal changed the config");
}

#[tokio::test]
async fn an_operator_keeps_the_full_config_set() {
    let fx = fixture("operator");
    let key = fx.key_by(&fx.member, None);

    // An increase, a brand-new cap and an unrelated key all still work.
    let got = fx.call(&key, "pools.glm.caps.m2", "9").await;
    assert_eq!(got["value"], "9", "{got}");
    assert_eq!(got["kind"], "integer");
    let got = fx.call(&key, r#"pools.glm.caps."new/m.d""#, "1").await;
    assert_eq!(got["value"], "1", "{got}");
    let got = fx.call(&key, "pump.tick", "30").await;
    assert_eq!(got["kind"], "integer", "{got}");
    let text = fx.config_text();
    assert!(text.contains("m2 = 9"), "{text}");
    assert!(text.contains("\"new/m.d\" = 1"), "{text}");
    assert!(text.contains("tick = 30"), "{text}");
    let lines: Vec<Value> = fx
        .audit_lines()
        .into_iter()
        .filter(|line| line["tool"] == "config_set")
        .collect();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines.iter().all(|line| line["outcome"] == "ok"));
}

#[tokio::test]
async fn a_foreign_members_mind_seat_cannot_reduce() {
    let fx = fixture("foreign-mind");
    let seat = fx.seat_of_kind("mind");
    fx.state
        .identities
        .register(
            &fx.project,
            "bob",
            &spki_pem(11),
            "0c0c0c0c-1111-4222-8333-444444444444",
        )
        .unwrap();
    let before = fx.config_text();
    let key = fx.key_by("bob", Some(&seat));
    let message = fx.refused(&key, "pools.glm.caps.m2", "1").await;
    assert!(message.contains("host's own member"), "{message}");
    assert_eq!(fx.config_text(), before, "a foreign mind changes nothing");
}

#[tokio::test]
async fn a_hand_cannot_reduce_a_cap() {
    let fx = fixture("hand");
    let hand = fx.key_by(&fx.member, Some("hand"));
    let before = fx.config_text();
    let message = fx.refused(&hand, "pools.glm.caps.m2", "1").await;
    assert!(message.contains("kind mind or operator"), "{message}");
    assert!(message.contains("of kind hand"), "{message}");
    assert_eq!(fx.config_text(), before, "a hand changes nothing");
}

#[tokio::test]
async fn reductions_respect_the_pump_lock() {
    let fx = fixture("lock");
    let seat = fx.seat_of_kind("mind");
    let key = fx.key_by(&fx.member, Some(&seat));
    let before = fx.config_text();

    // Running: the guarded edit refuses, and the file keeps its bytes.
    set_project_mode(&fx.root, PumpMode::Running).unwrap();
    let lock = hold_pump_lock(&fx.root);
    let message = fx.refused(&key, "pools.glm.caps.m2", "1").await;
    assert!(message.contains("the pump is running"), "{message}");
    assert_eq!(fx.config_text(), before, "the refusal changed nothing");
    let lines: Vec<Value> = fx
        .audit_lines()
        .into_iter()
        .filter(|line| line["tool"] == "config_set")
        .collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["outcome"], "refused");
    drop(lock);

    // Paused: the same reduction is accepted under the held lock.
    set_project_mode(&fx.root, PumpMode::Paused).unwrap();
    let _lock = hold_pump_lock(&fx.root);
    let got = fx.call(&key, "pools.glm.caps.m2", "1").await;
    assert_eq!(got["value"], "1", "{got}");
    let text = fx.config_text();
    assert!(text.contains("m2 = 1"), "{text}");
}
