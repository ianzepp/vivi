//! Tests for the project injection: a scripted [`Upstream`], no sockets.

use std::cell::RefCell;
use std::collections::VecDeque;

use serde_json::{Value, json};

use super::*;
use crate::error::VivariumError;
use crate::mcp::serve_with_project;
use crate::sync::http::RawResponse;

/// Records every body it saw and pops scripted answers.
struct Stub {
    posts: RefCell<Vec<Vec<u8>>>,
    answers: RefCell<VecDeque<Result<RawResponse, VivariumError>>>,
}

impl Stub {
    fn new(answers: Vec<Result<RawResponse, VivariumError>>) -> Self {
        Self {
            posts: RefCell::new(Vec::new()),
            answers: RefCell::new(answers.into()),
        }
    }

    fn count(&self) -> usize {
        self.posts.borrow().len()
    }

    fn body(&self, index: usize) -> Value {
        serde_json::from_slice(&self.posts.borrow()[index]).expect("posted JSON")
    }
}

impl Upstream for Stub {
    fn post(&self, _bearer: &str, body: &[u8]) -> Result<RawResponse, VivariumError> {
        self.posts.borrow_mut().push(body.to_vec());
        self.answers
            .borrow_mut()
            .pop_front()
            .expect("an unscripted post")
    }
}

fn answer(value: &Value) -> Result<RawResponse, VivariumError> {
    Ok(RawResponse {
        status: 200,
        content_type: Some("application/json".into()),
        body: value.to_string().into_bytes(),
    })
}

/// A tools/list answer for the first bridge-private id: `show` takes a
/// project, `echo` does not.
fn tool_list() -> Result<RawResponse, VivariumError> {
    answer(&json!({
        "jsonrpc": "2.0",
        "id": format!("{LIST_ID}-0"),
        "result": { "tools": [
            { "name": "show", "inputSchema": { "properties": { "project": {}, "handle": {} } } },
            { "name": "echo", "inputSchema": { "properties": { "text": {} } } },
        ] }
    }))
}

fn call(name: &str, arguments: &Value) -> String {
    json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":name,"arguments":arguments}})
        .to_string()
}

fn apply(injector: &mut Injector, stub: &Stub, line: &str) -> Forward {
    let message: Value = serde_json::from_str(line).expect("line is JSON");
    injector.apply(stub, "key", line, &message)
}

#[test]
fn a_call_without_project_gets_the_configured_one() {
    let stub = Stub::new(vec![tool_list()]);
    let mut injector = Injector::new(Some("p1"));
    let sent = apply(&mut injector, &stub, &call("show", &json!({"handle":"t1"})));
    let body: Value = serde_json::from_str(&sent.body).unwrap();
    assert_eq!(body["params"]["arguments"]["project"], "p1");
    assert_eq!(body["params"]["arguments"]["handle"], "t1");
    assert!(sent.diag.is_none());
    assert_eq!(stub.body(0)["method"], "tools/list");
}

#[test]
fn an_explicit_project_is_kept_and_asks_nothing() {
    let stub = Stub::new(vec![]);
    let mut injector = Injector::new(Some("p1"));
    let line = call("show", &json!({"project":"other"}));
    let sent = apply(&mut injector, &stub, &line);
    assert_eq!(sent.body, line);
    assert_eq!(stub.count(), 0);
}

#[test]
fn a_tool_without_project_is_untouched() {
    let stub = Stub::new(vec![tool_list()]);
    let mut injector = Injector::new(Some("p1"));
    let line = call("echo", &json!({"text":"hi"}));
    assert_eq!(apply(&mut injector, &stub, &line).body, line);
}

#[test]
fn no_project_is_a_pure_pass_through() {
    let stub = Stub::new(vec![]);
    let mut injector = Injector::new(None);
    let line = call("show", &json!({"handle":"t1"}));
    assert_eq!(apply(&mut injector, &stub, &line).body, line);
    assert_eq!(stub.count(), 0);
}

#[test]
fn the_tool_list_is_fetched_once_per_session() {
    let stub = Stub::new(vec![tool_list()]);
    let mut injector = Injector::new(Some("p1"));
    for _ in 0..3 {
        apply(&mut injector, &stub, &call("show", &json!({"handle":"t1"})));
    }
    assert_eq!(stub.count(), 1);
}

#[test]
fn a_batch_item_gets_the_same_rule() {
    let stub = Stub::new(vec![tool_list()]);
    let mut injector = Injector::new(Some("p1"));
    let line = format!(
        "[{},{}]",
        call("show", &json!({"handle":"t1"})),
        call("echo", &json!({}))
    );
    let sent = apply(&mut injector, &stub, &line);
    let body: Value = serde_json::from_str(&sent.body).unwrap();
    assert_eq!(body[0]["params"]["arguments"]["project"], "p1");
    assert!(body[1]["params"]["arguments"].get("project").is_none());
}

#[test]
fn a_failed_list_forwards_the_original_with_one_diag() {
    let stub = Stub::new(vec![Err(VivariumError::Message("refused".into()))]);
    let mut injector = Injector::new(Some("p1"));
    let line = call("show", &json!({"handle":"t1"}));
    let sent = apply(&mut injector, &stub, &line);
    assert_eq!(sent.body, line);
    assert!(sent.diag.expect("a diag").contains("refused"));
}

#[test]
fn the_bridges_own_list_reply_never_reaches_the_output() {
    let stub = Stub::new(vec![
        tool_list(),
        answer(&json!({"jsonrpc":"2.0","id":7,"result":{"ok":true}})),
    ]);
    let line = call("show", &json!({"handle":"t1"}));
    let mut output: Vec<u8> = Vec::new();
    let mut diag: Vec<u8> = Vec::new();
    serve_with_project(
        &stub,
        "key",
        Some("p1"),
        format!("{line}\n").as_bytes(),
        &mut output,
        &mut diag,
    )
    .expect("serve");
    let text = String::from_utf8(output).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains("\"ok\":true"));
    assert_eq!(stub.count(), 2);
    assert_eq!(stub.body(1)["params"]["arguments"]["project"], "p1");
}
