//! Tests for the MCP bridge engine: a scripted [`Upstream`], no sockets.

use std::cell::RefCell;
use std::collections::VecDeque;

use serde_json::{Value, json};

use super::*;

/// The upstream records every bearer and body it saw and pops scripted
/// answers; running out of script is a test bug.
struct Stub {
    posts: RefCell<Vec<(String, Vec<u8>)>>,
    answers: RefCell<VecDeque<Result<RawResponse, VivariumError>>>,
}

impl Stub {
    fn new(answers: Vec<Result<RawResponse, VivariumError>>) -> Self {
        Self {
            posts: RefCell::new(Vec::new()),
            answers: RefCell::new(answers.into()),
        }
    }

    /// A 200 JSON answer, application/json, compact body.
    fn json(status: u16, value: &Value) -> Result<RawResponse, VivariumError> {
        Ok(RawResponse {
            status,
            content_type: Some("application/json".into()),
            body: value.to_string().into_bytes(),
        })
    }

    /// A non-2xx JSON answer with an error body.
    fn status(status: u16, body: &Value) -> Result<RawResponse, VivariumError> {
        Ok(RawResponse {
            status,
            content_type: Some("application/json".into()),
            body: body.to_string().into_bytes(),
        })
    }

    /// A 200 SSE answer with the exact frame text.
    fn sse(body: &str) -> Result<RawResponse, VivariumError> {
        Ok(RawResponse {
            status: 200,
            content_type: Some("text/event-stream".into()),
            body: body.as_bytes().to_vec(),
        })
    }

    /// A transport error: the server could not be reached.
    fn unreachable() -> Result<RawResponse, VivariumError> {
        Err(VivariumError::Message("connection refused".into()))
    }

    fn bearer(&self, index: usize) -> String {
        self.posts.borrow()[index].0.clone()
    }

    fn body(&self, index: usize) -> Vec<u8> {
        self.posts.borrow()[index].1.clone()
    }

    fn post_count(&self) -> usize {
        self.posts.borrow().len()
    }
}

impl Upstream for Stub {
    fn post(&self, bearer: &str, body: &[u8]) -> Result<RawResponse, VivariumError> {
        self.posts
            .borrow_mut()
            .push((bearer.to_string(), body.to_vec()));
        self.answers.borrow_mut().pop_front().unwrap_or_else(|| {
            panic!(
                "unexpected post #{}, bearer had {} chars",
                self.post_count(),
                bearer.len()
            )
        })
    }
}

/// Run one session over `input` and return (output, diag) as strings.
fn run(stub: &Stub, api_key: &str, input: &str) -> (String, String) {
    let mut output: Vec<u8> = Vec::new();
    let mut diag: Vec<u8> = Vec::new();
    serve_with(stub, api_key, input.as_bytes(), &mut output, &mut diag).expect("serve session");
    (
        String::from_utf8(output).expect("output utf-8"),
        String::from_utf8(diag).expect("diag utf-8"),
    )
}

fn lines_of(output: &str) -> Vec<&str> {
    output.lines().collect()
}

#[test]
fn requests_are_forwarded_byte_for_byte_and_answered_in_order() {
    let answer1 = json!({"jsonrpc":"2.0","id":1,"result":{"server":"vivi"}});
    let answer2 = json!({"jsonrpc":"2.0","id":"tools","result":{"tools":[]}});
    let answer3 = json!({"jsonrpc":"2.0","id":3,"result":{"ok":true}});
    let stub = Stub::new(vec![
        Stub::json(200, &answer1),
        Stub::json(200, &answer2),
        Stub::json(200, &answer3),
    ]);
    let line1 = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"name":"probe"}}"#;
    let line2 = r#"{"jsonrpc":"2.0","id":"tools","method":"tools/list"}"#;
    let line3 = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"board"}}"#;
    let input = format!("{line1}\n{line2}\n{line3}\n");

    let (output, diag) = run(&stub, "key", &input);

    assert_eq!(stub.post_count(), 3);
    assert_eq!(stub.body(0), line1.as_bytes());
    assert_eq!(stub.body(1), line2.as_bytes());
    assert_eq!(stub.body(2), line3.as_bytes());
    let lines = lines_of(&output);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], answer1.to_string());
    assert_eq!(lines[1], answer2.to_string());
    assert_eq!(lines[2], answer3.to_string());
    assert!(diag.is_empty());
}

#[test]
fn a_notification_is_forwarded_with_no_reply_and_blank_lines_are_skipped() {
    let stub = Stub::new(vec![Ok(RawResponse {
        status: 202,
        content_type: None,
        body: Vec::new(),
    })]);
    let notification = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let input = format!("\n   \n{notification}\n\n");

    let (output, diag) = run(&stub, "k", &input);

    assert_eq!(stub.post_count(), 1, "blank lines must not be forwarded");
    assert_eq!(stub.body(0), notification.as_bytes());
    assert!(output.is_empty(), "a notification got a reply: {output}");
    assert!(diag.is_empty());
}

#[test]
fn an_empty_answer_writes_nothing() {
    for empty in [
        Ok(RawResponse {
            status: 202,
            content_type: None,
            body: Vec::new(),
        }),
        Ok(RawResponse {
            status: 204,
            content_type: None,
            body: Vec::new(),
        }),
    ] {
        let stub = Stub::new(vec![empty]);
        let request = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#;
        let (output, _diag) = run(&stub, "k", &format!("{request}\n"));
        assert!(output.is_empty());
    }
}

#[test]
fn a_line_that_is_not_json_is_answered_with_a_parse_error_and_not_forwarded() {
    let stub = Stub::new(vec![]);

    let (output, diag) = run(&stub, "k", "{not json\n");

    assert_eq!(stub.post_count(), 0, "invalid JSON must not be forwarded");
    let reply: Value = serde_json::from_str(output.trim()).expect("reply is one json line");
    assert_eq!(reply["jsonrpc"], "2.0");
    assert!(reply["id"].is_null());
    assert_eq!(reply["error"]["code"].as_i64(), Some(CODE_PARSE_ERROR));
    assert!(
        reply["error"]["message"]
            .as_str()
            .expect("message")
            .starts_with("parse error")
    );
    assert!(diag.is_empty());
}

#[test]
fn a_json_value_that_is_no_message_is_an_invalid_request() {
    for bare in ["42", "\"hi\"", "true", "null"] {
        let stub = Stub::new(vec![]);
        let (output, diag) = run(&stub, "k", &format!("{bare}\n"));
        assert_eq!(stub.post_count(), 0);
        let reply: Value = serde_json::from_str(output.trim()).expect("reply");
        assert!(reply["id"].is_null());
        assert_eq!(
            reply["error"]["code"].as_i64(),
            Some(CODE_INVALID_REQUEST),
            "{bare}"
        );
        assert!(diag.is_empty(), "{bare}");
    }
}

#[test]
fn an_api_key_is_the_bearer_as_given_and_its_401_is_final() {
    let refused = json!({"error":{"code":"bad_key","message":"no such key"}});
    let stub = Stub::new(vec![Stub::status(401, &refused)]);
    let request = r#"{"jsonrpc":"2.0","id":"a","method":"tools/list"}"#;

    let (output, diag) = run(&stub, "k-123", &format!("{request}\n"));

    assert_eq!(stub.post_count(), 1);
    assert_eq!(stub.bearer(0), "k-123", "the bearer is the key as given");
    let reply: Value = serde_json::from_str(output.trim()).expect("reply");
    assert_eq!(reply["id"], json!("a"));
    assert_eq!(reply["error"]["code"].as_i64(), Some(CODE_UNSERVED));
    assert_eq!(reply["error"]["data"]["http_status"], json!(401));
    let message = reply["error"]["message"].as_str().expect("message");
    assert!(message.contains("authentication"), "{message}");
    assert!(message.contains("bad_key"), "{message}");
    assert!(diag.is_empty());
    assert!(
        !output.contains("k-123"),
        "the key leaked into output: {output}"
    );
}

#[test]
fn a_pretty_body_is_one_line_and_an_sse_body_is_unwrapped() {
    let pretty_answer = json!({"jsonrpc":"2.0","id":1,"result":{"note":"multi"}});
    let pretty = serde_json::to_string_pretty(&pretty_answer).expect("pretty body");
    let sse_body = concat!(
        ": keepalive comment\n",
        "data: {\"first\":true}\n",
        "\n",
        "event: ping\n",
        "data: not for the client\n",
        "\n",
        "event: message\n",
        "data: {\"second\":2}\n",
        "\n",
    );
    let stub = Stub::new(vec![
        Ok(RawResponse {
            status: 200,
            content_type: Some("application/json".into()),
            body: pretty.into_bytes(),
        }),
        Stub::sse(sse_body),
    ]);
    let request1 = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let request2 = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call"}"#;

    let (output, diag) = run(&stub, "k", &format!("{request1}\n{request2}\n"));

    let lines = lines_of(&output);
    assert_eq!(lines.len(), 3, "pretty body must stay one line: {output}");
    assert_eq!(lines[0], pretty_answer.to_string());
    assert_eq!(lines[1], "{\"first\":true}", "a frame with no event line");
    assert_eq!(lines[2], "{\"second\":2}", "the event: message frame");
    assert!(
        !output.contains("not for the client"),
        "ping frames are dropped"
    );
    assert!(diag.is_empty());
}

#[test]
fn a_transport_error_answers_requests_and_only_logs_notifications() {
    let stub = Stub::new(vec![Stub::unreachable(), Stub::unreachable()]);
    let request = r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#;
    let notification = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

    let (output, diag) = run(&stub, "k", &format!("{request}\n{notification}\n"));

    assert_eq!(
        stub.post_count(),
        2,
        "the session continues after a failure"
    );
    let lines = lines_of(&output);
    assert_eq!(lines.len(), 1, "only the request is answered: {output}");
    let reply: Value = serde_json::from_str(lines[0]).expect("reply");
    assert_eq!(reply["id"], json!(3));
    assert_eq!(reply["error"]["code"].as_i64(), Some(CODE_UNSERVED));
    assert!(reply["error"]["data"]["http_status"].is_null());
    assert!(
        reply["error"]["message"]
            .as_str()
            .expect("message")
            .contains("unreachable")
    );
    assert_eq!(diag.lines().count(), 1, "the notification logged one line");
    assert!(diag.starts_with("vivi mcp: "), "{diag}");
}

#[test]
fn a_batch_that_cannot_be_served_is_answered_once_with_id_null() {
    let refused = json!({"error":{"code":"project_not_found","message":"no such project"}});
    let stub = Stub::new(vec![Stub::status(404, &refused)]);
    let batch =
        r#"[{"jsonrpc":"2.0","id":1,"method":"tools/list"},{"jsonrpc":"2.0","method":"x"}]"#;

    let (output, diag) = run(&stub, "k", &format!("{batch}\n"));

    let reply: Value = serde_json::from_str(output.trim()).expect("one reply line");
    assert!(reply["id"].is_null(), "a batch error has id null");
    assert_eq!(reply["error"]["code"].as_i64(), Some(CODE_UNSERVED));
    assert_eq!(reply["error"]["data"]["http_status"], json!(404));
    let message = reply["error"]["message"].as_str().expect("message");
    assert!(message.contains("project_not_found"), "{message}");
    assert!(diag.is_empty());
}

#[test]
fn no_key_text_reaches_output_or_diag_in_a_mixed_session() {
    let refused = json!({"error":{"code":"bad_key","message":"no such key"}});
    let stub = Stub::new(vec![Stub::status(401, &refused), Stub::unreachable()]);
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
    );

    let (output, diag) = run(&stub, "secret-api-key", input);

    assert!(!output.contains("secret-api-key"), "key leaked into output");
    assert!(!diag.contains("secret-api-key"), "key leaked into diag");
    assert_eq!(stub.bearer(0), "secret-api-key");
    assert!(diag.starts_with("vivi mcp: "), "{diag}");
    assert_eq!(lines_of(&output).len(), 1);
}
