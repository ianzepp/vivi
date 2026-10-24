//! MCP on a hosted project: an API key acts on the host's own mailspace
//! through the vivi library, as a mapped vivi identity, exactly as the CLI
//! would. A project the server owns, and a member-token caller, keep the
//! transport tools.

use std::collections::BTreeSet;
use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi::mailspace::board::BoardOptions;
use vivi::mailspace::{Mailspace, RoleUpdate, SendRequest, folded_marks};
use vivi_server::{AppState, app_state, hosted, router};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-mcp-hosted-{}-{name}",
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
    /// The registered member the keys authenticate as.
    member: String,
}

/// A host project root with `operator`, `mind` and `hand` roles, served.
fn fixture(name: &str) -> Fx {
    fixture_with(name, |_| {})
}

fn fixture_with(name: &str, tune: impl FnOnce(&mut AppState)) -> Fx {
    let base = scratch(name);
    let root = base.join("hosts").join("app");
    let mut ms = Mailspace::init(Some(&root)).unwrap();
    ms.ensure_operator_role().unwrap();
    for (role, kind) in [("mind", "mind"), ("hand", "hand")] {
        ms.add_role(
            role,
            RoleUpdate {
                kind: Some(Some(kind.into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    }
    let mut state = app_state(base.join("data"), None, None);
    tune(&mut state);
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
    fn ms(&self) -> Mailspace {
        Mailspace::open_root(self.root.clone()).unwrap()
    }

    fn key(&self, identity: Option<&str>) -> String {
        self.state
            .keys
            .mint(&self.project, &self.member, "test", identity)
            .unwrap()
            .key
    }

    async fn rpc(&self, token: &str, method: &str, params: Value) -> Value {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/projects/{}/mcp", self.project))
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

    async fn call(&self, token: &str, name: &str, arguments: Value) -> Value {
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

    async fn call_err(&self, token: &str, name: &str, arguments: Value) -> String {
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
}

/// Send a need as `from` straight through the library, as the pump or the
/// CLI would, and return the recipient's handle.
fn library_need(ms: &Mailspace, from: &str, to: &str, subject: &str) -> String {
    let result = ms
        .send(SendRequest {
            from: from.into(),
            to: vec![to.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            // A format-1 need letter: front matter with done_when, then prose.
            body: "+++\ndone_when = [\"it is done\"]\n+++\n\ndo it".into(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap();
    result.delivered[0].handle.clone()
}

fn handles(views: &[vivi::storage::StoredMessageView]) -> Vec<String> {
    views.iter().map(|v| v.handle.clone()).collect()
}

#[tokio::test]
async fn a_key_without_identity_sends_as_the_default_operator() {
    let fx = fixture("send");
    let key = fx.key(None);
    let sent = fx
        .call(
            &key,
            "send",
            json!({
                "kind": "need", "to": "mind", "subject": "ship it",
                "body": "+++\ndone_when = [\"shipped\"]\n+++\n\nship",
            }),
        )
        .await;
    let handle = sent["handle"].as_str().unwrap();
    assert_eq!(sent["from"], "operator");
    assert_eq!(sent["delivered"][0]["identity"], "mind");
    assert!(sent.get("note").is_none(), "{sent}");

    // The need is in mind's needs folder, as `vivi need list --for mind`
    // would list it.
    let ms = fx.ms();
    let listed = ms.list_kind(Some("mind"), "needs", "need").unwrap();
    assert_eq!(handles(&listed), vec![handle.to_owned()]);
    assert_eq!(listed[0].subject, "ship it");
    assert!(listed[0].from_addr.starts_with("operator@"), "{listed:?}");

    // And back through MCP, as mind.
    let mind = fx.key(Some("mind"));
    let items = fx.call(&mind, "list", json!({"kind": "need"})).await;
    assert_eq!(items["items"].as_array().unwrap().len(), 1, "{items}");
    assert_eq!(items["items"][0]["handle"], handle);
    assert_eq!(items["items"][0]["status"], "open");
    assert_eq!(items["items"][0]["role"], "needs");
}

#[tokio::test]
async fn a_hosted_send_stores_a_format_one_letter_with_done_when() {
    let fx = fixture("letter-bytes");
    let key = fx.key(None);
    let sent = fx
        .call(
            &key,
            "send",
            json!({"kind": "need", "to": "mind", "subject": "v1",
                   "body": "+++\ndone_when = [\"it is a letter\"]\n+++\n\nthe prose"}),
        )
        .await;
    let handle = sent["handle"].as_str().unwrap();
    // The stored raw bytes are a format-1 letter: the header says so, and
    // the body opens with front matter carrying done_when.
    let ms = fx.ms();
    let storage = ms.storage().unwrap();
    let id = storage.resolve_message_token(handle).unwrap();
    let raw = String::from_utf8_lossy(&storage.read_message(&id).unwrap()).into_owned();
    assert!(raw.contains("X-Vivi-Format: 1"), "{raw}");
    assert!(raw.contains("done_when = [\"it is a letter\"]"), "{raw}");
    assert!(raw.contains("the prose"), "{raw}");
}

#[tokio::test]
async fn a_need_body_without_done_when_is_refused() {
    let fx = fixture("note");
    let key = fx.key(None);
    let message = fx
        .call_err(
            &key,
            "send",
            json!({"kind": "need", "to": "mind", "subject": "s", "body": "just do it"}),
        )
        .await;
    assert!(message.contains("need has no done_when"), "{message}");
    // Nothing was sent: the refusal left no letter behind.
    assert!(
        fx.ms()
            .list_kind(Some("mind"), "needs", "need")
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_hosted_send_builds_front_matter_from_the_sugar_arguments() {
    let fx = fixture("sugar");
    let key = fx.key(None);
    let sent = fx
        .call(
            &key,
            "send",
            json!({"kind": "need", "to": "mind", "subject": "sugared",
                   "body": "the prose",
                   "done_when": ["it is done"], "target": "repo"}),
        )
        .await;
    let handle = sent["handle"].as_str().unwrap();
    // The stored raw bytes are a format-1 letter whose front matter the
    // sugar arguments built; vivi's send wrote the header, the gate the body.
    let ms = fx.ms();
    let storage = ms.storage().unwrap();
    let id = storage.resolve_message_token(handle).unwrap();
    let raw = String::from_utf8_lossy(&storage.read_message(&id).unwrap()).into_owned();
    assert!(raw.contains("X-Vivi-Format: 1"), "{raw}");
    assert!(raw.contains("done_when = [\"it is done\"]"), "{raw}");
    assert!(raw.contains("target = \"repo\""), "{raw}");
    assert!(raw.contains("the prose"), "{raw}");
}

#[tokio::test]
async fn a_dependency_named_at_send_rides_in_the_front_matter() {
    let fx = fixture("sugar-depends");
    let key = fx.key(None);
    let first = fx
        .call(
            &key,
            "send",
            json!({"kind": "need", "to": "mind", "subject": "first",
                   "body": "one", "done_when": "first done"}),
        )
        .await;
    let first_handle = first["handle"].as_str().unwrap().to_owned();
    let second = fx
        .call(
            &key,
            "send",
            json!({"kind": "task", "to": "mind", "subject": "second",
                   "body": "two", "depends_on": first_handle}),
        )
        .await;
    let handle = second["handle"].as_str().unwrap();
    let ms = fx.ms();
    let storage = ms.storage().unwrap();
    let id = storage.resolve_message_token(handle).unwrap();
    let raw = String::from_utf8_lossy(&storage.read_message(&id).unwrap()).into_owned();
    assert!(
        raw.contains(&format!("depends_on = [\"{first_handle}\"]")),
        "{raw}"
    );
    assert!(!raw.contains("X-Vivi-Depends-On"), "{raw}");
}

#[tokio::test]
async fn sugar_arguments_beside_a_whole_letter_body_are_refused() {
    let fx = fixture("sugar-whole");
    let key = fx.key(None);
    let message = fx
        .call_err(
            &key,
            "send",
            json!({"kind": "need", "to": "mind", "subject": "s",
                   "body": "+++\ndone_when = [\"y\"]\n+++\n\nx",
                   "target": "repo"}),
        )
        .await;
    // vivi's own refusal, naming the front-matter key.
    assert!(message.contains("whole letter"), "{message}");
    assert!(
        message.contains("put target in its front matter"),
        "{message}"
    );
}

#[tokio::test]
async fn mail_takes_no_sugar_arguments_and_prose_mail_still_sends() {
    let fx = fixture("sugar-mail");
    let key = fx.key(None);
    let message = fx
        .call_err(
            &key,
            "send",
            json!({"kind": "mail", "to": "mind", "subject": "s", "body": "prose",
                   "done_when": "never"}),
        )
        .await;
    assert!(
        message.contains("mail takes no sugar arguments"),
        "{message}"
    );
    let sent = fx
        .call(
            &key,
            "send",
            json!({"kind": "mail", "to": "mind", "subject": "plain", "body": "prose"}),
        )
        .await;
    assert_eq!(fx.ms().list("mind", "inbox").unwrap().len(), 1, "{sent}");
}

#[tokio::test]
async fn a_key_identity_acts_as_that_identity() {
    let fx = fixture("identity");
    let hand = fx.key(Some("hand"));
    let sent = fx
        .call(
            &hand,
            "send",
            json!({"kind": "mail", "to": "mind", "subject": "hello", "body": "from the hand"}),
        )
        .await;
    assert_eq!(sent["from"], "hand");
    let inbox = fx.ms().list("mind", "inbox").unwrap();
    assert_eq!(inbox.len(), 1);
    assert!(inbox[0].from_addr.starts_with("hand@"), "{inbox:?}");

    let overview = fx.call(&hand, "overview", json!({})).await;
    assert_eq!(overview["you"]["name"], "hand");
    let mind = fx.key(Some("mind"));
    let overview = fx.call(&mind, "overview", json!({})).await;
    assert_eq!(overview["you"]["name"], "mind");
    assert_eq!(overview["totals"]["mail_visible"], 1);
}

#[tokio::test]
async fn an_identity_the_mailspace_lacks_is_a_clear_error() {
    let fx = fixture("unknown");
    let ghost = fx.key(Some("ghost"));
    let message = fx
        .call_err(
            &ghost,
            "send",
            json!({"kind": "mail", "to": "mind", "subject": "s", "body": "b"}),
        )
        .await;
    assert!(message.contains("identity 'ghost'"), "{message}");
    assert!(message.contains("not a role"), "{message}");
    for tool in ["overview", "list", "board"] {
        let message = fx.call_err(&ghost, tool, json!({})).await;
        assert!(message.contains("identity 'ghost'"), "{tool}: {message}");
    }
    assert!(fx.ms().list("mind", "inbox").unwrap().is_empty());
}

#[tokio::test]
async fn the_server_default_identity_is_configurable() {
    let fx = fixture_with("default", |state| {
        state.hosted_default_identity = "hand".into();
    });
    let key = fx.key(None);
    let sent = fx
        .call(
            &key,
            "send",
            json!({"kind": "mail", "to": "mind", "subject": "s", "body": "b"}),
        )
        .await;
    assert_eq!(sent["from"], "hand");

    let fx = fixture_with("default-missing", |state| {
        state.hosted_default_identity = "nobody".into();
    });
    let key = fx.key(None);
    let message = fx.call_err(&key, "overview", json!({})).await;
    assert!(message.contains("identity 'nobody'"), "{message}");
}

#[tokio::test]
async fn a_reply_threads_with_its_parent() {
    let fx = fixture("reply");
    let operator = fx.key(None);
    let mind = fx.key(Some("mind"));
    let sent = fx
        .call(
            &operator,
            "send",
            json!({"kind": "need", "to": "mind", "subject": "question", "body": "+++\ndone_when = [\"answered\"]\n+++\n\nwhy?"}),
        )
        .await;
    let handle = sent["handle"].as_str().unwrap().to_owned();
    let replied = fx
        .call(&mind, "reply", json!({"handle": handle, "body": "because"}))
        .await;
    assert_eq!(replied["in_reply_to"], handle);
    let reply_handle = replied["handle"].as_str().unwrap();

    // The library threads the two messages.
    let thread = fx.ms().thread(&handle, false, 50, 50).unwrap();
    assert_eq!(thread.len(), 2, "{thread:?}");
    assert!(thread.iter().any(|m| m.body.contains("because")));

    // So does the tool, from either end.
    for from in [&handle, &reply_handle.to_owned()] {
        let shown = fx.call(&mind, "thread", json!({"handle": from})).await;
        let messages = shown["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2, "{shown}");
        assert!(messages.iter().any(|m| m["body"] == "because"), "{shown}");
    }
    // The reply went to the operator's inbox.
    let inbox = fx.ms().list("operator", "inbox").unwrap();
    assert_eq!(inbox.len(), 1);
}

#[tokio::test]
async fn done_and_reopen_move_the_item_as_the_cli_does() {
    let fx = fixture("lifecycle");
    let operator = fx.key(None);
    let mind = fx.key(Some("mind"));
    let sent = fx
        .call(
            &operator,
            "send",
            json!({"kind": "need", "to": "mind", "subject": "close me", "body": "+++\ndone_when = [\"y\"]\n+++\n\nx"}),
        )
        .await;
    let handle = sent["handle"].as_str().unwrap().to_owned();

    let closed = fx
        .call(&mind, "done", json!({"handle": handle, "note": "finished"}))
        .await;
    assert!(
        matches!(closed["mode"].as_str(), Some("closed" | "reported")),
        "{closed}"
    );
    let ms = fx.ms();
    assert!(
        handles(&ms.list_kind(Some("mind"), "done", "need").unwrap()).contains(&handle),
        "done folder"
    );
    assert!(
        ms.list_kind(Some("mind"), "needs", "need")
            .unwrap()
            .is_empty()
    );
    let done = fx
        .call(&mind, "list", json!({"kind": "need", "status": "done"}))
        .await;
    assert_eq!(done["items"][0]["handle"], handle, "{done}");
    assert_eq!(done["items"][0]["status"], "done");
    let open = fx.call(&mind, "list", json!({"kind": "need"})).await;
    assert!(open["items"].as_array().unwrap().is_empty(), "{open}");

    let reopened = fx
        .call(&mind, "reopen", json!({"handle": handle, "note": "again"}))
        .await;
    assert!(
        matches!(reopened["mode"].as_str(), Some("reopened" | "reported")),
        "{reopened}"
    );
    let ms = fx.ms();
    assert_eq!(
        handles(&ms.list_kind(Some("mind"), "needs", "need").unwrap()),
        vec![handle.clone()]
    );
    let shown = fx.call(&mind, "thread", json!({"handle": handle})).await;
    assert_eq!(shown["state"], "open");
}

#[tokio::test]
async fn mark_and_unmark_change_the_marks_the_cli_sees() {
    let fx = fixture("marks");
    let operator = fx.key(None);
    let mind = fx.key(Some("mind"));
    let sent = fx
        .call(
            &operator,
            "send",
            json!({"kind": "need", "to": "mind", "subject": "mark me", "body": "+++\ndone_when = [\"y\"]\n+++\n\nx"}),
        )
        .await;
    let handle = sent["handle"].as_str().unwrap().to_owned();
    let events = |fx: &Fx| {
        let ms = fx.ms();
        let storage = ms.storage().unwrap();
        let id = storage.resolve_message_token(&handle).unwrap();
        folded_marks(&storage.list_mailspace_events(&id).unwrap())
    };

    let marked = fx
        .call(
            &mind,
            "mark",
            json!({"handle": handle, "marks": ["bug", "p1"]}),
        )
        .await;
    assert_eq!(marked["action"], "marked");
    assert_eq!(marked["marks"], json!(["bug", "p1"]));
    assert_eq!(events(&fx), vec!["bug", "p1"]);
    // Again: nothing changes.
    let again = fx
        .call(&mind, "mark", json!({"handle": handle, "marks": "bug"}))
        .await;
    assert_eq!(again["changes"][0]["changed"], false);

    let filtered = fx
        .call(&mind, "list", json!({"kind": "need", "marks": "p1"}))
        .await;
    assert_eq!(filtered["items"][0]["marks"], json!(["bug", "p1"]));
    let none = fx
        .call(&mind, "list", json!({"kind": "need", "marks": ["nope"]}))
        .await;
    assert!(none["items"].as_array().unwrap().is_empty());

    let unmarked = fx
        .call(&mind, "unmark", json!({"handle": handle, "marks": "bug"}))
        .await;
    assert_eq!(unmarked["marks"], json!(["p1"]));
    assert_eq!(events(&fx), vec!["p1"]);

    // The operator does not hold mind's copy: the CLI refuses it, so do we.
    let refused = fx
        .call_err(&operator, "mark", json!({"handle": handle, "marks": "x"}))
        .await;
    assert!(!refused.is_empty());
    assert_eq!(events(&fx), vec!["p1"]);
    let bad = fx
        .call_err(
            &mind,
            "mark",
            json!({"handle": handle, "marks": "no spaces!"}),
        )
        .await;
    assert!(
        bad.contains("invalid mark") || bad.contains("mark"),
        "{bad}"
    );
}

#[tokio::test]
async fn reads_reflect_what_the_library_wrote() {
    let fx = fixture("reads");
    let ms = fx.ms();
    // As if by the pump: written straight through the library.
    let first = library_need(&ms, "operator", "mind", "first need");
    let second = library_need(&ms, "operator", "hand", "second need");
    ms.send(SendRequest {
        from: "operator".into(),
        to: vec!["mind".into()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "a task".into(),
        // A format-1 task letter; its prerequisite lives in the front matter.
        body: format!("+++\ndepends_on = [\"{first}\"]\n+++\n\nt"),
        role: "tasks".into(),
        kind: Some("task".into()),
        reply_to: None,
    })
    .unwrap();
    ms.set_marks("mind", &first, "need", true, &["urgent".to_owned()])
        .unwrap();
    ms.save_memo("mind", "remember", "the milk", &[]).unwrap();
    let key = fx.key(Some("mind"));

    // list: by kind, to, marks, status; folders and marks as the CLI shows.
    let needs = fx.call(&key, "list", json!({"kind": "need"})).await;
    let items = needs["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "mind holds one need: {needs}");
    assert_eq!(items[0]["handle"], first);
    assert_eq!(items[0]["marks"], json!(["urgent"]));
    assert_eq!(items[0]["subject"], "first need");
    let work = fx.call(&key, "list", json!({})).await;
    assert_eq!(work["items"].as_array().unwrap().len(), 2, "need + task");
    let to_hand = fx
        .call(
            &key,
            "list",
            json!({"kind": "need", "status": "all", "to": "hand"}),
        )
        .await;
    assert!(to_hand["items"].as_array().unwrap().is_empty(), "{to_hand}");
    let task = fx.call(&key, "list", json!({"kind": "task"})).await;
    assert_eq!(task["items"][0]["depends_on"], json!([first.clone()]));
    let memos = fx.call(&key, "list", json!({"kind": "memo"})).await;
    assert_eq!(memos["items"][0]["body"], "the milk");

    // show.
    let shown = fx.call(&key, "show", json!({"handle": first})).await;
    assert_eq!(shown["item"]["kind"], "need");
    assert_eq!(shown["item"]["marks"], json!(["urgent"]));
    assert!(shown["text"].as_str().unwrap().contains("do it"));
    let memo = fx
        .call(&key, "show", json!({"handle": memos["items"][0]["handle"]}))
        .await;
    assert_eq!(memo["subject"], "remember");

    // thread by handle, with state.
    let thread = fx.call(&key, "thread", json!({"handle": first})).await;
    assert_eq!(thread["state"], "open");
    // The need and the `marked` report the mark left on its thread.
    let messages = thread["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "{thread}");
    assert!(
        messages
            .iter()
            .any(|m| m["handle"] == first && m["kind"] == "need")
    );
    assert!(messages.iter().any(|m| m["body"] == "marked"));

    // overview: counts and ready handles.
    let overview = fx.call(&key, "overview", json!({})).await;
    assert_eq!(overview["totals"]["needs_open"], 1);
    assert_eq!(overview["totals"]["tasks_open"], 1);
    assert_eq!(overview["totals"]["memos"], 1);
    assert!(
        overview["ready"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| *h == first),
        "{overview}"
    );
    assert!(
        !overview["ready"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h == "task"),
    );

    // graph: the task waits on the first need.
    let graph = fx.call(&key, "graph", json!({"view": "status"})).await;
    assert_eq!(graph["blocked"].as_array().unwrap().len(), 1, "{graph}");
    assert!(graph["ready"].as_array().unwrap().len() >= 2);

    // board: the library's own, for every role, without the host path.
    let board = fx.call(&key, "board", json!({})).await;
    let direct = ms
        .board(&BoardOptions {
            wants_cap: 5,
            ..BoardOptions::default()
        })
        .unwrap();
    assert_eq!(board["totals"]["needs_open"], direct.totals.needs_open);
    assert_eq!(board["totals"]["needs_open"], 2);
    assert_eq!(board["name"], direct.name);
    assert!(board.get("root").is_none(), "{board}");
    let mine = fx
        .call(&key, "board", json!({"for": "hand", "graph": true}))
        .await;
    assert_eq!(mine["identities"].as_array().unwrap().len(), 1);
    assert_eq!(mine["identities"][0]["identity"], "hand");
    assert_eq!(mine["identities"][0]["needs"][0]["handle"], second);
    assert!(mine["graphs"].is_array());

    // And a change made after: the next read sees it.
    ms.move_item_outcome("mind", &first, "done", None, "need done", None)
        .unwrap();
    let after = fx.call(&key, "list", json!({"kind": "need"})).await;
    assert!(after["items"].as_array().unwrap().is_empty(), "{after}");
}

#[tokio::test]
async fn memo_save_lands_in_the_acting_identitys_memos() {
    let fx = fixture("memo");
    let mind = fx.key(Some("mind"));
    let saved = fx
        .call(
            &mind,
            "memo",
            json!({"action": "save", "subject": "note", "body": "keep"}),
        )
        .await;
    let memos = fx.ms().list_kind(Some("mind"), "memos", "memo").unwrap();
    assert_eq!(
        handles(&memos),
        vec![saved["handle"].as_str().unwrap().to_owned()]
    );
    let listed = fx.call(&mind, "memo", json!({})).await;
    assert_eq!(listed["items"][0]["subject"], "note");
    // Another identity does not see it.
    let hand = fx.key(Some("hand"));
    let theirs = fx.call(&hand, "memo", json!({})).await;
    assert!(theirs["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn the_configured_write_limit_reaches_mcp_writes() {
    let fx = fixture_with("rate-limit", |state| {
        state.limiter = vivi_server::safety::RateLimiter::new(Some(2));
    });
    let mind = fx.key(Some("mind"));
    for n in 0..2 {
        fx.call(
            &mind,
            "memo",
            json!({"action": "save", "subject": format!("note {n}"), "body": "b"}),
        )
        .await;
    }
    let message = fx
        .call_err(
            &mind,
            "memo",
            json!({"action": "save", "subject": "note 2", "body": "b"}),
        )
        .await;
    assert_eq!(message, "over 2 writes/min/identity");
    // Reads are not writes, and another identity has its own budget.
    fx.call(&mind, "memo", json!({})).await;
    let hand = fx.key(Some("hand"));
    fx.call(
        &hand,
        "memo",
        json!({"action": "save", "subject": "mine", "body": "b"}),
    )
    .await;
}

#[tokio::test]
async fn zero_unlimits_mcp_writes() {
    let fx = fixture_with("rate-unlimited", |state| {
        state.limiter = vivi_server::safety::RateLimiter::new(None);
    });
    let mind = fx.key(Some("mind"));
    for n in 0..40 {
        fx.call(
            &mind,
            "memo",
            json!({"action": "save", "subject": format!("note {n}"), "body": "b"}),
        )
        .await;
    }
}

#[tokio::test]
async fn a_member_token_on_a_hosted_project_keeps_the_transport_tools() {
    let fx = fixture("token");
    let key = fx.key(None);
    // Not a key: a registered member's own bearer.
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
    // `mark` and `board` are hosted-key tools; the transport path says so.
    let message = fx.call_err(&bearer, "board", json!({})).await;
    assert!(message.contains("API key"), "{message}");
    let message = fx
        .call_err(&bearer, "mark", json!({"handle": "deadbeef", "marks": "x"}))
        .await;
    assert!(message.contains("API key"), "{message}");
    // A member token reads the transport index (which the hosted indexer
    // keeps current), never the live mailspace: items carry the transport
    // shape (`appended_at`), not the live one (`role`, `last_event`).
    library_need(&fx.ms(), "operator", "mind", "read through the index");
    let listed = fx.call(&bearer, "list", json!({})).await;
    for item in listed["items"].as_array().unwrap() {
        assert!(
            item.get("role").is_none(),
            "live shape on a token read: {item}"
        );
        assert!(
            item.get("last_event").is_none(),
            "live shape on a token read: {item}"
        );
    }
    // The key sees the mailspace: mind's need is not the operator's item,
    // but the operator's sent copy is open outbound work only when remote.
    let live = fx.call(&key, "overview", json!({})).await;
    assert_eq!(live["you"]["name"], "operator");
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

#[tokio::test]
async fn the_operator_closes_a_need_it_sent_and_lists_every_role() {
    let fx = fixture("operator");
    let operator = fx.key(None);
    let sent = fx
        .call(
            &operator,
            "send",
            json!({"kind": "need", "to": "mind", "subject": "s", "body": "+++\ndone_when = [\"y\"]\n+++\n\nx"}),
        )
        .await;
    let handle = sent["handle"].as_str().unwrap().to_owned();
    // The operator role closes any item, so the close travels as an event.
    let closed = fx.call(&operator, "done", json!({"handle": handle})).await;
    assert_eq!(closed["mode"], "closed", "{closed}");
    assert!(closed["event"].is_string());
    // The library error is passed through when it is closed already.
    let again = fx
        .call_err(&operator, "done", json!({"handle": handle}))
        .await;
    assert!(again.contains("already closed"), "{again}");

    // Scoped to the operator's own folders the need is mind's, not listed;
    // `for: all` lists it as `vivi need list` without `--for` does.
    let own = fx
        .call(&operator, "list", json!({"kind": "need", "status": "all"}))
        .await;
    assert!(own["items"].as_array().unwrap().is_empty(), "{own}");
    let all = fx
        .call(
            &operator,
            "list",
            json!({"kind": "need", "status": "all", "for": "all"}),
        )
        .await;
    assert_eq!(all["items"][0]["handle"], handle, "{all}");
    assert_eq!(all["items"][0]["status"], "done");
    let mind = fx
        .call(
            &operator,
            "list",
            json!({"kind": "need", "status": "done", "for": "mind"}),
        )
        .await;
    assert_eq!(mind["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_server_owned_project_keeps_the_transport_tools_for_keys() {
    let base = scratch("owned");
    let state = app_state(base.join("data"), None, None);
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
    let key = state
        .keys
        .mint(&project, "alice", "bot", Some("operator"))
        .unwrap()
        .key;
    let fx = Fx {
        app: router(state.clone()),
        state,
        root: base,
        project,
        member: "alice".into(),
    };
    // The key's identity is not consulted: the member sends as itself.
    let sent = fx
        .call(
            &key,
            "send",
            json!({"kind": "need", "to": "alice@0a0a0a0a.local", "subject": "s", "body": "b",
                   "done_when": "y"}),
        )
        .await;
    assert!(sent["message_id"].is_string(), "{sent}");
    assert!(sent.get("delivered").is_none(), "{sent}");
    let listed = fx.call(&key, "list", json!({"kind": "need"})).await;
    assert_eq!(listed["items"].as_array().unwrap().len(), 1, "{listed}");
    let message = fx.call_err(&key, "board", json!({})).await;
    assert!(message.contains("hosted project"), "{message}");
    let tools = fx.rpc(&key, "tools/list", json!({})).await;
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for tool in ["mark", "unmark", "depend", "board", "send", "overview"] {
        assert!(names.contains(&tool), "{tool} in {names:?}");
    }
}

#[tokio::test]
async fn for_names_the_holder_a_command_runs_as() {
    let fx = fixture("for");
    let operator = fx.key(None);
    let sent = fx
        .call(
            &operator,
            "send",
            json!({"kind": "need", "to": "mind", "subject": "approve", "body": "+++\ndone_when = [\"y\"]\n+++\n\nx"}),
        )
        .await;
    let handle = sent["handle"].as_str().unwrap().to_owned();
    let folded = |fx: &Fx| {
        let ms = fx.ms();
        let storage = ms.storage().unwrap();
        let id = storage.resolve_message_token(&handle).unwrap();
        folded_marks(&storage.list_mailspace_events(&id).unwrap())
    };
    // Without `for` the operator does not hold mind's copy.
    fx.call_err(
        &operator,
        "mark",
        json!({"handle": handle, "marks": "pump"}),
    )
    .await;
    assert!(folded(&fx).is_empty());
    // With `for=mind`, as `vivi need mark --for mind`.
    let marked = fx
        .call(
            &operator,
            "mark",
            json!({"handle": handle, "marks": "pump", "for": "mind"}),
        )
        .await;
    assert_eq!(marked["marks"], json!(["pump"]));
    assert_eq!(folded(&fx), vec!["pump"]);
    fx.call(
        &operator,
        "unmark",
        json!({"handle": handle, "marks": "pump", "for": "mind"}),
    )
    .await;
    assert!(folded(&fx).is_empty());

    let ghost = fx
        .call_err(
            &operator,
            "mark",
            json!({"handle": handle, "marks": "x", "for": "ghost"}),
        )
        .await;
    assert!(ghost.contains("for: 'ghost'"), "{ghost}");

    // done / reopen with `for`.
    let closed = fx
        .call(&operator, "done", json!({"handle": handle, "for": "mind"}))
        .await;
    assert_eq!(closed["mode"], "closed", "{closed}");
    assert_eq!(
        handles(&fx.ms().list_kind(Some("mind"), "done", "need").unwrap()),
        vec![handle.clone()]
    );
    let reopened = fx
        .call(
            &operator,
            "reopen",
            json!({"handle": handle, "for": "mind"}),
        )
        .await;
    assert_eq!(reopened["mode"], "reopened", "{reopened}");
    assert_eq!(
        handles(&fx.ms().list_kind(Some("mind"), "needs", "need").unwrap()),
        vec![handle]
    );
}

#[tokio::test]
async fn a_want_has_no_reopen_and_a_read_writes_nothing_to_the_host() {
    let fx = fixture("want");
    let ms = fx.ms();
    let want = ms
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "maybe".into(),
            // A format-1 want letter: front matter with question, then prose.
            body: "+++\nquestion = \"w?\"\n+++\n\nw".into(),
            role: "wants".into(),
            kind: Some("want".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered[0]
        .handle
        .clone();
    let key = fx.key(Some("mind"));
    let message = fx.call_err(&key, "reopen", json!({"handle": want})).await;
    assert!(message.contains("no reopen"), "{message}");

    // Serving already ran the open-time repairs; a read changes no file.
    let snapshot = |root: &std::path::Path| {
        let dir = root.join(".vivi");
        ["mailspace.toml", ".gitignore"]
            .iter()
            .map(|f| std::fs::read(dir.join(f)).ok())
            .collect::<Vec<_>>()
    };
    let before = snapshot(&fx.root);
    fx.call(&key, "overview", json!({})).await;
    fx.call(&key, "list", json!({})).await;
    assert_eq!(snapshot(&fx.root), before);
}

/// A need `mind` holds whose last event carries `note`, as a pool receipt does.
fn need_with_note(ms: &Mailspace, subject: &str, note: &str) -> String {
    let handle = library_need(ms, "operator", "mind", subject);
    ms.move_item_outcome("mind", &handle, "needs", Some(note), "pool_collected", None)
        .unwrap();
    handle
}

#[tokio::test]
async fn board_is_bounded_by_default_and_full_lifts_it() {
    let fx = fixture("board-bounds");
    let ms = fx.ms();
    let note = "x".repeat(5000);
    let needs: Vec<String> = (0..12)
        .map(|n| need_with_note(&ms, &format!("need {n}"), &note))
        .collect();
    let key = fx.key(Some("mind"));

    let board = fx.call(&key, "board", json!({})).await;
    let mind = board["identities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["identity"] == "mind")
        .unwrap();
    assert_eq!(mind["needs"].as_array().unwrap().len(), 5, "{mind}");
    assert_eq!(mind["needs_hidden"], 7);
    assert_eq!(board["totals"]["needs_open"], 12, "{}", board["totals"]);
    assert_eq!(board["totals"]["needs_hidden"], 7);
    assert_eq!(board["totals"]["tasks_hidden"], 0);
    let event = &mind["needs"][0]["last_event"];
    assert_eq!(event["command"], "pool_collected", "{event}");
    assert!(event.get("note").is_none(), "{event}");
    assert_eq!(event["note_chars"], 5000);
    // The preview is a short prefix, not the receipt.
    assert!(event["note_preview"].as_str().unwrap().chars().count() <= 81);
    assert!(
        board.to_string().len() < 20_000,
        "{}",
        board.to_string().len()
    );

    // Caps are explicit arguments; hidden counts follow them.
    let capped = fx
        .call(&key, "board", json!({"for": "mind", "needs_cap": 2}))
        .await;
    assert_eq!(
        capped["identities"][0]["needs"].as_array().unwrap().len(),
        2
    );
    assert_eq!(capped["identities"][0]["needs_hidden"], 10);

    // `notes` keeps whole notes; `full` lifts every cap too.
    let noted = fx
        .call(&key, "board", json!({"for": "mind", "notes": true}))
        .await;
    assert_eq!(
        noted["identities"][0]["needs"][0]["last_event"]["note"]
            .as_str()
            .unwrap()
            .len(),
        5000
    );
    assert_eq!(noted["identities"][0]["needs"].as_array().unwrap().len(), 5);
    let full = fx
        .call(&key, "board", json!({"for": "mind", "full": true}))
        .await;
    let all: BTreeSet<&str> = full["identities"][0]["needs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["handle"].as_str().unwrap())
        .collect();
    assert_eq!(all, needs.iter().map(String::as_str).collect());
    assert_eq!(full["identities"][0]["needs_hidden"], 0);
}

#[tokio::test]
async fn board_graph_lists_a_capped_frontier_without_the_node_table() {
    let fx = fixture("board-graph");
    let ms = fx.ms();
    for n in 0..8 {
        library_need(&ms, "operator", "mind", &format!("need {n}"));
    }
    let key = fx.key(Some("mind"));
    let graph = |args: Value| {
        let key = key.clone();
        let fx = &fx;
        async move { fx.call(&key, "board", args).await["graphs"][0].clone() }
    };
    let bounded = graph(json!({"graph": true, "graph_cap": 3})).await;
    assert_eq!(bounded["ready"].as_array().unwrap().len(), 3, "{bounded}");
    assert_eq!(bounded["ready_total"], 8);
    assert_eq!(bounded["nodes_total"], 8);
    assert!(bounded.get("nodes").is_none(), "{bounded}");
    let full = graph(json!({"graph": true, "full": true})).await;
    assert_eq!(full["ready"].as_array().unwrap().len(), 8);
    assert_eq!(full["nodes"].as_array().unwrap().len(), 8);
}

#[tokio::test]
async fn a_want_on_the_board_lists_its_serving_needs_with_state() {
    let fx = fixture("board-serving");
    let operator = fx.key(None);
    let want = fx
        .call(
            &operator,
            "send",
            json!({"kind": "want", "to": "mind", "subject": "maybe",
                   "body": "+++\nquestion = \"worth it?\"\n+++\n\nw"}),
        )
        .await;
    let want_handle = want["handle"].as_str().unwrap().to_owned();
    // A format-1 need whose front matter `serves` names the want; the
    // library resolves the handle to the want.
    let need = fx
        .call(
            &operator,
            "send",
            json!({"kind": "need", "to": "mind", "subject": "serve it",
                   "body": format!("+++\ndone_when = [\"served\"]\nserves = \"{want_handle}\"\n+++\n\ns")}),
        )
        .await;
    let need_handle = need["handle"].as_str().unwrap().to_owned();

    let mind = fx.key(Some("mind"));
    let board = fx.call(&mind, "board", json!({})).await;
    let entry = board["identities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["identity"] == "mind")
        .unwrap()["wants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["handle"] == want_handle)
        .unwrap();
    assert_eq!(
        entry["serving"],
        json!([{ "handle": need_handle, "state": "open" }]),
        "{entry}"
    );
}

#[tokio::test]
async fn overview_ready_is_the_open_work_the_caller_holds_and_is_capped() {
    let fx = fixture("overview-ready");
    let ms = fx.ms();
    let to_mind: Vec<String> = (0..60)
        .map(|n| library_need(&ms, "operator", "mind", &format!("mind need {n}")))
        .collect();
    let to_hand = library_need(&ms, "operator", "hand", "hand need");
    let done = library_need(&ms, "operator", "mind", "finished need");
    ms.move_item_outcome("mind", &done, "done", None, "need done", None)
        .unwrap();

    // The operator holds nothing: the totals say so, and so does ready, though
    // the project frontier is long.
    let operator = fx.key(None);
    let overview = fx.call(&operator, "overview", json!({})).await;
    assert_eq!(overview["totals"]["needs_open"], 0, "{overview}");
    assert_eq!(overview["totals"]["ready"], 0);
    assert_eq!(overview["ready_total"], 0);
    assert!(overview["ready"].as_array().unwrap().is_empty());
    let frontier = fx.call(&operator, "graph", json!({"view": "ready"})).await;
    assert_eq!(
        frontier["ready"].as_array().unwrap().len(),
        61,
        "{frontier}"
    );

    // Mind holds 60 open needs (the closed one is gone): ready lists 50 of
    // them, counts all 60, and names nothing it does not hold.
    let mind = fx.key(Some("mind"));
    let overview = fx.call(&mind, "overview", json!({})).await;
    assert_eq!(overview["totals"]["needs_open"], 60);
    assert_eq!(overview["totals"]["ready"], 60);
    assert_eq!(overview["ready_total"], 60);
    let ready = overview["ready"].as_array().unwrap();
    assert_eq!(ready.len(), 50);
    for handle in ready {
        assert!(to_mind.iter().any(|h| h == handle), "{handle}");
    }
    assert!(!ready.iter().any(|h| *h == done || *h == to_hand));
}

#[tokio::test]
async fn depend_adds_and_removes_prerequisites_as_the_cli_would() {
    let fx = fixture("depend");
    let operator = fx.key(None);
    let mind = fx.key(Some("mind"));
    let hand = fx.key(Some("hand"));
    let send = |subject: &'static str, to: &'static str, deps: &[&str]| {
        // A format-1 need letter; its prerequisites live in the front matter
        // (quoted: a hex handle of only digits is not a bare TOML number).
        let list = deps
            .iter()
            .map(|dep| format!("{dep:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        json!({"kind": "need", "to": to, "subject": subject,
               "body": format!("+++\ndone_when = [\"y\"]\ndepends_on = [{list}]\n+++\n\nx")})
    };
    let work = fx.call(&operator, "send", send("work", "mind", &[])).await["handle"]
        .as_str()
        .unwrap()
        .to_owned();
    let other = fx.call(&operator, "send", send("other", "mind", &[])).await["handle"]
        .as_str()
        .unwrap()
        .to_owned();
    let release = fx
        .call(&operator, "send", send("release", "mind", &[&work]))
        .await["handle"]
        .as_str()
        .unwrap()
        .to_owned();
    let blocked = |graph: &Value| {
        graph["blocked"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node.as_str() == Some(release.as_str()) || node["handle"] == release)
    };
    let graph = fx.call(&mind, "graph", json!({"view": "status"})).await;
    assert!(blocked(&graph), "{graph}");

    // Remove the dependency named at send: the release is ready.
    let removed = fx
        .call(&mind, "depend", json!({"handle": release, "remove": work}))
        .await;
    assert_eq!(removed["changes"][0]["action"], "removed");
    assert_eq!(removed["changes"][0]["changed"], true);
    assert_eq!(removed["dependencies"], json!([]));
    let graph = fx.call(&mind, "graph", json!({"view": "status"})).await;
    assert!(!blocked(&graph), "{graph}");

    // The CLI's own view agrees: the library reads the same letters.
    let ms = fx.ms();
    assert!(ms.backlog_dependencies(&release).unwrap().is_empty());

    // Add two back in one call; both block.
    let added = fx
        .call(
            &mind,
            "depend",
            json!({"handle": release, "add": [work, other]}),
        )
        .await;
    assert_eq!(
        added["dependencies"].as_array().unwrap().len(),
        2,
        "{added}"
    );
    let graph = fx.call(&mind, "graph", json!({"view": "status"})).await;
    assert!(blocked(&graph), "{graph}");

    // Again: nothing changes.
    let again = fx
        .call(&mind, "depend", json!({"handle": release, "add": work}))
        .await;
    assert_eq!(again["changes"][0]["changed"], false);

    // `for` runs the command as the holder, as on the CLI.
    let for_mind = fx
        .call(
            &operator,
            "depend",
            json!({"handle": release, "remove": other, "for": "mind"}),
        )
        .await;
    assert_eq!(for_mind["dependencies"].as_array().unwrap().len(), 1);

    // An unknown handle fails before anything is written.
    let before = ms.storage().unwrap().list_messages().unwrap().len();
    let unknown = fx
        .call_err(
            &mind,
            "depend",
            json!({"handle": release, "add": [other, "deadbeef"]}),
        )
        .await;
    assert!(unknown.contains("deadbeef"), "{unknown}");
    assert_eq!(
        fx.ms().storage().unwrap().list_messages().unwrap().len(),
        before
    );

    // A hand does not hold the release, and may not change it.
    let refused = fx
        .call_err(&hand, "depend", json!({"handle": release, "add": other}))
        .await;
    assert!(!refused.is_empty());
    let empty = fx
        .call_err(&mind, "depend", json!({"handle": release}))
        .await;
    assert!(empty.contains("--add or --remove"), "{empty}");
}
