//! The hosted `send` with `dispatch true`: the operator-authorized shortcut
//! that files the need through the library and hands it to the pump with one
//! open record ([`Mailspace::pump_send`]), plus an optional priority level.
//!
//! Fixture pattern borrowed from `server_pump_records.rs`: one served host
//! root with `operator` and `hand` roles and a configured pump home.

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi::mailspace::{Mailspace, PumpState, RoleUpdate};
use vivi::pump::priority;
use vivi::server::grants::{GrantLevel, GrantRecord};
use vivi::server::{AppState, app_state, hosted, router};

/// The non-default pump home the fixture configures, relative to the project
/// root, so tests can tell the configured home from the default one.
const PUMP_HOME: &str = "pump-home";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-send-dispatch-{}-{name}",
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

/// A served host root with `operator` and `hand` roles and a `[pump]` table
/// whose home is the non-default [`PUMP_HOME`]. `pump` selects whether the
/// table exists at all: dispatch without a priority must not need one.
fn fixture(name: &str, pump: bool) -> Fx {
    let base = scratch(name);
    let root = base.join("hosts").join("app");
    let mut ms = Mailspace::init(Some(&root)).unwrap();
    ms.ensure_operator_role().unwrap();
    ms.add_identity("mind").unwrap();
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
    if pump {
        text.push_str(&format!("\n[pump]\ntick = 20\nhome = \"{PUMP_HOME}\"\n"));
    }
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

    fn home(&self) -> PathBuf {
        self.root.join(PUMP_HOME)
    }

    fn default_home_db(&self) -> PathBuf {
        self.root.join(".vivi").join("pump").join("pump.db")
    }

    fn ms(&self) -> Mailspace {
        Mailspace::open_root(self.root.clone()).unwrap()
    }

    /// Every stored view, for counting what a call actually filed.
    fn stored_roles(&self) -> VecDeque<String> {
        self.ms()
            .storage()
            .unwrap()
            .list_messages()
            .unwrap()
            .into_iter()
            .map(|view| view.local_role)
            .collect()
    }

    fn records(&self, state: Option<PumpState>) -> Vec<vivi::mailspace::PumpRecord> {
        self.ms().pump_list(None, state).unwrap()
    }

    fn priorities(&self) -> Vec<(String, i64)> {
        priority::list(&self.home()).unwrap()
    }

    /// The need-level arguments of a dispatch send: one recipient, one
    /// `done_when`, prose. `extra` carries the dispatch options under test.
    fn need_arguments(extra: Value) -> Value {
        let arguments = json!({
            "kind": "need",
            "to": ["mind"],
            "subject": "fix auth",
            "body": "please fix login",
            "done_when": "it works",
        });
        let mut merged = arguments.as_object().unwrap().clone();
        for (key, value) in extra.as_object().unwrap() {
            merged.insert(key.clone(), value.clone());
        }
        Value::Object(merged)
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

    async fn call(&self, token: &str, tool: &str, arguments: Value) -> Value {
        let value = self
            .rpc(
                token,
                "tools/call",
                json!({"name": tool, "arguments": arguments}),
            )
            .await;
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        assert_ne!(value["result"]["isError"], true, "{tool}: {text}");
        serde_json::from_str(text).unwrap()
    }

    async fn refused(&self, token: &str, tool: &str, arguments: Value) -> String {
        let value = self
            .rpc(
                token,
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

#[tokio::test]
async fn operator_dispatch_returns_need_and_one_open_record() {
    // The explicit operator identity and the host-default one (a key with no
    // named identity acts as the server default) both dispatch.
    for (name, identity) in [("explicit", Some("operator")), ("default", None)] {
        let fx = fixture(name, true);
        let key = fx.key_by(&fx.member, identity);

        let plain = fx.call(&key, "send", Fx::need_arguments(json!({}))).await;
        let sent = fx
            .call(&key, "send", Fx::need_arguments(json!({"dispatch": true})))
            .await;
        // HTTP 200 and isError false are asserted by `call`. The delivery
        // fields are exactly the ordinary send's, plus `pump`.
        let plain_keys: BTreeSet<String> = plain.as_object().unwrap().keys().cloned().collect();
        let sent_keys: BTreeSet<String> = sent.as_object().unwrap().keys().cloned().collect();
        let extra: Vec<_> = sent_keys.difference(&plain_keys).collect();
        assert_eq!(extra, ["pump"], "{sent}");
        assert_eq!(sent["kind"], "need", "{sent}");
        assert_eq!(sent["from"], "operator", "{sent}");
        assert_eq!(sent["delivered"][0]["identity"], "mind", "{sent}");
        assert_eq!(sent["remote"], plain["remote"], "{sent}");

        let need = sent["handle"].as_str().unwrap().to_owned();
        let pump = sent["pump"].as_object().expect("pump record");
        assert_eq!(pump["status"], "open", "{sent}");
        assert_eq!(pump["kind"], "pump", "{sent}");
        assert_eq!(pump["need"], need, "{sent}");
        assert_eq!(pump["handle"], sent["pump"]["handle"], "{sent}");
        // Both handles resolve in the mailspace store, and exactly one open
        // library record exists for the canonical need.
        let ms = fx.ms();
        let storage = ms.storage().unwrap();
        storage.resolve_message_token(&need).unwrap();
        storage
            .resolve_message_token(pump["handle"].as_str().unwrap())
            .unwrap();
        let open = fx.records(Some(PumpState::Open));
        assert_eq!(open.len(), 1, "{open:?}");
        assert_eq!(open[0].handle, pump["handle"].as_str().unwrap());
        assert_eq!(open[0].need, need);

        // A second dispatch files exactly one open record for its own
        // canonical need, alongside the first.
        let mut other = Fx::need_arguments(json!({"dispatch": true}));
        other["subject"] = json!("fix billing");
        let second = fx.call(&key, "send", other).await;
        let second_need = second["handle"].as_str().unwrap().to_owned();
        let open = fx.records(Some(PumpState::Open));
        assert_eq!(open.len(), 2, "{open:?}");
        let needs: Vec<&str> = open.iter().map(|r| r.need.as_str()).collect();
        assert_eq!(needs, [need.as_str(), second_need.as_str()]);
        assert_ne!(need, second_need);
    }
}

#[tokio::test]
async fn a_requested_priority_lands_at_the_configured_home_and_zero_clears() {
    let fx = fixture("priority", true);
    let key = fx.key_by(&fx.member, Some("operator"));

    let sent = fx
        .call(
            &key,
            "send",
            Fx::need_arguments(json!({"dispatch": true, "priority": 7})),
        )
        .await;
    let need = sent["handle"].as_str().unwrap().to_owned();
    assert_eq!(sent["priority"], 7, "{sent}");
    assert_eq!(sent["pump"]["need"], need.as_str(), "{sent}");
    // The row is keyed by the record's canonical need, in the configured
    // home, and the default home stayed unused.
    assert_eq!(fx.priorities(), vec![(need.clone(), 7)]);
    assert!(!fx.default_home_db().exists());

    // An explicit zero keeps the clearing semantics: the dispatch succeeds,
    // the answer carries the level, and the zero-level need acquires no row
    // while the first need's row stays.
    let mut cleared = Fx::need_arguments(json!({"dispatch": true, "priority": 0}));
    cleared["subject"] = json!("fix billing");
    let second = fx.call(&key, "send", cleared).await;
    assert_eq!(second["priority"], 0, "{second}");
    assert_eq!(second["pump"]["need"], second["handle"], "{second}");
    assert_eq!(fx.priorities(), vec![(need, 7)]);
    assert_eq!(fx.records(Some(PumpState::Open)).len(), 2);
}

#[tokio::test]
async fn dispatch_without_priority_needs_no_pump_table_and_files_no_row() {
    let fx = fixture("no-pump-table", false);
    let key = fx.key_by(&fx.member, Some("operator"));

    let sent = fx
        .call(&key, "send", Fx::need_arguments(json!({"dispatch": true})))
        .await;
    let need = sent["handle"].as_str().unwrap().to_owned();
    assert_eq!(sent["pump"]["need"], need.as_str(), "{sent}");
    assert_eq!(fx.records(Some(PumpState::Open)).len(), 1);
    // No priority was requested: no pump home was resolved, so no database
    // was created anywhere, configured or default.
    assert!(!fx.home().join("pump.db").exists());
    assert!(!fx.default_home_db().exists());
}

#[tokio::test]
async fn a_priority_without_a_pump_table_is_refused_before_anything_is_filed() {
    let fx = fixture("pump-refusal", false);
    let key = fx.key_by(&fx.member, Some("operator"));

    let message = fx
        .refused(
            &key,
            "send",
            Fx::need_arguments(json!({"dispatch": true, "priority": 3})),
        )
        .await;
    assert!(message.contains("[pump]"), "{message}");
    // The refusal came before the send: no need, no record, no database.
    assert!(fx.records(None).is_empty());
    assert!(
        fx.stored_roles()
            .iter()
            .all(|role| role != "needs" && role != "pumps")
    );
    assert!(!fx.default_home_db().exists());
}

#[tokio::test]
async fn a_non_operator_key_is_refused_and_nothing_is_filed() {
    let fx = fixture("hand", true);
    let hand = fx.key_by(&fx.member, Some("hand"));
    let message = fx
        .refused(&hand, "send", Fx::need_arguments(json!({"dispatch": true})))
        .await;
    assert!(message.contains("kind operator"), "{message}");
    assert!(fx.records(None).is_empty());
    assert!(fx.stored_roles().iter().all(|role| role != "needs"));

    // An invalid dispatch combination is refused before routing, too, and
    // files nothing.
    let operator = fx.key_by(&fx.member, Some("operator"));
    let message = fx
        .refused(
            &operator,
            "send",
            Fx::need_arguments(json!({"dispatch": true, "kind": "mail"})),
        )
        .await;
    assert!(message.contains("kind need"), "{message}");
    let message = fx
        .refused(
            &operator,
            "send",
            Fx::need_arguments(json!({"priority": 1})),
        )
        .await;
    assert!(message.contains("priority requires dispatch"), "{message}");
    assert!(fx.records(None).is_empty());
    assert!(fx.stored_roles().iter().all(|role| role != "needs"));
}
