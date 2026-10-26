#![allow(dead_code)]
//! A real-server fixture for the hosted mind-read and priority tools on
//! `/v1/mcp`, adapted from `tests/server_pump_action.rs`: one served host
//! root with `operator` and `hand` roles and a configured `[pump]` table,
//! server-key minting, and raw and parsed JSON-RPC over the real axum
//! router. The transport fake in `support/project_server.rs` is not used.
//!
//! Include it from a test target with
//! `#[path = "support/mind_server.rs"] mod mind_server;`; the
//! `#![allow(dead_code)]` above covers the subset one target does not use.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi::mailspace::{Mailspace, RoleUpdate, SendRequest};
use vivi::server::grants::{GrantLevel, GrantRecord};
use vivi::server::{AppState, app_state, hosted, router};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vivi-mind-server-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// One served hosted project: the real router and its state, the hosted
/// mailspace root, and the ids a key needs.
pub struct Fx {
    pub app: Router,
    pub state: AppState,
    pub data: PathBuf,
    /// The hosted project's mailspace root on disk.
    pub root: PathBuf,
    pub project: String,
    /// The host's own member, which minted keys authenticate as.
    pub member: String,
}

/// A served hosted project with the default state.
pub fn fixture(name: &str) -> Fx {
    fixture_with(name, |_| ())
}

/// A served hosted project whose [`AppState`] is customized before the
/// router is built — a write limit, for instance:
/// `fixture_with(name, |state| state.limiter = RateLimiter::new(Some(2)))`.
pub fn fixture_with(name: &str, customize: impl FnOnce(&mut AppState)) -> Fx {
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
    let mut text = fs::read_to_string(&toml).unwrap();
    text.push_str("\n[pump]\ntick = 20\n");
    fs::write(&toml, text).unwrap();
    let data = base.join("data");
    let mut state = app_state(data.clone(), None, None).expect("app state");
    customize(&mut state);
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
    /// A server key minted for `member`; operator level unless `identity`
    /// names another role, which mints at member level.
    pub fn key_by(&self, member: &str, identity: Option<&str>) -> String {
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

    /// One raw JSON-RPC POST to `/v1/mcp`; a `tools/call` arguments object
    /// gets the fixture's `project` unless the caller set one.
    pub async fn rpc(&self, token: &str, method: &str, mut params: Value) -> Value {
        if method == "tools/call" {
            let arguments = params
                .as_object_mut()
                .unwrap()
                .entry("arguments")
                .or_insert_with(|| json!({}));
            if let Some(object) = arguments.as_object_mut() {
                object
                    .entry("project")
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
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The parsed answer of a successful `tools/call`.
    pub async fn call(&self, token: &str, name: &str, arguments: Value) -> Value {
        let value = self
            .rpc(
                token,
                "tools/call",
                json!({"name": name, "arguments": arguments}),
            )
            .await;
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        assert_ne!(value["result"]["isError"], true, "{name}: {text}");
        serde_json::from_str(text).unwrap()
    }

    /// The error message of a refused `tools/call`.
    pub async fn refused(&self, token: &str, name: &str, arguments: Value) -> String {
        let value = self
            .rpc(
                token,
                "tools/call",
                json!({"name": name, "arguments": arguments}),
            )
            .await;
        assert_eq!(value["result"]["isError"], true, "{value}");
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str::<Value>(text).unwrap()["error"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// The `tools/list` entries `token` sees on `/v1/mcp`.
    pub async fn tools(&self, token: &str) -> Vec<Value> {
        let value = self.rpc(token, "tools/list", json!({})).await;
        value["result"]["tools"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// Seed one letter in the hosted mailspace; returns its handle.
    pub fn send(&self, from: &str, kind: &str, to: &str, subject: &str, body: &str) -> String {
        Mailspace::open_root(self.root.to_path_buf())
            .unwrap()
            .send(SendRequest {
                from: from.into(),
                to: vec![to.into()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: subject.into(),
                body: body.into(),
                role: format!("{kind}s"),
                kind: Some(kind.into()),
                reply_to: None,
            })
            .unwrap()
            .delivered
            .remove(0)
            .handle
    }
}

/// The mailspace handle of the one need seeded through `Fx::send`, when a
/// test must address the item rather than its letter.
pub fn open_root(root: &Path) -> Mailspace {
    Mailspace::open_root(root.to_path_buf()).unwrap()
}
