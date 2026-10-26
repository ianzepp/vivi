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
    // The task_thread row: step required, one limit/offset pair for both
    // pages, default 20 and at most 50.
    let thread = schema("task_thread");
    assert_eq!(thread["required"], json!(["project", "step"]));
    assert_eq!(thread["additionalProperties"], json!(false));
    assert_eq!(thread["properties"]["limit"]["default"], 20);
    assert_eq!(thread["properties"]["limit"]["minimum"], 1);
    assert_eq!(thread["properties"]["limit"]["maximum"], 50);
    assert_eq!(thread["properties"]["offset"]["default"], 0);
    assert_eq!(thread["properties"]["offset"]["minimum"], 0);
    for name in ["step", "limit", "offset"] {
        assert!(
            thread["properties"][name].is_object(),
            "{name} is an argument"
        );
    }
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

/// A pump attempt note stamped `t` over `prose`, as the pump posts records.
fn stamped(t: i64, prose: &str) -> String {
    format!("+++\n[pump]\nnote = \"attempt\"\nt = {t}\n+++\n\n{prose}")
}

/// A delivered attempt record naming `attempt`, the shape `parse_record`
/// reads.
fn delivered_record(attempt: &str) -> String {
    format!(
        "pump: attempt record\nattempt: {attempt}\nneed: n1\ntask: t1000\npool: studio\n\
         start: 1793040000\nend: 1793040120\nagent-seconds: 120\noutcome: delivered\n\
         hand's final message:\n  delivered\n"
    )
}

/// The newest attempt record: a failed hand whose excerpt carries the pool's
/// 429 rate-limit line.
const RECORD_429: &str = "\
pump: attempt record
attempt: t1000-a4
need: n1
task: t1000
pool: studio
slot: slot-15
model: zai/glm-5.3-flash
start: 1793049839
end: 1793049856
agent-seconds: 17
outcome: agent failed: hand failed
unit: t1000  attempt: 4  model: zai/glm-5.3-flash  pool: studio  target: vivi
branch: none (none: the hand left no changes)
task: t1000
base:
failing command:
  agent exit 1
excerpt:
  429: {\"code\":\"1302\",\"message\":\"Rate limit reached for requests\"}
hand's final message:
  pool result 7516c305216a: finished
  pool studio  slot 542e6330-studio-slot-7  model zai/glm-5.3-flash
  agent exit 1 in 17s, job 19s, started from 1793049839
";

/// Stamp the two same-second attempt records share.
const T_SAME: i64 = 1_793_049_600_000_000;
/// An earlier stamp for the oldest attempt record.
const T_OLD: i64 = 1_793_049_000_000_000;
/// The strictly greatest stamp, so the failure leads the attempts.
const T_NEW: i64 = 1_793_050_200_000_000;

/// Reply on `task`'s thread as a mind, the way the pump posts attempt notes.
fn reply(root: &Path, task: &str, body: &str) -> String {
    Mailspace::open_root(root.to_path_buf())
        .unwrap()
        .reply(task, "mind", Vec::new(), Vec::new(), None, body.into())
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

/// Close `task` as its holder: the letter moves to done, an event letter
/// lands on the thread, and the task stays readable.
fn close_task(root: &Path, task: &str) {
    Mailspace::open_root(root.to_path_buf())
        .unwrap()
        .move_item_outcome("mind", task, "done", None, "task done", None)
        .unwrap();
}

#[tokio::test]
async fn task_thread_resolves_only_real_tasks() {
    let fx = fixture("task");
    let key = fx.key_by(&fx.member, None);
    let task = unit(&fx.root, "diagnose me");
    let prefix = format!("msg_{}", &task[..4]);

    let got = fx.call(&key, "task_thread", json!({"step": prefix})).await;
    assert_eq!(got["task"], json!(task), "the prefix resolves canonically");
    assert_eq!(got["total"], 1, "the anchor alone");
    assert_eq!(got["attempts_total"], 0);

    let parent = need(&fx.root, "not a task");
    let message = fx
        .refused(&key, "task_thread", json!({"step": parent}))
        .await;
    assert!(message.contains("not a task"), "{message}");
    let memo = send(&fx.root, "memo", "note to self", "remember this");
    let message = fx.refused(&key, "task_thread", json!({"step": memo})).await;
    assert!(message.contains("not a task"), "{message}");
    let message = fx
        .refused(&key, "task_thread", json!({"step": "zzzzzzzz"}))
        .await;
    assert!(message.contains("not found"), "{message}");

    close_task(&fx.root, &task);
    let got = fx.call(&key, "task_thread", json!({"step": task})).await;
    assert_eq!(got["task"], json!(task), "a closed task stays readable");
    assert!(audit_lines(&fx.data).is_empty(), "reads are not audited");
}

#[tokio::test]
async fn task_thread_orders_and_pages_both_streams() {
    let fx = fixture("order");
    let key = fx.key_by(&fx.member, None);
    let task = unit(&fx.root, "paged diagnostics");
    for t in 1..=3 {
        reply(&fx.root, &task, &stamped(t, &format!("progress note {t}")));
    }

    let got = fx
        .call(&key, "task_thread", json!({"step": task, "limit": 2}))
        .await;
    assert_eq!(got["task"], json!(task));
    assert_eq!(got["total"], 4);
    assert_eq!(got["offset"], 0);
    assert_eq!(got["limit"], 2);
    assert_eq!(got["next_offset"], 2);
    assert_eq!(got["truncated"], json!(true));
    let rows = got["messages"].as_array().unwrap();
    assert_eq!(rows[0]["handle"], json!(task), "the anchor leads");
    assert_eq!(rows[0]["is_attempt"], json!(false));
    for (row, t) in rows[1..].iter().zip(1..=3) {
        assert_eq!(row["is_attempt"], json!(false));
        assert!(
            row["body"].as_str().unwrap().contains(&format!("note {t}")),
            "stamped notes run in stamp order: {row}"
        );
    }
    assert_eq!(got["attempts"], json!([]));
    assert_eq!(got["attempts_total"], 0);
    assert_eq!(got["attempts_offset"], 0);
    assert_eq!(got["attempts_limit"], 2, "both pages share the limit");
    assert_eq!(got["attempts_next_offset"], Value::Null);
    assert_eq!(got["attempts_truncated"], json!(false));

    let tail = fx
        .call(
            &key,
            "task_thread",
            json!({"step": task, "limit": 2, "offset": 2}),
        )
        .await;
    assert_eq!(tail["offset"], 2);
    assert_eq!(tail["messages"].as_array().unwrap().len(), 2);
    assert_eq!(tail["next_offset"], Value::Null, "the page ends the list");
    assert_eq!(tail["truncated"], json!(false));

    let past = fx
        .call(
            &key,
            "task_thread",
            json!({"step": task, "limit": 2, "offset": 9}),
        )
        .await;
    assert_eq!(
        past["messages"],
        json!([]),
        "past the end is the empty page"
    );
    assert_eq!(past["total"], 4);
    assert_eq!(past["next_offset"], Value::Null);

    let whole = fx.call(&key, "task_thread", json!({"step": task})).await;
    assert_eq!(whole["limit"], 20, "the default limit is the effective one");
    assert_eq!(whole["messages"].as_array().unwrap().len(), 4);
    let capped = fx
        .call(&key, "task_thread", json!({"step": task, "limit": 1000}))
        .await;
    assert_eq!(capped["limit"], 50, "the limit caps at the maximum");

    // The serialized answer leads with the attempt evidence, before the
    // verbose message bodies, so a downstream cap cannot conceal it.
    let raw = fx.rpc(&key, "task_thread", json!({"step": task})).await;
    let text = raw["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.find("\"attempts\"").unwrap() < text.find("\"messages\"").unwrap(),
        "evidence first: {text}"
    );
    assert!(audit_lines(&fx.data).is_empty(), "reads are not audited");
}

#[tokio::test]
async fn newest_failure_leads_the_attempts_after_older_replies() {
    let fx = fixture("failure");
    let key = fx.key_by(&fx.member, None);
    let task = unit(&fx.root, "attempt evidence");
    reply(
        &fx.root,
        &task,
        &stamped(T_OLD, &delivered_record("t1000-a1")),
    );
    for t in 1..=4 {
        reply(&fx.root, &task, &stamped(t, "older chatter"));
    }
    reply(
        &fx.root,
        &task,
        &stamped(T_SAME, &delivered_record("t1000-a2")),
    );
    reply(
        &fx.root,
        &task,
        &stamped(T_SAME, &delivered_record("t1000-a3")),
    );
    reply(&fx.root, &task, &stamped(T_NEW, RECORD_429));

    let got = fx
        .call(&key, "task_thread", json!({"step": task, "limit": 2}))
        .await;
    assert_eq!(got["attempts_total"], 4);
    assert_eq!(got["attempts_offset"], 0);
    assert_eq!(got["attempts_limit"], 2);
    assert_eq!(got["attempts_next_offset"], 2);
    assert_eq!(got["attempts_truncated"], json!(true));
    assert_eq!(got["total"], 9, "messages count the whole thread");
    assert_eq!(got["next_offset"], 2, "the message page continues too");

    // Four older replies and two same-second delivered records cannot hide
    // the newest failure: it leads the attempts.
    let attempts = got["attempts"].as_array().unwrap();
    let newest = &attempts[0];
    assert_eq!(newest["metadata"]["attempt"], "t1000-a4");
    assert_eq!(newest["metadata"]["outcome"], "agent failed: hand failed");
    assert_eq!(newest["metadata"]["model"], "zai/glm-5.3-flash");
    assert_eq!(newest["metadata"]["slot"], "slot-15");
    assert_eq!(newest["metadata"]["agent_seconds"], 17);
    // The source handles name a real letter: the generic show resolves it.
    let source = newest["handle"].as_str().unwrap();
    assert!(!source.is_empty());
    assert!(newest["message_id"].as_str().unwrap().starts_with("msg_"));
    let shown = fx.call(&key, "show", json!({"handle": source})).await;
    assert!(
        shown["item"]["handle"]
            .as_str()
            .unwrap()
            .eq_ignore_ascii_case(source),
        "{shown}"
    );
    let body = newest["body"].as_str().unwrap();
    assert!(
        body.starts_with("outcome: agent failed: hand failed\nfailing command: agent exit 1\n"),
        "{body}"
    );
    assert!(body.contains("excerpt tail:"), "{body}");
    assert!(body.contains("429: {\"code\":\"1302\""), "{body}");
    assert_eq!(
        newest["body_truncated"],
        json!(false),
        "short tails keep all"
    );
    let second = attempts[1]["metadata"]["attempt"].as_str().unwrap();
    assert!(
        second == "t1000-a2" || second == "t1000-a3",
        "the pair follows"
    );

    let later = fx
        .call(
            &key,
            "task_thread",
            json!({"step": task, "limit": 2, "offset": 2}),
        )
        .await;
    assert_eq!(later["attempts_offset"], 2);
    let metas: Vec<String> = later["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["metadata"]["attempt"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(metas.len(), 2, "{metas:?}");
    assert!(metas.contains(&"t1000-a1".to_owned()), "{metas:?}");
    assert!(
        metas.contains(&"t1000-a2".to_owned()) || metas.contains(&"t1000-a3".to_owned()),
        "{metas:?}"
    );
    assert_eq!(later["attempts_next_offset"], Value::Null, "terminal page");
    assert_eq!(later["attempts_truncated"], json!(false));

    let past = fx
        .call(
            &key,
            "task_thread",
            json!({"step": task, "limit": 2, "offset": 4}),
        )
        .await;
    assert_eq!(
        past["attempts"],
        json!([]),
        "past the end is the empty page"
    );
    assert_eq!(past["attempts_total"], 4);
    assert_eq!(past["attempts_next_offset"], Value::Null);

    // The same thread reads oldest-first: the failure runs last there.
    let whole = fx.call(&key, "task_thread", json!({"step": task})).await;
    let rows = whole["messages"].as_array().unwrap();
    assert_eq!(rows.len(), 9);
    assert_eq!(rows[0]["handle"], json!(task));
    assert_eq!(rows[8]["is_attempt"], json!(true));
    assert!(
        rows[8]["body"]
            .as_str()
            .unwrap()
            .contains("attempt: t1000-a4"),
        "the newest-stamped note closes the thread"
    );
    assert!(audit_lines(&fx.data).is_empty(), "reads are not audited");
}

#[tokio::test]
async fn malformed_notes_stay_ordinary_content() {
    let fx = fixture("malformed");
    let key = fx.key_by(&fx.member, None);
    let task = unit(&fx.root, "malformed evidence");
    reply(
        &fx.root,
        &task,
        &stamped(T_SAME, "pump: attempt record\nattempt: x-a1\n"),
    );
    reply(&fx.root, &task, "pool result j9: finished\n");
    reply(&fx.root, &task, &"y".repeat(4 * 1024 + 1));

    let got = fx.call(&key, "task_thread", json!({"step": task})).await;
    assert_eq!(got["attempts_total"], 0, "nothing parses as an attempt");
    assert_eq!(got["attempts"], json!([]));
    assert_eq!(got["total"], 4);
    let rows = got["messages"].as_array().unwrap();
    let with_body = |needle: &str| {
        rows.iter()
            .find(|row| row["body"].as_str().unwrap().contains(needle))
            .unwrap_or_else(|| panic!("no row carries {needle}"))
    };
    let bad_row = with_body("pump: attempt record");
    assert_eq!(
        bad_row["is_attempt"],
        json!(false),
        "a record without an outcome is content"
    );
    assert_eq!(
        bad_row["body"].as_str().unwrap(),
        stamped(T_SAME, "pump: attempt record\nattempt: x-a1\n"),
        "the note is preserved verbatim"
    );
    assert_eq!(with_body("pool result j9")["is_attempt"], json!(false));
    let huge_row = with_body("yyyy");
    assert_eq!(huge_row["body_truncated"], json!(true), "the cap cut it");
    assert_eq!(huge_row["body"].as_str().unwrap().len(), 4 * 1024);
    assert!(audit_lines(&fx.data).is_empty(), "reads are not audited");
}
