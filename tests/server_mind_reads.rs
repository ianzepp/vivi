//! `tools/list` registration of the operator-only mind reads on `/v1/mcp`:
//! the `need_show`, `need_thread`, `task_thread` and `pump_priority`
//! schemas — types, bounds and required arguments — and the required
//! `project` argument the server endpoint injects into every project tool.

#[path = "support/mind_server.rs"]
mod mind_server;

use std::collections::BTreeMap;

use serde_json::{Value, json};

use vivi::mailspace::SendRequest;
use vivi::server::safety::RateLimiter;

/// The registered tool entry named `name` on an operator key's tools/list.
async fn entry(fx: &mind_server::Fx, key: &str, name: &str) -> Value {
    let tools = fx.tools(key).await;
    tools
        .into_iter()
        .find(|tool| tool["name"] == name)
        .unwrap_or_else(|| panic!("{name} is missing from tools/list"))
}

#[tokio::test]
async fn need_show_schema_takes_one_required_handle() {
    let fx = mind_server::fixture("mind-need-show-schema");
    let key = fx.key_by(&fx.member, Some("operator"));
    let schema = entry(&fx, &key, "need_show").await["inputSchema"].clone();
    assert_eq!(schema["type"], "object", "{schema}");
    assert_eq!(schema["additionalProperties"], false, "{schema}");
    assert_eq!(
        schema["properties"]["handle"],
        json!({"type": "string", "description": "The need handle to show"}),
        "{schema}"
    );
    assert_eq!(schema["required"], json!(["project", "handle"]), "{schema}");
}

#[tokio::test]
async fn need_thread_schema_bounds_its_page() {
    let fx = mind_server::fixture("mind-need-thread-schema");
    let key = fx.key_by(&fx.member, Some("operator"));
    let schema = entry(&fx, &key, "need_thread").await["inputSchema"].clone();
    assert_eq!(schema["type"], "object", "{schema}");
    assert_eq!(schema["additionalProperties"], false, "{schema}");
    assert_eq!(schema["properties"]["handle"]["type"], "string", "{schema}");
    assert_eq!(
        schema["properties"]["limit"],
        json!({
            "type": "integer",
            "minimum": 1,
            "maximum": 100,
            "default": 20,
            "description": "Letters returned"
        }),
        "{schema}"
    );
    assert_eq!(schema["properties"]["before"]["type"], "string", "{schema}");
    assert_eq!(schema["required"], json!(["project", "handle"]), "{schema}");
}

#[tokio::test]
async fn task_thread_schema_bounds_its_page() {
    let fx = mind_server::fixture("mind-task-thread-schema");
    let key = fx.key_by(&fx.member, Some("operator"));
    let schema = entry(&fx, &key, "task_thread").await["inputSchema"].clone();
    assert_eq!(schema["type"], "object", "{schema}");
    assert_eq!(schema["additionalProperties"], false, "{schema}");
    assert_eq!(schema["properties"]["handle"]["type"], "string", "{schema}");
    assert_eq!(
        schema["properties"]["limit"],
        json!({
            "type": "integer",
            "minimum": 1,
            "maximum": 100,
            "default": 20,
            "description": "Letters returned"
        }),
        "{schema}"
    );
    assert_eq!(schema["properties"]["before"]["type"], "string", "{schema}");
    assert_eq!(schema["required"], json!(["project", "handle"]), "{schema}");
}

#[tokio::test]
async fn pump_priority_schema_keeps_action_need_level() {
    let fx = mind_server::fixture("mind-pump-priority-schema");
    let key = fx.key_by(&fx.member, Some("operator"));
    let schema = entry(&fx, &key, "pump_priority").await["inputSchema"].clone();
    assert_eq!(schema["type"], "object", "{schema}");
    assert_eq!(schema["additionalProperties"], false, "{schema}");
    assert_eq!(
        schema["properties"]["action"]["enum"],
        json!(["set", "clear", "list"]),
        "{schema}"
    );
    assert_eq!(schema["properties"]["need"]["type"], "string", "{schema}");
    assert_eq!(
        schema["properties"]["level"],
        json!({
            "type": "integer",
            "minimum": 0,
            "maximum": i64::MAX,
            "description": "The priority level; required for set, refused beside clear and list"
        }),
        "{schema}"
    );
    assert_eq!(schema["required"], json!(["project", "action"]), "{schema}");
}

/// The server endpoint injects a required `project` argument into every
/// project tool and keeps the tool's own required arguments.
#[tokio::test]
async fn project_argument_is_injected_into_the_mind_schemas() {
    let fx = mind_server::fixture("mind-project-injection");
    let key = fx.key_by(&fx.member, Some("operator"));
    for (name, own) in [
        ("need_show", "handle"),
        ("need_thread", "handle"),
        ("task_thread", "handle"),
        ("pump_priority", "action"),
    ] {
        let schema = entry(&fx, &key, name).await["inputSchema"].clone();
        assert_eq!(
            schema["properties"]["project"]["type"], "string",
            "{name}: {schema}"
        );
        let required = schema["required"]
            .as_array()
            .unwrap_or_else(|| panic!("{name}: required is not an array: {schema}"));
        assert!(required.contains(&json!("project")), "{name}: {schema}");
        assert!(required.contains(&json!(own)), "{name}: {schema}");
    }
}

/// `fixture_with` state customization selects the write limit the endpoint
/// counts priority writes against: the second write in the window is
/// refused while the read still answers.
#[tokio::test]
async fn fixture_write_limit_bounds_priority_writes() {
    let fx = mind_server::fixture_with("mind-write-limit", |state| {
        state.limiter = RateLimiter::new(Some(1));
    });
    let key = fx.key_by(&fx.member, Some("operator"));
    let answer = fx
        .call(
            &key,
            "pump_priority",
            json!({"action": "set", "need": "n1", "level": 2}),
        )
        .await;
    assert_eq!(
        answer,
        json!({"action": "set", "need": "n1", "level": 2}),
        "{answer}"
    );
    let message = fx
        .refused(
            &key,
            "pump_priority",
            json!({"action": "set", "need": "n2", "level": 3}),
        )
        .await;
    assert!(message.contains("writes/min"), "{message}");
    let listed = fx
        .call(&key, "pump_priority", json!({"action": "list"}))
        .await;
    assert_eq!(
        listed["priorities"],
        json!([{"need": "n1", "level": 2}]),
        "{listed}"
    );
}

/// `need_show` refuses an unknown handle and any anchor that is not a need:
/// a task and a plain letter are refused alike, on the wire as `isError`.
#[tokio::test]
async fn need_show_refuses_unknown_and_non_need_anchors() {
    let fx = mind_server::fixture("mind-need-show-refusals");
    let key = fx.key_by(&fx.member, Some("operator"));
    let task = fx.send("mind", "task", "hand", "unit task", "+++\n+++\n\ndo it");
    let mail = fx.send("mind", "mail", "hand", "plain letter", "hello");
    for handle in ["no-such-handle", &task, &mail] {
        let message = fx
            .refused(&key, "need_show", json!({"handle": handle}))
            .await;
        assert!(!message.is_empty(), "{handle}: refused without a message");
    }
}

/// `need_show` resolves a handle prefix to the canonical need and answers
/// the generic show body — item row, rendered text, parsed letter — with an
/// empty `units` list while nothing is bound to the need.
#[tokio::test]
async fn need_show_resolves_the_canonical_need_and_renders_content() {
    let fx = mind_server::fixture("mind-need-show-canonical");
    let key = fx.key_by(&fx.member, Some("operator"));
    let need = fx.send(
        "mind",
        "need",
        "hand",
        "wire the flux capacitor",
        "+++\ndone_when = [\"it is wired\"]\n+++\n\nplease wire it",
    );
    let answer = fx
        .call(&key, "need_show", json!({"handle": &need[..8]}))
        .await;
    assert_eq!(answer["item"]["handle"], json!(need), "{answer}");
    assert_eq!(answer["item"]["kind"], "need", "{answer}");
    assert_eq!(answer["item"]["status"], "open", "{answer}");
    assert_eq!(answer["item"]["role"], "needs", "{answer}");
    assert_eq!(
        answer["item"]["subject"], "wire the flux capacitor",
        "{answer}"
    );
    assert!(
        answer["text"]
            .as_str()
            .unwrap_or_default()
            .contains("please wire it"),
        "{answer}"
    );
    assert_eq!(
        answer["letter"]["subject"], "wire the flux capacitor",
        "{answer}"
    );
    assert_eq!(answer["units"], json!([]), "{answer}");
}

/// `need_show` lists the units bound to the need with their backlog node
/// states, keyed by handle regardless of the library's row order.
#[tokio::test]
async fn need_show_lists_bound_units_with_states() {
    let fx = mind_server::fixture("mind-need-show-units");
    let key = fx.key_by(&fx.member, Some("operator"));
    let need = fx.send(
        "mind",
        "need",
        "hand",
        "lowered outcome",
        "+++\ndone_when = [\"it is lowered\"]\n+++\n\nbind me",
    );
    let first = fx.send("mind", "task", "hand", "unit one", "+++\n+++\n\nfirst");
    let second = fx.send("mind", "task", "hand", "unit two", "+++\n+++\n\nsecond");
    mind_server::open_root(&fx.root)
        .backlog_bind_units(&need, &[first.clone(), second.clone()])
        .unwrap();
    let answer = fx.call(&key, "need_show", json!({"handle": need})).await;
    let units = answer["units"]
        .as_array()
        .unwrap_or_else(|| panic!("units missing or not a list: {answer}"));
    let states: BTreeMap<&str, &str> = units
        .iter()
        .map(|unit| {
            (
                unit["handle"].as_str().unwrap_or_default(),
                unit["state"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(states.len(), 2, "{answer}");
    assert_eq!(states.get(first.as_str()), Some(&"open"), "{answer}");
    assert_eq!(states.get(second.as_str()), Some(&"open"), "{answer}");
}

/// One plain letter seeded straight into the hosted mailspace, replying to
/// `parent` when it names one; returns the delivered handle.
fn letter(fx: &mind_server::Fx, parent: Option<&str>, subject: &str, body: &str) -> String {
    mind_server::open_root(&fx.root)
        .send(SendRequest {
            from: "hand".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: body.into(),
            role: "mails".into(),
            kind: Some("mail".into()),
            reply_to: parent.map(str::to_owned),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

/// Every letter's subject of a thread answer, oldest first.
fn subjects(answer: &Value) -> Vec<&str> {
    answer["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("messages missing or not a list: {answer}"))
        .iter()
        .map(|m| m["subject"].as_str().unwrap_or_default())
        .collect()
}

/// The same expected subjects, owned.
fn owned(slice: &[String]) -> Vec<&str> {
    slice.iter().map(String::as_str).collect()
}

/// `need_thread` and `task_thread` refuse a missing or unknown anchor and
/// any anchor that is not the tool's own kind, on the wire as `isError`.
#[tokio::test]
async fn work_threads_refuse_missing_wrong_kind_and_unknown_anchors() {
    let fx = mind_server::fixture("mind-work-thread-refusals");
    let key = fx.key_by(&fx.member, Some("operator"));
    let need = fx.send(
        "mind",
        "need",
        "hand",
        "a need",
        "+++\ndone_when = [\"it is done\"]\n+++\n\nbody",
    );
    let task = fx.send("mind", "task", "hand", "a task", "+++\n+++\n\nbody");
    let mail = fx.send("mind", "mail", "hand", "plain letter", "hello");
    for (name, arguments) in [
        ("need_thread", json!({})),
        ("need_thread", json!({"handle": "no-such-handle"})),
        ("need_thread", json!({"handle": task})),
        ("need_thread", json!({"handle": mail})),
        ("task_thread", json!({})),
        ("task_thread", json!({"handle": "no-such-handle"})),
        ("task_thread", json!({"handle": need})),
        ("task_thread", json!({"handle": mail})),
    ] {
        let message = fx.refused(&key, name, arguments).await;
        assert!(!message.is_empty(), "{name}: refused without a message");
    }
}

/// `need_thread` answers the whole thread when it fits one page: the
/// canonical anchor identity, the open state with no lifecycle event yet,
/// and every letter oldest-first through the shared serializer, bodies and
/// handles complete.
#[tokio::test]
async fn need_thread_answers_the_whole_thread_when_it_fits_one_page() {
    let fx = mind_server::fixture("mind-need-thread-whole");
    let key = fx.key_by(&fx.member, Some("operator"));
    let need = fx.send(
        "mind",
        "need",
        "hand",
        "wire the flux capacitor",
        "+++\ndone_when = [\"it is wired\"]\n+++\n\nplease wire it",
    );
    letter(&fx, Some(&need), "first reply", "one plain line");
    letter(
        &fx,
        Some(&need),
        "second reply",
        "two lines here\nand a third line",
    );
    let answer = fx
        .call(&key, "need_thread", json!({"handle": &need[..8]}))
        .await;
    assert_eq!(answer["handle"], json!(need), "{answer}");
    assert_eq!(answer["state"], "open", "{answer}");
    assert!(answer["event"].is_null(), "{answer}");
    assert_eq!(answer["total"], 3, "{answer}");
    assert_eq!(answer["truncated"], false, "{answer}");
    assert!(answer["next_before"].is_null(), "{answer}");
    assert_eq!(
        subjects(&answer),
        vec!["wire the flux capacitor", "first reply", "second reply"],
        "{answer}"
    );
    let messages = answer["messages"].as_array().unwrap();
    assert_eq!(messages[0]["handle"], json!(need), "{answer}");
    assert_eq!(messages[0]["kind"], "need", "{answer}");
    assert_eq!(messages[0]["status"], "open", "{answer}");
    assert_eq!(
        messages[0]["body"], "+++\ndone_when = [\"it is wired\"]\n+++\n\nplease wire it",
        "{answer}"
    );
    assert_eq!(messages[1]["kind"], "mail", "{answer}");
    assert_eq!(messages[1]["body"], "one plain line", "{answer}");
    assert_eq!(messages[2]["kind"], "mail", "{answer}");
}

/// `need_thread` defaults to the newest 20 letters of the complete thread
/// and pages backwards through a `before` cursor: 25 letters (the anchor
/// plus 24 replies) come back as 20 + 5, in thread order both times, with
/// no older page behind the last one.
#[tokio::test]
async fn need_thread_pages_backwards_from_the_newest_twenty_of_a_long_thread() {
    let fx = mind_server::fixture("mind-need-thread-paging");
    let key = fx.key_by(&fx.member, Some("operator"));
    let need = fx.send(
        "mind",
        "need",
        "hand",
        "need anchor",
        "+++\ndone_when = [\"it is anchored\"]\n+++\n\nanchor body",
    );
    let mut expected: Vec<String> = vec!["need anchor".into()];
    for n in 1..=24 {
        letter(
            &fx,
            Some(&need),
            &format!("reply {n:02}"),
            &format!("body {n}"),
        );
        expected.push(format!("reply {n:02}"));
    }
    let expected_open = owned(&expected[5..]);
    let answer = fx.call(&key, "need_thread", json!({"handle": need})).await;
    // The complete linked thread is fetched before paging: no 20/50 cap.
    assert_eq!(answer["total"], 25, "{answer}");
    assert_eq!(answer["truncated"], true, "{answer}");
    assert_eq!(subjects(&answer), expected_open, "{answer}");
    // The cursor back to the older letters is the page's oldest handle.
    let next_before = answer["next_before"]
        .as_str()
        .unwrap_or_else(|| panic!("next_before missing: {answer}"))
        .to_owned();
    assert_eq!(
        next_before,
        answer["messages"][0]["handle"].as_str().unwrap_or_default(),
        "{answer}"
    );
    // The continuation answers exactly the letters the cursor excludes and
    // everything older, and stops there: no next page behind the anchor.
    let older = fx
        .call(
            &key,
            "need_thread",
            json!({"handle": need, "before": next_before}),
        )
        .await;
    assert_eq!(older["total"], 25, "{older}");
    assert_eq!(older["truncated"], false, "{older}");
    assert!(older["next_before"].is_null(), "{older}");
    assert_eq!(subjects(&older), owned(&expected[..5]), "{older}");
    // A cursor on the thread's second letter excludes it and the anchor:
    // only the anchor remains, with no older page behind it.
    let second = older["messages"][1]["handle"].as_str().unwrap().to_owned();
    let last = fx
        .call(
            &key,
            "need_thread",
            json!({"handle": need, "before": second}),
        )
        .await;
    assert_eq!(subjects(&last), owned(&expected[..1]), "{last}");
    assert_eq!(last["truncated"], false, "{last}");
    assert!(last["next_before"].is_null(), "{last}");
    // A page size up to 100 is permitted and still reads the whole thread.
    let wide = fx
        .call(&key, "need_thread", json!({"handle": need, "limit": 500}))
        .await;
    assert_eq!(wide["total"], 25, "{wide}");
    assert_eq!(subjects(&wide).len(), 25, "{wide}");
    assert_eq!(wide["truncated"], false, "{wide}");
    assert!(wide["next_before"].is_null(), "{wide}");
}

/// `need_thread` honors an explicit page size at both bounds: one letter,
/// then the rest through the cursor; the whole thread at the 100 ceiling.
#[tokio::test]
async fn need_thread_honors_explicit_page_sizes_at_the_boundaries() {
    let fx = mind_server::fixture("mind-need-thread-bounds");
    let key = fx.key_by(&fx.member, Some("operator"));
    let need = fx.send(
        "mind",
        "need",
        "hand",
        "bound anchor",
        "+++\ndone_when = [\"it is bounded\"]\n+++\n\nanchor body",
    );
    letter(&fx, Some(&need), "first reply", "one");
    letter(&fx, Some(&need), "second reply", "two");
    let answer = fx
        .call(&key, "need_thread", json!({"handle": need, "limit": 1}))
        .await;
    assert_eq!(subjects(&answer), vec!["second reply"], "{answer}");
    assert_eq!(answer["total"], 3, "{answer}");
    assert_eq!(answer["truncated"], true, "{answer}");
    // The cursor back to the older letters is the page's oldest handle.
    let second = answer["messages"][0]["handle"].as_str().unwrap_or_default();
    assert_eq!(answer["next_before"], json!(second), "{answer}");
    let rest = fx
        .call(
            &key,
            "need_thread",
            json!({"handle": need, "limit": 1, "before": second}),
        )
        .await;
    assert_eq!(subjects(&rest), vec!["first reply"], "{rest}");
    let first = rest["messages"][0]["handle"].as_str().unwrap_or_default();
    assert_eq!(rest["next_before"], json!(first), "{rest}");
    assert_eq!(rest["truncated"], true, "{rest}");
    let whole = fx
        .call(&key, "need_thread", json!({"handle": need, "limit": 100}))
        .await;
    assert_eq!(
        subjects(&whole),
        vec!["bound anchor", "first reply", "second reply"],
        "{whole}"
    );
    assert_eq!(whole["truncated"], false, "{whole}");
    assert!(whole["next_before"].is_null(), "{whole}");
}

/// `need_thread` rejects a cursor that names no thread letter — unknown or
/// belonging to another thread — and arguments of the wrong type.
#[tokio::test]
async fn need_thread_rejects_outside_cursors_and_wrong_argument_types() {
    let fx = mind_server::fixture("mind-need-thread-bad-cursors");
    let key = fx.key_by(&fx.member, Some("operator"));
    let need = fx.send(
        "mind",
        "need",
        "hand",
        "cursor anchor",
        "+++\ndone_when = [\"it is cursored\"]\n+++\n\nanchor body",
    );
    letter(&fx, Some(&need), "own reply", "in thread");
    let other = fx.send(
        "mind",
        "need",
        "hand",
        "other anchor",
        "+++\ndone_when = [\"it is other\"]\n+++\n\nother body",
    );
    let outside = letter(&fx, Some(&other), "other reply", "other thread");
    for arguments in [
        json!({"handle": need, "before": "no-such-handle"}),
        json!({"handle": need, "before": outside}),
        json!({"handle": need, "before": 42}),
        json!({"handle": need, "limit": "20"}),
        json!({"handle": need, "limit": 0}),
        json!({"handle": need, "limit": -1}),
        json!({"handle": 7}),
    ] {
        let named = format!("{arguments}");
        let message = fx.refused(&key, "need_thread", arguments).await;
        assert!(!message.is_empty(), "refused without a message: {named}");
    }
}

/// `task_thread` answers the task's full thread and folds the lifecycle
/// state: `open` with no event while the task is new, `done` with the
/// newest event once it settles, whose event letter joins the thread.
#[tokio::test]
async fn task_thread_answers_the_full_thread_and_folds_the_lifecycle_state() {
    let fx = mind_server::fixture("mind-task-thread-state");
    let key = fx.key_by(&fx.member, Some("operator"));
    let task = fx.send(
        "mind",
        "task",
        "hand",
        "solder the board",
        "+++\n+++\n\nsolder it",
    );
    letter(&fx, Some(&task), "first note", "one");
    letter(&fx, Some(&task), "second note", "two");
    let answer = fx.call(&key, "task_thread", json!({"handle": task})).await;
    assert_eq!(answer["handle"], json!(task), "{answer}");
    assert_eq!(answer["state"], "open", "{answer}");
    assert!(answer["event"].is_null(), "{answer}");
    assert_eq!(answer["total"], 3, "{answer}");
    assert_eq!(answer["truncated"], false, "{answer}");
    assert_eq!(
        subjects(&answer),
        vec!["solder the board", "first note", "second note"],
        "{answer}"
    );
    let messages = answer["messages"].as_array().unwrap();
    assert_eq!(messages[0]["handle"], json!(task), "{answer}");
    assert_eq!(messages[0]["kind"], "task", "{answer}");
    assert_eq!(messages[0]["status"], "open", "{answer}");
    assert_eq!(messages[1]["body"], "one", "{answer}");
    assert_eq!(messages[2]["body"], "two", "{answer}");
    mind_server::open_root(&fx.root)
        .move_item_outcome("mind", &task, "done", None, "task done", None)
        .unwrap();
    let answer = fx.call(&key, "task_thread", json!({"handle": task})).await;
    assert_eq!(answer["state"], "done", "{answer}");
    assert_eq!(answer["event"]["state"], "done", "{answer}");
    assert!(answer["event"]["event"].is_string(), "{answer}");
    assert_eq!(answer["total"], 4, "{answer}");
    assert_eq!(answer["truncated"], false, "{answer}");
    assert_eq!(
        &subjects(&answer)[..3],
        &["solder the board", "first note", "second note"],
        "{answer}"
    );
}
