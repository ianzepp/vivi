//! `pump_action` and `pump_reset` on `/v1/mcp`: the library calls the CLI
//! makes, one audit line per call.
//!
//! `pump_action` posts for the next tick and is not refused while the pump
//! lock is held. `pump_reset` is `pump::reset::archive`, which refuses while
//! that lock is held, running or paused.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi::mailspace::{ItemEventState, Mailspace, RoleUpdate, SendRequest, WantState};
use vivi::pump::control::{self, PumpMode};
use vivi::pump::flow::index::{RunRow, StepState};
use vivi::pump::jobs::{JobSpec, JobStatus, enqueue};
use vivi::pump::requeue::{RequeueResult, RequeueTarget};
use vivi::pump::state::State;
use vivi::pump::store::{DB_FILENAME, PumpStore};
use vivi::pump::withdraw::WithdrawClose;
use vivi::server::grants::{GrantLevel, GrantRecord};
use vivi::server::{AppState, app_state, hosted, router};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-pump-action-{}-{name}",
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
    let toml = root.join(".vivi").join("mailspace.toml");
    let mut text = fs::read_to_string(&toml).unwrap();
    text.push_str("\n[pump]\ntick = 20\n");
    fs::write(&toml, text).unwrap();
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

fn open_record(root: &Path, need: &str) -> String {
    Mailspace::open_root(root.to_path_buf())
        .unwrap()
        .pump_send(None, need, None)
        .unwrap()
        .handle
}

fn ensure_db(root: &Path) {
    let home = control::project_home(root).unwrap();
    let mut store = PumpStore::open(&home.join(DB_FILENAME)).unwrap();
    store.initialize_control("stopped").unwrap();
    store.save(&State::default()).unwrap();
}

fn seed_flow(root: &Path, need: &str, task: &str) {
    let home = control::project_home(root).unwrap();
    let mut store = PumpStore::open(&home.join(DB_FILENAME)).unwrap();
    store.initialize_control("running").unwrap();
    let run = RunRow {
        need: need.to_owned(),
        run: 1,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    };
    store.insert_run(&run).unwrap();
    store
        .insert_run_step(&run, &run.step("plan", task, StepState::Pending))
        .unwrap();
}

fn seed_jobs(root: &Path) {
    let mut st = State::default();
    for id in ["m1", "d1"] {
        enqueue(
            &mut st,
            JobSpec::new(id, "merge", "merge", "integrate", "merge branches"),
        )
        .unwrap();
    }
    st.jobs.get_mut("m1").unwrap().held = Some("no usable bridge".into());
    st.jobs.get_mut("d1").unwrap().status = JobStatus::Dispatched;
    let home = control::project_home(root).unwrap();
    let mut store = PumpStore::open(&home.join(DB_FILENAME)).unwrap();
    store.initialize_control("running").unwrap();
    store.save(&st).unwrap();
}

fn releases(root: &Path) -> Vec<String> {
    let home = control::project_home(root).unwrap();
    PumpStore::open(&home.join(DB_FILENAME))
        .unwrap()
        .take_job_releases()
        .unwrap()
}

fn pending(root: &Path) -> Vec<Value> {
    Mailspace::open_root(root.to_path_buf())
        .unwrap()
        .pump_pending_actions(&BTreeSet::new())
        .unwrap()
        .into_iter()
        .map(|action| {
            json!({
                "record": action.record,
                "need": action.need,
                "kind": action.request.kind.as_str(),
                "unit": action.request.unit,
                "step": action.request.step,
                "word": action.request.word,
                "reason": action.request.reason,
            })
        })
        .collect()
}

fn event_state(root: &Path, handle: &str) -> Option<ItemEventState> {
    Mailspace::open_root(root.to_path_buf())
        .unwrap()
        .item_event_state(handle)
        .unwrap()
}

fn want_done_withdrawn(root: &Path, want: &str) -> bool {
    let report = Mailspace::open_root(root.to_path_buf())
        .unwrap()
        .want_state(want)
        .unwrap();
    report.state == WantState::Done
        && report.serving.len() == 1
        && report.serving[0].verdict.as_deref() == Some("withdrawn")
}

fn twin(root: &Path) -> PathBuf {
    let dst = std::env::temp_dir().join(format!(
        "vivi-server-pump-action-twin-{}-{}",
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

fn archives(root: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(root.join(".vivi"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("pump-archive-"))
        .collect();
    names.sort();
    names
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

fn serving_need(root: &Path) -> (String, String) {
    let want = send(
        root,
        "want",
        "the question",
        "+++\nquestion = \"q\"\n+++\n\np",
    );
    let need = send(
        root,
        "need",
        "fix auth",
        &format!("+++\ndone_when = [\"it works\"]\nserves = \"{want}\"\n+++\n\nmake it work"),
    );
    (want, need)
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
    let action = schema("pump_action");
    assert_eq!(action["required"], json!(["project", "action"]));
    assert_eq!(
        action["properties"]["action"]["enum"],
        json!(["withdraw", "requeue", "replan", "answer", "unlock"])
    );
    for name in [
        "need", "needs", "steps", "job", "step", "word", "entry", "reason", "note", "dry_run",
    ] {
        assert!(
            action["properties"][name].is_object(),
            "{name} is an argument"
        );
    }
    let reset = schema("pump_reset");
    assert_eq!(reset["required"], json!(["project"]));
    assert_eq!(
        reset["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        vec!["project"]
    );
}

#[tokio::test]
async fn withdraw_matches_the_library_and_closes_the_need() {
    let fx = fixture("withdraw");
    let key = fx.key_by(&fx.member, None);
    let (want, need) = serving_need(&fx.root);
    let record = open_record(&fx.root, &need);
    ensure_db(&fx.root);
    let twin_root = twin(&fx.root);

    let lib = vivi::pump::withdraw::withdraw(&twin_root, &[need.clone()], "stale").unwrap();
    let got = fx
        .call(
            &key,
            "pump_action",
            json!({"action": "withdraw", "needs": [need], "reason": "stale"}),
        )
        .await;

    assert_eq!(lib.len(), 1);
    assert_eq!(lib[0].close, WithdrawClose::Closed);
    assert_eq!(lib[0].record, record);
    assert_eq!(got["action"], "withdraw");
    assert_eq!(got["results"][0]["need"], need);
    assert_eq!(got["results"][0]["close"], "closed");
    assert_eq!(got["results"][0]["record"], record);
    assert!(!got["results"][0]["id"].as_str().unwrap().is_empty());
    assert_eq!(pending(&fx.root), pending(&twin_root));
    assert_eq!(
        event_state(&fx.root, &need).unwrap().state,
        "done",
        "withdraw closes the need"
    );
    assert!(want_done_withdrawn(&fx.root, &want));
    assert!(want_done_withdrawn(&twin_root, &want));
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["tool"], "pump_action");
    assert_eq!(lines[0]["outcome"], "ok");
    let _ = fs::remove_dir_all(&twin_root);
}

#[tokio::test]
async fn requeue_steps_and_job_match_the_library() {
    let fx = fixture("requeue");
    let key = fx.key_by(&fx.member, None);
    let need = send(
        &fx.root,
        "need",
        "fix auth",
        "+++\ndone_when = [\"it works\"]\n+++\n\nmake it work",
    );
    let record = open_record(&fx.root, &need);
    seed_flow(&fx.root, &need, "step-task-1");
    let twin_root = twin(&fx.root);

    let preview = vivi::pump::requeue::requeue(
        &twin_root,
        &RequeueTarget::Steps {
            tasks: &["step-task-1".into()],
            reason: None,
        },
        true,
    )
    .unwrap();
    let got = fx
        .call(
            &key,
            "pump_action",
            json!({"action": "requeue", "steps": "step-task-1", "dry_run": true}),
        )
        .await;
    assert_eq!(
        preview,
        vec![RequeueResult::StepPreview {
            task: "step-task-1".into(),
            need: need.clone(),
        }]
    );
    assert_eq!(got["dry_run"], true);
    assert_eq!(got["results"][0]["kind"], "step_preview");
    assert_eq!(got["results"][0]["task"], "step-task-1");
    assert_eq!(got["results"][0]["need"], need);
    assert!(pending(&fx.root).is_empty());
    assert!(pending(&twin_root).is_empty());

    let posted = vivi::pump::requeue::requeue(
        &twin_root,
        &RequeueTarget::Steps {
            tasks: &["step-task-1".into()],
            reason: None,
        },
        false,
    )
    .unwrap();
    let got = fx
        .call(
            &key,
            "pump_action",
            json!({"action": "requeue", "steps": ["step-task-1"]}),
        )
        .await;
    assert!(matches!(
        &posted[0],
        RequeueResult::Step {
            record: posted_record,
            ..
        } if posted_record == &record
    ));
    assert_eq!(got["dry_run"], false);
    assert_eq!(got["results"][0]["kind"], "step");
    assert_eq!(got["results"][0]["record"], record);
    assert_eq!(got["results"][0]["need"], need);
    assert_eq!(pending(&fx.root), pending(&twin_root));
    assert_eq!(pending(&fx.root)[0]["kind"], "requeue");
    assert_eq!(pending(&fx.root)[0]["unit"], "step-task-1");
    assert_eq!(pending(&fx.root)[0]["reason"], "operator requeue");
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.iter().all(|line| line["tool"] == "pump_action"));
    assert!(lines.iter().all(|line| line["outcome"] == "ok"));
    let _ = fs::remove_dir_all(&twin_root);

    seed_jobs(&fx.root);
    let twin_root = twin(&fx.root);
    let preview =
        vivi::pump::requeue::requeue(&twin_root, &RequeueTarget::Job("m1"), true).unwrap();
    let got = fx
        .call(
            &key,
            "pump_action",
            json!({"action": "requeue", "job": "m1", "dry_run": true}),
        )
        .await;
    assert_eq!(preview, vec![RequeueResult::JobPreview { id: "m1".into() }]);
    assert_eq!(
        got["results"][0],
        json!({"kind": "job_preview", "id": "m1"})
    );
    assert!(releases(&fx.root).is_empty());
    assert!(releases(&twin_root).is_empty());

    let posted =
        vivi::pump::requeue::requeue(&twin_root, &RequeueTarget::Job("m1"), false).unwrap();
    let got = fx
        .call(
            &key,
            "pump_action",
            json!({"action": "requeue", "job": "m1"}),
        )
        .await;
    assert_eq!(posted, vec![RequeueResult::Job { id: "m1".into() }]);
    assert_eq!(got["results"][0], json!({"kind": "job", "id": "m1"}));
    assert_eq!(releases(&fx.root), ["m1"]);
    assert_eq!(releases(&twin_root), ["m1"]);
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert_eq!(lines[2]["outcome"], "ok");
    assert_eq!(lines[3]["outcome"], "ok");
    let _ = fs::remove_dir_all(&twin_root);
}

#[tokio::test]
async fn replan_answer_and_unlock_match_the_library() {
    let fx = fixture("posts");
    let key = fx.key_by(&fx.member, None);
    let need = send(
        &fx.root,
        "need",
        "fix auth",
        "+++\ndone_when = [\"it works\"]\n+++\n\nmake it work",
    );
    let record = open_record(&fx.root, &need);
    ensure_db(&fx.root);
    let step = send(
        &fx.root,
        "task",
        "plan it",
        &format!("+++\n[step]\nneed = \"{need}\"\nrun = 1\nnode = \"plan\"\n+++\n\ndo the step"),
    );
    let twin_root = twin(&fx.root);

    let lib_replan = vivi::pump::replan::replan(&twin_root, &need).unwrap();
    let lib_answer =
        vivi::pump::flow::answer::answer(&twin_root, &step, "merged", Some("see you")).unwrap();
    let lib_unlock =
        vivi::pump::unlock::unlock(&twin_root, &need, Some("plan"), Some("go")).unwrap();

    let got_replan = fx
        .call(
            &key,
            "pump_action",
            json!({"action": "replan", "need": need}),
        )
        .await;
    let got_answer = fx
        .call(
            &key,
            "pump_action",
            json!({"action": "answer", "step": step, "word": "merged", "note": "see you"}),
        )
        .await;
    let got_unlock = fx
        .call(
            &key,
            "pump_action",
            json!({"action": "unlock", "need": need, "entry": "plan", "note": "go"}),
        )
        .await;

    assert_eq!(lib_replan.record, record);
    assert_eq!(got_replan["need"], need);
    assert_eq!(got_replan["record"], record);
    assert_eq!(lib_answer.need, need);
    assert_eq!(lib_answer.word, "merged");
    assert_eq!(got_answer["step"], step);
    assert_eq!(got_answer["word"], "merged");
    assert_eq!(got_answer["need"], need);
    assert_eq!(got_answer["record"], record);
    assert_eq!(lib_unlock.entry.as_deref(), Some("plan"));
    assert_eq!(got_unlock["entry"], "plan");
    assert_eq!(got_unlock["record"], record);
    assert_eq!(pending(&fx.root), pending(&twin_root));
    let kinds: Vec<_> = pending(&fx.root)
        .iter()
        .map(|row| row["kind"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(kinds, ["replan", "answer", "unlock"]);
    assert_eq!(pending(&fx.root)[1]["reason"], "see you");
    assert_eq!(pending(&fx.root)[1]["word"], "merged");
    assert_eq!(pending(&fx.root)[2]["word"], "plan");
    assert_eq!(pending(&fx.root)[2]["reason"], "go");
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines.iter().all(|line| line["outcome"] == "ok"));
    let _ = fs::remove_dir_all(&twin_root);
}

#[tokio::test]
async fn library_refusals_are_the_tool_error_and_one_audit_line() {
    let fx = fixture("refusals");
    let key = fx.key_by(&fx.member, None);
    let mut expect = 0u64;
    let mut refuse = async |arguments: Value, needle: &str| {
        let message = fx.refused(&key, "pump_action", arguments).await;
        assert!(message.contains(needle), "{message}");
        expect += 1;
        let lines = audit_lines(&fx.data);
        assert_eq!(lines.len() as u64, expect, "{lines:?}");
        assert_eq!(lines.last().unwrap()["tool"], "pump_action");
        assert_eq!(lines.last().unwrap()["outcome"], "refused");
    };

    refuse(json!({"action": "spin"}), "must be withdraw").await;
    refuse(json!({}), "action is required").await;
    refuse(
        json!({"action": "requeue", "steps": ["a"], "job": "m1"}),
        "not both",
    )
    .await;
    refuse(json!({"action": "requeue"}), "at least one step").await;
    refuse(
        json!({"action": "requeue", "steps": ["a"], "dry_run": "yes"}),
        "must be true or false",
    )
    .await;
    refuse(
        json!({"action": "answer", "step": "t", "word": "  "}),
        "names the word to answer",
    )
    .await;
    refuse(json!({"action": "answer", "step": "t"}), "word is required").await;
    refuse(
        json!({"action": "withdraw", "need": "n1", "reason": "why"}),
        "at least one need",
    )
    .await;

    let need = send(
        &fx.root,
        "need",
        "fix auth",
        "+++\ndone_when = [\"it works\"]\n+++\n\nmake it work",
    );
    refuse(json!({"action": "replan", "need": need}), "no pump.db").await;
    ensure_db(&fx.root);
    refuse(
        json!({"action": "withdraw", "needs": [need], "reason": "why"}),
        "no open pump record",
    )
    .await;
    refuse(
        json!({"action": "requeue", "steps": ["neither"]}),
        "not a step of an active flow run",
    )
    .await;
    refuse(
        json!({"action": "unlock", "need": need}),
        "no open pump record",
    )
    .await;

    let plain = send(
        &fx.root,
        "task",
        "plain",
        "+++\ndone_when = [\"done\"]\n+++\n\nplain task",
    );
    refuse(
        json!({"action": "answer", "step": plain, "word": "merged"}),
        "is not a pump step task",
    )
    .await;

    let record = open_record(&fx.root, &need);
    refuse(
        json!({"action": "withdraw", "needs": [need], "reason": "  "}),
        "non-empty --reason",
    )
    .await;
    refuse(
        json!({"action": "withdraw", "needs": [need]}),
        "non-empty --reason",
    )
    .await;
    assert!(event_state(&fx.root, &need).is_none());
    assert_eq!(
        open_record_handle(&fx.root, &need).as_deref(),
        Some(record.as_str())
    );

    seed_jobs(&fx.root);
    refuse(
        json!({"action": "requeue", "job": "nope"}),
        "unknown job: nope",
    )
    .await;
    refuse(json!({"action": "requeue", "job": "d1"}), "not held").await;
    assert!(releases(&fx.root).is_empty());
}

fn open_record_handle(root: &Path, need: &str) -> Option<String> {
    Mailspace::open_root(root.to_path_buf())
        .unwrap()
        .pump_open_record(need)
        .unwrap()
        .map(|record| record.handle)
}

#[tokio::test]
async fn withdraw_posts_while_the_pump_lock_is_held() {
    let fx = fixture("running-action");
    let key = fx.key_by(&fx.member, None);
    let (want, need) = serving_need(&fx.root);
    open_record(&fx.root, &need);
    ensure_db(&fx.root);
    control::set_project_mode(&fx.root, PumpMode::Running).unwrap();
    let lock = hold_pump_lock(&fx.root);
    let got = fx
        .call(
            &key,
            "pump_action",
            json!({"action": "withdraw", "needs": [need], "reason": "stop"}),
        )
        .await;
    assert_eq!(got["results"][0]["close"], "closed");
    assert_eq!(event_state(&fx.root, &need).unwrap().state, "done");
    assert!(want_done_withdrawn(&fx.root, &want));
    assert_eq!(pending(&fx.root)[0]["kind"], "withdraw");
    assert!(archives(&fx.root).is_empty());
    drop(lock);
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["tool"], "pump_action");
    assert_eq!(lines[0]["outcome"], "ok");
}

#[tokio::test]
async fn pump_reset_matches_the_library() {
    let fx = fixture("reset");
    let key = fx.key_by(&fx.member, None);
    let need = send(
        &fx.root,
        "need",
        "fix auth",
        "+++\ndone_when = [\"it works\"]\n+++\n\nmake it work",
    );
    open_record(&fx.root, &need);
    control::set_project_mode(&fx.root, PumpMode::Stopped).unwrap();
    let home = control::project_home(&fx.root).unwrap();
    fs::write(home.join("exceptions.txt"), "boom\n").unwrap();
    let twin_root = twin(&fx.root);

    let lib = vivi::pump::reset::archive(&twin_root).unwrap();
    let got = fx.call(&key, "pump_reset", json!({})).await;

    assert_eq!(got["moved"], json!(lib.moved));
    assert!(got["moved"].as_array().unwrap().len() >= 2, "{got}");
    assert_eq!(got["moved"].as_array().unwrap().len(), lib.moved.len());
    assert_eq!(got["mode"], lib.mode.to_string());
    assert_eq!(got["mode"], "stopped");
    assert_eq!(got["open_records"], lib.open_records);
    assert_eq!(got["open_records"], 1);
    let archive = got["archive_dir"].as_str().unwrap();
    assert!(archive.contains("pump-archive-"), "{archive}");
    assert!(Path::new(archive).is_dir());
    assert_eq!(archives(&fx.root).len(), 1);
    assert_eq!(archives(&twin_root).len(), 1);
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["tool"], "pump_reset");
    assert_eq!(lines[0]["outcome"], "ok");
    let _ = fs::remove_dir_all(&twin_root);
}

#[tokio::test]
async fn pump_reset_is_refused_while_running_and_while_paused() {
    let fx = fixture("reset-lock");
    let key = fx.key_by(&fx.member, None);
    control::set_project_mode(&fx.root, PumpMode::Running).unwrap();
    let lock = hold_pump_lock(&fx.root);
    let message = fx.refused(&key, "pump_reset", json!({})).await;
    assert!(
        message.contains("the pump is running (lock held)"),
        "{message}"
    );
    assert!(message.contains("vivi pump stop"), "{message}");
    assert!(archives(&fx.root).is_empty());
    assert!(fx.root.join(".vivi/pump").join(DB_FILENAME).is_file());
    drop(lock);

    control::set_project_mode(&fx.root, PumpMode::Paused).unwrap();
    let lock = hold_pump_lock(&fx.root);
    let message = fx.refused(&key, "pump_reset", json!({})).await;
    assert!(
        message.contains("the pump is running (lock held)"),
        "{message}"
    );
    assert!(archives(&fx.root).is_empty());
    drop(lock);

    let got = fx.call(&key, "pump_reset", json!({})).await;
    assert_eq!(got["mode"], "stopped");
    assert!(
        got["moved"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "pump.db")
    );
    assert_eq!(archives(&fx.root).len(), 1);
    let home = control::project_home(&fx.root).unwrap();
    assert_eq!(control::inspect(&home).unwrap().mode, PumpMode::Stopped);
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[0]["tool"], "pump_reset");
    assert_eq!(lines[0]["outcome"], "refused");
    assert_eq!(lines[1]["outcome"], "refused");
    assert_eq!(lines[2]["outcome"], "ok");
}

#[tokio::test]
async fn a_key_that_is_not_the_operators_is_refused() {
    let fx = fixture("others");
    let hand = fx.key_by(&fx.member, Some("hand"));
    for tool in ["pump_action", "pump_reset"] {
        let arguments = match tool {
            "pump_action" => json!({"action": "replan", "need": "n1"}),
            _ => json!({}),
        };
        let message = fx.refused(&hand, tool, arguments).await;
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
    for tool in ["pump_action", "pump_reset"] {
        let arguments = match tool {
            "pump_action" => json!({"action": "replan", "need": "n1"}),
            _ => json!({}),
        };
        let message = fx.refused(&bobs, tool, arguments).await;
        assert!(message.contains("host's own member"), "{tool}: {message}");
    }
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(lines.iter().all(|line| line["outcome"] == "refused"));
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
    let key = {
        state
            .server_keys
            .mint(
                &format!("alice.operator@{project}"),
                false,
                vec![GrantRecord {
                    project_id: project.clone(),
                    member: "alice".into(),
                    identity: Some("operator".into()),
                    level: GrantLevel::Operator,
                }],
                "test",
                "alice",
            )
            .unwrap()
            .key
    };
    let fx = Fx {
        app: router(state.clone()),
        state,
        data: base.join("data"),
        root: base,
        project,
        member: "alice".into(),
    };
    for tool in ["pump_action", "pump_reset"] {
        let arguments = match tool {
            "pump_action" => json!({"action": "unlock", "need": "n1"}),
            _ => json!({}),
        };
        let message = fx.refused(&key, tool, arguments).await;
        assert!(message.contains("hosted project"), "{tool}: {message}");
    }
    let lines = audit_lines(&fx.data);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.iter().all(|line| line["outcome"] == "refused"));
}
