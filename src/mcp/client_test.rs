//! Tests for the one-shot MCP client: a scripted [`Upstream`], no sockets.

use std::cell::RefCell;
use std::collections::VecDeque;

use serde_json::{Value, json};

use super::*;
use crate::error::VivariumError;
use crate::sync::http::{MintedToken, RawResponse};

struct Stub {
    bodies: RefCell<Vec<Value>>,
    answers: RefCell<VecDeque<RawResponse>>,
}

impl Stub {
    fn new(answers: Vec<RawResponse>) -> Self {
        Self {
            bodies: RefCell::new(Vec::new()),
            answers: RefCell::new(answers.into()),
        }
    }

    fn methods(&self) -> Vec<String> {
        self.bodies
            .borrow()
            .iter()
            .map(|body| body["method"].as_str().unwrap_or_default().to_string())
            .collect()
    }
}

impl Upstream for Stub {
    fn post(&self, _bearer: &str, body: &[u8]) -> Result<RawResponse, VivariumError> {
        self.bodies
            .borrow_mut()
            .push(serde_json::from_slice(body).expect("request body is JSON"));
        Ok(self
            .answers
            .borrow_mut()
            .pop_front()
            .expect("scripted answer"))
    }

    fn mint(&self) -> Result<MintedToken, VivariumError> {
        panic!("an API key never mints");
    }
}

fn reply(id: u64, result: &Value) -> RawResponse {
    RawResponse {
        status: 200,
        content_type: Some("application/json".into()),
        body: json!({ "jsonrpc": "2.0", "id": id, "result": result })
            .to_string()
            .into_bytes(),
    }
}

fn accepted() -> RawResponse {
    RawResponse {
        status: 202,
        content_type: None,
        body: Vec::new(),
    }
}

fn handshake() -> Vec<RawResponse> {
    vec![
        reply(1, &json!({ "protocolVersion": PROTOCOL_VERSION })),
        accepted(),
    ]
}

fn key() -> Credential {
    Credential::ApiKey("secret-key".into())
}

#[test]
fn connect_initializes_then_notifies() {
    let stub = Stub::new(handshake());
    let credential = key();
    Client::connect(&stub, &credential).expect("connect");
    assert_eq!(stub.methods(), ["initialize", "notifications/initialized"]);
    assert!(stub.bodies.borrow()[1].get("id").is_none());
}

#[test]
fn tools_follow_the_cursor() {
    let mut answers = handshake();
    answers.push(reply(
        2,
        &json!({ "tools": [{ "name": "a" }], "nextCursor": "p2" }),
    ));
    answers.push(reply(3, &json!({ "tools": [{ "name": "b" }] })));
    let stub = Stub::new(answers);
    let credential = key();
    let mut client = Client::connect(&stub, &credential).expect("connect");
    let tools = client.tools().expect("tools");
    assert_eq!(tools.len(), 2);
    assert_eq!(stub.bodies.borrow()[3]["params"]["cursor"], "p2");
}

#[test]
fn a_json_rpc_error_is_typed() {
    let mut answers = handshake();
    answers.push(RawResponse {
        status: 200,
        content_type: Some("application/json".into()),
        body: json!({ "jsonrpc": "2.0", "id": 2,
            "error": { "code": -32601, "message": "method not found" } })
        .to_string()
        .into_bytes(),
    });
    let stub = Stub::new(answers);
    let credential = key();
    let mut client = Client::connect(&stub, &credential).expect("connect");
    let error = client.call_tool("nope", &json!({})).unwrap_err();
    assert_eq!(
        error,
        ClientError::Rpc {
            code: -32601,
            message: "method not found".into()
        }
    );
    assert!(!error.to_string().contains("secret-key"));
}

#[test]
fn an_http_refusal_names_the_status() {
    let stub = Stub::new(vec![RawResponse {
        status: 403,
        content_type: Some("application/json".into()),
        body: json!({ "error": { "code": "forbidden", "message": "no" } })
            .to_string()
            .into_bytes(),
    }]);
    let credential = key();
    let Err(error) = Client::connect(&stub, &credential) else {
        panic!("expected a refusal");
    };
    assert!(error.to_string().contains("HTTP 403"), "{error}");
}

#[test]
fn a_sse_reply_is_matched_by_id() {
    let mut answers = handshake();
    answers.push(RawResponse {
        status: 200,
        content_type: Some("text/event-stream".into()),
        body: b"event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":99,\"result\":{}}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"ok\":true}}\n\n".to_vec(),
    });
    let stub = Stub::new(answers);
    let credential = key();
    let mut client = Client::connect(&stub, &credential).expect("connect");
    let result = client.call_tool("t", &json!({})).expect("call");
    assert_eq!(result["ok"], true);
}

#[test]
fn rendering_helpers() {
    let tools = [
        json!({ "name": "show", "description": "Show one item\nmore detail" }),
        json!({ "name": "bare" }),
    ];
    assert_eq!(tool_lines(&tools), ["show\tShow one item", "bare"]);
    let result = json!({ "content": [
        { "type": "text", "text": "one" },
        { "type": "image", "data": "x" },
        { "type": "text", "text": "two" }
    ] });
    assert_eq!(result_text(&result), "one\ntwo");
    assert!(result_is_error(&json!({ "isError": true })));
    assert!(!result_is_error(&json!({ "isError": false })));
    assert!(!result_is_error(&json!({})));
}

#[test]
fn arguments_must_be_a_json_object() {
    assert_eq!(parse_arguments(r#"{"a":1}"#).unwrap(), json!({ "a": 1 }));
    assert!(parse_arguments("[1]").unwrap_err().contains("object"));
    assert!(parse_arguments("\"s\"").unwrap_err().contains("object"));
    assert!(parse_arguments("{nope").unwrap_err().contains("not JSON"));
}

fn project_tools() -> Vec<Value> {
    vec![
        json!({"name": "send", "inputSchema": {"properties": {"project": {"type": "string"}, "to": {}}}}),
        json!({"name": "ping", "inputSchema": {"properties": {}}}),
        json!({"name": "bare"}),
    ]
}

#[test]
fn takes_project_reads_the_input_schema() {
    let tools = project_tools();
    assert!(takes_project(&tools, "send"));
    assert!(!takes_project(&tools, "ping"));
    assert!(!takes_project(&tools, "bare"));
    assert!(!takes_project(&tools, "missing"));
}

#[test]
fn with_project_adds_the_key_and_keeps_the_rest() {
    let result = with_project(&json!({"to": "bob"}), "abcde-fghjk");
    assert_eq!(result, json!({"to": "bob", "project": "abcde-fghjk"}));
}

#[test]
fn with_project_keeps_an_explicit_project() {
    let result = with_project(&json!({"project": "mine"}), "abcde-fghjk");
    assert_eq!(result, json!({"project": "mine"}));
}

#[test]
fn with_project_leaves_a_non_object_unchanged() {
    assert_eq!(with_project(&json!("text"), "p"), json!("text"));
    assert_eq!(with_project(&json!(null), "p"), json!(null));
}
