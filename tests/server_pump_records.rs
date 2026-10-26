//! The `pump_send`, `pump_list` and `pump_show` MCP tools: operator-only pump
//! records on a hosted project, through the vivi library's record verbs.

use std::collections::BTreeSet;
use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi::mailspace::{Mailspace, PumpState, RoleUpdate, SendRequest};
use vivi::server::grants::{GrantLevel, GrantRecord};
use vivi::server::{AppState, app_state, hosted, router};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-pump-records-{}-{name}",
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
    text.push_str("\n[pump]\ntick = 20\n");
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

    /// A need addressed to the pump identity (`mind`), as the pump reads them.
    fn need(&self, subject: &str) -> String {
        let ms = Mailspace::open_root(self.root.clone()).unwrap();
        ms.send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            // A format-1 need letter: front matter with done_when, then prose.
            body: "+++\ndone_when = [\"it works\"]\n+++\n\nmake it work".into(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
    }

    fn records(&self, state: Option<PumpState>) -> Vec<vivi::mailspace::PumpRecord> {
        Mailspace::open_root(self.root.clone())
            .unwrap()
            .pump_list(None, state)
            .unwrap()
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

const TOOLS: [&str; 3] = ["pump_send", "pump_list", "pump_show"];

fn arguments_for(tool: &str, need: &str) -> Value {
    match tool {
        "pump_send" => json!({"need": need}),
        "pump_list" => json!({}),
        _ => json!({"handle": need}),
    }
}

#[tokio::test]
async fn the_tools_are_listed_as_operator_only() {
    let fx = fixture("listed");
    let key = fx.key_by(&fx.member, None);
    let listed = fx.rpc(&key, "tools/list", json!({})).await;
    for name in TOOLS {
        let tool = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} is listed"));
        let description = tool["description"].as_str().unwrap();
        assert!(
            description.contains("operator only"),
            "{name}: {description}"
        );
    }
}

#[tokio::test]
async fn send_files_a_record_the_pump_reads_and_list_and_show_return_it() {
    let fx = fixture("flow");
    let key = fx.key_by(&fx.member, None);
    let need = fx.need("fix auth");

    let sent = fx
        .call(
            &key,
            "pump_send",
            json!({"need": need, "body": "dispatch now"}),
        )
        .await;
    assert_eq!(sent["status"], "open", "{sent}");
    assert_eq!(sent["kind"], "pump");
    assert!(sent["to"].as_str().unwrap().contains("mind"), "{sent}");
    let handle = sent["handle"].as_str().unwrap().to_owned();
    assert!(!sent["need"].as_str().unwrap().is_empty());

    // The record is the one the pump itself reads: an open record of the need.
    let open = fx.records(Some(PumpState::Open));
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].handle, handle);
    assert_eq!(open[0].need, sent["need"].as_str().unwrap());

    let listed = fx.call(&key, "pump_list", json!({})).await;
    assert_eq!(listed["total"], 1, "{listed}");
    assert_eq!(listed["truncated"], false);
    assert_eq!(listed["records"][0]["handle"], handle.as_str());
    let by_need = fx
        .call(&key, "pump_list", json!({"need": need, "status": "all"}))
        .await;
    assert_eq!(by_need["total"], 1, "{by_need}");
    let done = fx.call(&key, "pump_list", json!({"status": "done"})).await;
    assert_eq!(done["total"], 0, "{done}");

    let shown = fx.call(&key, "pump_show", json!({"handle": handle})).await;
    assert_eq!(shown["handle"], handle.as_str());
    assert_eq!(shown["need"], sent["need"]);
    assert_eq!(shown["status"], "open");
    assert!(
        shown["body"].as_str().unwrap().contains("dispatch now"),
        "{shown}"
    );
}

#[tokio::test]
async fn send_is_refused_while_the_need_has_an_open_record_and_for_a_non_need() {
    let fx = fixture("refusals");
    let key = fx.key_by(&fx.member, Some("operator"));
    let need = fx.need("one");
    fx.call(&key, "pump_send", json!({"need": need})).await;
    let message = fx.refused(&key, "pump_send", json!({"need": need})).await;
    assert!(
        message.contains("already has an open pump record"),
        "{message}"
    );
    let message = fx
        .refused(&key, "pump_send", json!({"need": "deadbeef"}))
        .await;
    assert!(message.contains("unknown need"), "{message}");
    assert_eq!(fx.records(None).len(), 1);
    // The body is literal text: a path form is not read from the server.
    let other = fx.need("two");
    let sent = fx
        .call(
            &key,
            "pump_send",
            json!({"need": other, "body": "@/etc/hosts"}),
        )
        .await;
    let shown = fx
        .call(&key, "pump_show", json!({"handle": sent["handle"]}))
        .await;
    assert!(
        shown["body"].as_str().unwrap().contains("@/etc/hosts"),
        "{shown}"
    );
}

#[tokio::test]
async fn list_is_bounded_and_reports_the_total() {
    let fx = fixture("bounded");
    let key = fx.key_by(&fx.member, None);
    let mut handles = Vec::new();
    for n in 0..3 {
        let need = fx.need(&format!("need {n}"));
        let sent = fx.call(&key, "pump_send", json!({"need": need})).await;
        handles.push(sent["handle"].as_str().unwrap().to_owned());
    }
    let listed = fx.call(&key, "pump_list", json!({"limit": 2})).await;
    assert_eq!(listed["total"], 3, "{listed}");
    assert_eq!(listed["truncated"], true);
    let records = listed["records"].as_array().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["handle"], handles[1].as_str());
    assert_eq!(records[1]["handle"], handles[2].as_str());
    let bad = fx
        .refused(&key, "pump_list", json!({"status": "stale"}))
        .await;
    assert!(bad.contains("open, done, or all"), "{bad}");
}

#[tokio::test]
async fn a_member_token_is_refused_and_nothing_is_filed() {
    let fx = fixture("token");
    let need = fx.need("one");
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
    for tool in TOOLS {
        let (status, value) = fx
            .post(
                &bearer,
                "tools/call",
                json!({"name": tool, "arguments": arguments_for(tool, &need)}),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{tool}: {value}");
        let message = value["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("identity tokens are not accepted on /v1/mcp"),
            "{tool}: {message}"
        );
        assert!(message.contains("server key"), "{tool}: {message}");
    }
    assert!(fx.records(None).is_empty());
}

#[tokio::test]
async fn a_key_that_is_not_the_operators_is_refused() {
    let fx = fixture("others");
    let need = fx.need("one");
    let hand = fx.key_by(&fx.member, Some("hand"));
    for tool in TOOLS {
        let message = fx.refused(&hand, tool, arguments_for(tool, &need)).await;
        assert!(message.contains("kind operator"), "{tool}: {message}");
    }
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
    for tool in TOOLS {
        let message = fx.refused(&bobs, tool, arguments_for(tool, &need)).await;
        assert!(message.contains("host's own member"), "{tool}: {message}");
    }
    assert!(fx.records(None).is_empty());
}

#[tokio::test]
async fn a_project_that_is_not_hosted_is_refused() {
    let base = scratch("owned");
    let state = app_state(base.join("data"), None, None).expect("app state");
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
    let message = fx.refused(&key, "pump_list", json!({})).await;
    assert!(message.contains("hosted project"), "{message}");
}
