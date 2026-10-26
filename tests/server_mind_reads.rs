//! `need_units` on `/v1/mcp`: the read-only page over a hosted need's bound
//! backlog units, and the schema that advertises it.
//!
//! Authorization is `pump_control`'s: an API key minted by the host's own
//! member, acting as a role of kind `operator`. A read: no audit line, and
//! no write rate limit.

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
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-mind-reads-{}-{name}",
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
        assert_ne!(value["result"]["isError"], true, "{name}: {text}");
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
        self.post(token, body).await
    }

    async fn post(&self, token: &str, body: Value) -> Value {
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

    async fn listed(&self, token: &str) -> Vec<Value> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}});
        let listed: Value = self.post(token, body).await;
        listed["result"]["tools"].as_array().cloned().unwrap()
    }
}

fn send(root: &Path, kind: &str, subject: &str, body: &str) -> String {
    Mailspace::open_root(root.to_path_buf())
        .unwrap()
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
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

fn need(root: &Path, subject: &str) -> String {
    send(
        root,
        "need",
        subject,
        "+++\ndone_when = [\"it works\"]\n+++\n\nmake it work",
    )
}

fn unit(root: &Path, subject: &str) -> String {
    send(
        root,
        "task",
        subject,
        "+++\ndone_when = [\"landed\"]\n+++\n\nland it",
    )
}

fn bind(root: &Path, need: &str, units: &[String]) {
    Mailspace::open_root(root.to_path_buf())
        .unwrap()
        .backlog_bind_units(need, units)
        .unwrap();
}

fn complete(root: &Path, unit: &str) {
    Mailspace::open_root(root.to_path_buf())
        .unwrap()
        .backlog_complete_item(unit)
        .unwrap();
}

fn audit_lines(dir: &Path) -> Vec<Value> {
    fs::read_to_string(dir.join("audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn the_tool_is_listed_with_its_arguments() {
    let fx = fixture("list");
    let key = fx.key_by(&fx.member, None);
    let tools = fx.listed(&key).await;
    let schema = |name: &str| {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} is listed"));
        assert!(
            tool["description"]
                .as_str()
                .unwrap()
                .contains("operator only"),
            "{name}"
        );
        tool["inputSchema"].clone()
    };
    let units = schema("need_units");
    assert_eq!(units["required"], json!(["project", "need"]));
    assert_eq!(units["additionalProperties"], json!(false));
    assert_eq!(units["properties"]["limit"]["default"], 20);
    assert_eq!(units["properties"]["limit"]["minimum"], 1);
    assert_eq!(units["properties"]["limit"]["maximum"], 100);
    assert_eq!(units["properties"]["offset"]["default"], 0);
    assert_eq!(units["properties"]["offset"]["minimum"], 0);
    for name in ["need", "limit", "offset"] {
        assert!(
            units["properties"][name].is_object(),
            "{name} is an argument"
        );
    }
    // The moved pump_action row is unchanged beside it, plus the new word.
    let action = schema("pump_action");
    assert_eq!(action["required"], json!(["project", "action"]));
    assert_eq!(
        action["properties"]["action"]["enum"],
        json!([
            "withdraw", "requeue", "replan", "answer", "unlock", "priority"
        ])
    );
}

#[tokio::test]
async fn resolves_the_canonical_need_and_refuses_other_handles() {
    let fx = fixture("resolve");
    let key = fx.key_by(&fx.member, None);
    let full = need(&fx.root, "fix auth");
    let prefix = format!("msg_{}", &full[..4]);

    let got = fx.call(&key, "need_units", json!({"need": prefix})).await;
    assert_eq!(got["need"], json!(full), "the prefix resolves canonically");
    assert_eq!(got["units"], json!([]));
    assert_eq!(got["total"], 0);
    assert_eq!(got["truncated"], json!(false));
    assert_eq!(got["next_offset"], Value::Null);

    let task = unit(&fx.root, "not a need");
    let message = fx.refused(&key, "need_units", json!({"need": task})).await;
    assert!(message.contains("not a need"), "{message}");

    let memo = send(&fx.root, "memo", "note to self", "remember this");
    let message = fx.refused(&key, "need_units", json!({"need": memo})).await;
    assert!(message.contains("not a need"), "{message}");

    let message = fx
        .refused(&key, "need_units", json!({"need": "zzzzzzzz"}))
        .await;
    assert!(message.contains("not found"), "{message}");
    assert!(message.contains("zzzzzzzz"), "{message}");
    assert!(audit_lines(&fx.data).is_empty(), "reads are not audited");
}

#[tokio::test]
async fn reads_bound_units_and_their_state_changes() {
    let fx = fixture("units");
    let key = fx.key_by(&fx.member, None);
    let parent = need(&fx.root, "lower me");
    let mut units: Vec<String> = ["unit 1", "unit 2"]
        .iter()
        .map(|subject| unit(&fx.root, subject))
        .collect();
    units.sort();
    bind(&fx.root, &parent, &units);

    let got = fx.call(&key, "need_units", json!({"need": parent})).await;
    assert_eq!(got["need"], json!(parent));
    assert_eq!(got["total"], 2);
    assert_eq!(got["offset"], 0);
    assert_eq!(got["limit"], 20, "the default limit is the effective one");
    assert_eq!(got["truncated"], json!(false));
    assert_eq!(got["next_offset"], Value::Null);
    assert_eq!(
        got["units"],
        json!(
            units
                .iter()
                .map(|handle| json!({"handle": handle, "state": "open"}))
                .collect::<Vec<_>>()
        )
    );

    complete(&fx.root, &units[0]);
    let got = fx.call(&key, "need_units", json!({"need": parent})).await;
    assert_eq!(got["units"][0]["state"], "done");
    assert_eq!(got["units"][1]["state"], "open");
    assert!(audit_lines(&fx.data).is_empty(), "reads are not audited");
}

#[tokio::test]
async fn pages_the_units() {
    let fx = fixture("pages");
    let key = fx.key_by(&fx.member, None);
    let parent = need(&fx.root, "many units");
    let mut units: Vec<String> = ["u1", "u2", "u3"]
        .iter()
        .map(|subject| unit(&fx.root, subject))
        .collect();
    units.sort();
    bind(&fx.root, &parent, &units);
    let handles = |got: &Value| -> Vec<String> {
        got["units"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["handle"].as_str().unwrap().to_owned())
            .collect()
    };

    let got = fx
        .call(&key, "need_units", json!({"need": parent, "limit": 2}))
        .await;
    assert_eq!(got["limit"], 2);
    assert_eq!(handles(&got), units[..2]);
    assert_eq!(got["truncated"], json!(true));
    assert_eq!(got["next_offset"], json!(2));

    let got = fx
        .call(
            &key,
            "need_units",
            json!({"need": parent, "limit": 2, "offset": 2}),
        )
        .await;
    assert_eq!(got["offset"], 2);
    assert_eq!(handles(&got), units[2..]);
    assert_eq!(got["truncated"], json!(false));
    assert_eq!(got["next_offset"], Value::Null);

    let got = fx
        .call(
            &key,
            "need_units",
            json!({"need": parent, "limit": 2, "offset": 9}),
        )
        .await;
    assert_eq!(got["units"], json!([]), "past the end is the empty page");
    assert_eq!(got["total"], 3);
    assert_eq!(got["truncated"], json!(false));
    assert_eq!(got["next_offset"], Value::Null);

    let got = fx
        .call(&key, "need_units", json!({"need": parent, "limit": 1000}))
        .await;
    assert_eq!(got["limit"], 100, "the limit caps at the maximum");
    assert_eq!(got["units"].as_array().unwrap().len(), 3);

    let got = fx
        .call(&key, "need_units", json!({"need": parent, "offset": 0}))
        .await;
    assert_eq!(got["offset"], 0, "offset 0 is a valid start");
    assert_eq!(got["units"].as_array().unwrap().len(), 3);

    let message = fx
        .refused(&key, "need_units", json!({"need": parent, "limit": 0}))
        .await;
    assert!(message.contains("positive integer"), "{message}");
    let message = fx
        .refused(&key, "need_units", json!({"need": parent, "offset": -1}))
        .await;
    assert!(message.contains("non-negative integer"), "{message}");
}

#[tokio::test]
async fn a_key_that_is_not_the_operators_is_refused() {
    let fx = fixture("others");
    let hand = fx.key_by(&fx.member, Some("hand"));
    let message = fx.refused(&hand, "need_units", json!({"need": "n1"})).await;
    assert!(message.contains("kind operator"), "{message}");
    assert!(audit_lines(&fx.data).is_empty(), "reads are not audited");
}
