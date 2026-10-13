//! Tests for the MCP bridge engine: a scripted [`Upstream`], no sockets.

use std::cell::RefCell;
use std::collections::VecDeque;

use serde_json::{Value, json};

use super::*;

/// The upstream records every bearer and body it saw and pops scripted
/// answers and mints; running out of script is a test bug.
struct Stub {
    posts: RefCell<Vec<(String, Vec<u8>)>>,
    answers: RefCell<VecDeque<Result<RawResponse, VivariumError>>>,
    mints: RefCell<VecDeque<Result<MintedToken, VivariumError>>>,
    minted: RefCell<Vec<String>>,
}

impl Stub {
    fn new(
        answers: Vec<Result<RawResponse, VivariumError>>,
        mints: Vec<Result<MintedToken, VivariumError>>,
    ) -> Self {
        Self {
            posts: RefCell::new(Vec::new()),
            answers: RefCell::new(answers.into()),
            mints: RefCell::new(mints.into()),
            minted: RefCell::new(Vec::new()),
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

    /// A scripted mint with the given token and expiry.
    fn token(token: &str, expires_at: &str) -> Result<MintedToken, VivariumError> {
        Ok(MintedToken {
            token: token.to_string(),
            expires_at: Some(expires_at.to_string()),
        })
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

    fn mint_count(&self) -> usize {
        self.minted.borrow().len()
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

    fn mint(&self) -> Result<MintedToken, VivariumError> {
        let minted = self
            .mints
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected mint #{}", self.mint_count()));
        if let Ok(token) = &minted {
            self.minted.borrow_mut().push(token.token.clone());
        }
        minted
    }
}

/// Run one session over `input` and return (output, diag) as strings.
fn run(stub: &Stub, credential: &Credential, input: &str) -> (String, String) {
    let mut output: Vec<u8> = Vec::new();
    let mut diag: Vec<u8> = Vec::new();
    serve_with(stub, credential, input.as_bytes(), &mut output, &mut diag).expect("serve session");
    (
        String::from_utf8(output).expect("output utf-8"),
        String::from_utf8(diag).expect("diag utf-8"),
    )
}

/// A member credential backed by a throwaway 32-byte key file.
fn member() -> Credential {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("identity.ed25519");
    std::fs::write(&path, [0x2a; 32]).expect("write key file");
    let identity = SyncIdentity::from_key_file(&path).expect("identity");
    Credential::Member(identity)
}

/// An RFC 3339 stamp `seconds` from now.
fn rfc3339_in(seconds: i64) -> String {
    (Utc::now() + chrono::Duration::seconds(seconds))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn lines_of(output: &str) -> Vec<&str> {
    output.lines().collect()
}

#[test]
fn requests_are_forwarded_byte_for_byte_and_answered_in_order() {
    let answer1 = json!({"jsonrpc":"2.0","id":1,"result":{"server":"vivi"}});
    let answer2 = json!({"jsonrpc":"2.0","id":"tools","result":{"tools":[]}});
    let answer3 = json!({"jsonrpc":"2.0","id":3,"result":{"ok":true}});
    let stub = Stub::new(
        vec![
            Stub::json(200, &answer1),
            Stub::json(200, &answer2),
            Stub::json(200, &answer3),
        ],
        vec![],
    );
    let line1 = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"name":"probe"}}"#;
    let line2 = r#"{"jsonrpc":"2.0","id":"tools","method":"tools/list"}"#;
    let line3 = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"board"}}"#;
    let input = format!("{line1}\n{line2}\n{line3}\n");

    let (output, diag) = run(&stub, &Credential::ApiKey("key".into()), &input);

    assert_eq!(stub.post_count(), 3);
    assert_eq!(stub.body(0), line1.as_bytes());
    assert_eq!(stub.body(1), line2.as_bytes());
    assert_eq!(stub.body(2), line3.as_bytes());
    assert_eq!(stub.mint_count(), 0);
    let lines = lines_of(&output);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], answer1.to_string());
    assert_eq!(lines[1], answer2.to_string());
    assert_eq!(lines[2], answer3.to_string());
    assert!(diag.is_empty());
}

#[test]
fn a_notification_is_forwarded_with_no_reply_and_blank_lines_are_skipped() {
    let stub = Stub::new(
        vec![Ok(RawResponse {
            status: 202,
            content_type: None,
            body: Vec::new(),
        })],
        vec![],
    );
    let notification = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let input = format!("\n   \n{notification}\n\n");

    let (output, diag) = run(&stub, &Credential::ApiKey("k".into()), &input);

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
        let stub = Stub::new(vec![empty], vec![]);
        let request = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#;
        let (output, _diag) = run(
            &stub,
            &Credential::ApiKey("k".into()),
            &format!("{request}\n"),
        );
        assert!(output.is_empty());
    }
}

#[test]
fn a_line_that_is_not_json_is_answered_with_a_parse_error_and_not_forwarded() {
    let stub = Stub::new(vec![], vec![]);

    let (output, diag) = run(&stub, &Credential::ApiKey("k".into()), "{not json\n");

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
        let stub = Stub::new(vec![], vec![]);
        let (output, diag) = run(&stub, &Credential::ApiKey("k".into()), &format!("{bare}\n"));
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
fn a_member_request_that_meets_401_renews_once_and_retries_with_the_new_bearer() {
    let answer = json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}});
    let refused = json!({"error":{"code":"stale_token","message":"expired"}});
    let stub = Stub::new(
        vec![Stub::status(401, &refused), Stub::json(200, &answer)],
        vec![
            Stub::token("first-token", &rfc3339_in(3600)),
            Stub::token("second-token", &rfc3339_in(3600)),
        ],
    );
    let request = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;

    let (output, diag) = run(&stub, &member(), &format!("{request}\n"));

    assert_eq!(stub.mint_count(), 2, "first mint plus the one 401 renewal");
    assert_eq!(stub.post_count(), 2);
    assert_eq!(stub.bearer(0), "first-token");
    assert_eq!(
        stub.bearer(1),
        "second-token",
        "the retry used the new bearer"
    );
    assert_eq!(
        stub.body(0),
        request.as_bytes(),
        "the retry resends the same bytes"
    );
    assert_eq!(stub.body(1), request.as_bytes());
    let lines = lines_of(&output);
    assert_eq!(lines.len(), 1);
    let reply: Value = serde_json::from_str(lines[0]).expect("reply");
    assert_eq!(reply, answer);
    assert!(diag.is_empty());
    assert!(
        !output.contains("first-token") && !output.contains("second-token"),
        "a token leaked into output: {output}"
    );
}

#[test]
fn a_second_401_ends_that_request_and_the_next_line_is_still_served() {
    let refused = json!({"error":{"code":"stale_token","message":"expired"}});
    let recovered = json!({"jsonrpc":"2.0","id":9,"result":{"after":"failure"}});
    let stub = Stub::new(
        vec![
            Stub::status(401, &refused),
            Stub::status(401, &refused),
            Stub::status(401, &refused),
            Stub::status(401, &refused),
            Stub::json(200, &recovered),
        ],
        vec![
            Stub::token("t-one", &rfc3339_in(3600)),
            Stub::token("t-two", &rfc3339_in(3600)),
            Stub::token("t-three", &rfc3339_in(3600)),
        ],
    );
    let numeric = r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#;
    let textual = r#"{"jsonrpc":"2.0","id":"seven","method":"tools/list"}"#;
    let next = r#"{"jsonrpc":"2.0","id":9,"method":"tools/list"}"#;

    let (output, diag) = run(&stub, &member(), &format!("{numeric}\n{textual}\n{next}\n"));

    assert_eq!(stub.mint_count(), 3, "one renewal per 401-meeting request");
    let lines = lines_of(&output);
    assert_eq!(
        lines.len(),
        3,
        "both failures answered, then recovery: {output}"
    );
    let first: Value = serde_json::from_str(lines[0]).expect("reply");
    assert_eq!(first["id"], json!(7), "numeric id verbatim");
    assert_eq!(first["error"]["code"].as_i64(), Some(CODE_UNSERVED));
    let message = first["error"]["message"].as_str().expect("message");
    assert!(message.contains("authentication"), "{message}");
    assert!(message.contains("401"), "{message}");
    assert!(message.contains("stale_token"), "{message}");
    assert_eq!(first["error"]["data"]["http_status"], json!(401));
    let second: Value = serde_json::from_str(lines[1]).expect("reply");
    assert_eq!(second["id"], json!("seven"), "string id verbatim");
    let third: Value = serde_json::from_str(lines[2]).expect("reply");
    assert_eq!(third, recovered);
    assert_eq!(stub.bearer(4), "t-three");
    assert!(diag.is_empty());
    assert!(
        !output.contains("t-one") && !output.contains("t-two") && !output.contains("t-three"),
        "a token leaked into output: {output}"
    );
}

#[test]
fn an_api_key_is_the_bearer_as_given_and_its_401_is_final() {
    let refused = json!({"error":{"code":"bad_key","message":"no such key"}});
    let stub = Stub::new(vec![Stub::status(401, &refused)], vec![]);
    let request = r#"{"jsonrpc":"2.0","id":"a","method":"tools/list"}"#;

    let (output, diag) = run(
        &stub,
        &Credential::ApiKey("k-123".into()),
        &format!("{request}\n"),
    );

    assert_eq!(stub.post_count(), 1);
    assert_eq!(stub.bearer(0), "k-123", "the bearer is the key as given");
    assert_eq!(stub.mint_count(), 0, "an API key is never minted");
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
    let stub = Stub::new(
        vec![
            Ok(RawResponse {
                status: 200,
                content_type: Some("application/json".into()),
                body: pretty.into_bytes(),
            }),
            Stub::sse(sse_body),
        ],
        vec![],
    );
    let request1 = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let request2 = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call"}"#;

    let (output, diag) = run(
        &stub,
        &Credential::ApiKey("k".into()),
        &format!("{request1}\n{request2}\n"),
    );

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
fn a_member_token_near_expiry_is_renewed_before_the_next_request() {
    let answer = |id: i64| json!({"jsonrpc":"2.0","id":id,"result":id});
    let stub = Stub::new(
        vec![
            Stub::json(200, &answer(1)),
            Stub::json(200, &answer(2)),
            Stub::json(200, &answer(3)),
            Stub::json(200, &answer(4)),
        ],
        vec![
            Stub::token("stale-token", &rfc3339_in(-60)),
            Stub::token("edge-token", &rfc3339_in(10)),
            Stub::token("fresh-token", &rfc3339_in(3600)),
        ],
    );

    let (output, diag) = run(
        &stub,
        &member(),
        "{\"jsonrpc\":\"2.0\",\"id\":1}\n{\"jsonrpc\":\"2.0\",\"id\":2}\n\
         {\"jsonrpc\":\"2.0\",\"id\":3}\n{\"jsonrpc\":\"2.0\",\"id\":4}\n",
    );

    assert_eq!(
        stub.mint_count(),
        3,
        "past and within-30s renew; far future does not"
    );
    assert_eq!(stub.bearer(0), "stale-token");
    assert_eq!(stub.bearer(1), "edge-token", "an expiry within 30 s renews");
    assert_eq!(stub.bearer(2), "fresh-token");
    assert_eq!(stub.bearer(3), "fresh-token", "a far-future expiry is kept");
    assert_eq!(lines_of(&output).len(), 4);
    assert!(diag.is_empty());
}

#[test]
fn a_transport_error_answers_requests_and_only_logs_notifications() {
    let stub = Stub::new(vec![Stub::unreachable(), Stub::unreachable()], vec![]);
    let request = r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#;
    let notification = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

    let (output, diag) = run(
        &stub,
        &Credential::ApiKey("k".into()),
        &format!("{request}\n{notification}\n"),
    );

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
fn a_failed_mint_ends_the_member_request_and_a_notification_only_logs() {
    let stub = Stub::new(
        vec![],
        vec![
            Err(VivariumError::Message("mint refused".into())),
            Err(VivariumError::Message("mint refused".into())),
        ],
    );
    let request = r#"{"jsonrpc":"2.0","id":5,"method":"tools/list"}"#;
    let notification = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

    let (output, diag) = run(&stub, &member(), &format!("{request}\n{notification}\n"));

    let lines = lines_of(&output);
    assert_eq!(lines.len(), 1, "only the request is answered: {output}");
    let reply: Value = serde_json::from_str(lines[0]).expect("reply");
    assert_eq!(reply["id"], json!(5));
    assert!(
        reply["error"]["message"]
            .as_str()
            .expect("message")
            .contains("authentication")
    );
    assert!(reply["error"]["data"]["http_status"].is_null());
    assert_eq!(diag.lines().count(), 1, "the notification logged one line");
    assert!(diag.starts_with("vivi mcp: "), "{diag}");
}

#[test]
fn a_batch_that_cannot_be_served_is_answered_once_with_id_null() {
    let refused = json!({"error":{"code":"project_not_found","message":"no such project"}});
    let stub = Stub::new(vec![Stub::status(404, &refused)], vec![]);
    let batch =
        r#"[{"jsonrpc":"2.0","id":1,"method":"tools/list"},{"jsonrpc":"2.0","method":"x"}]"#;

    let (output, diag) = run(
        &stub,
        &Credential::ApiKey("k".into()),
        &format!("{batch}\n"),
    );

    let reply: Value = serde_json::from_str(output.trim()).expect("one reply line");
    assert!(reply["id"].is_null(), "a batch error has id null");
    assert_eq!(reply["error"]["code"].as_i64(), Some(CODE_UNSERVED));
    assert_eq!(reply["error"]["data"]["http_status"], json!(404));
    let message = reply["error"]["message"].as_str().expect("message");
    assert!(message.contains("project_not_found"), "{message}");
    assert!(diag.is_empty());
}

#[test]
fn no_token_or_key_text_reaches_output_or_diag_in_a_mixed_session() {
    let answer = json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}});
    let refused = json!({"error":{"code":"stale_token","message":"expired"}});
    let stub = Stub::new(
        vec![
            Stub::status(401, &refused),
            Stub::json(200, &answer),
            Stub::unreachable(),
        ],
        vec![
            Stub::token("secret-token-one", &rfc3339_in(3600)),
            Stub::token("secret-token-two", &rfc3339_in(3600)),
        ],
    );
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
    );

    let (output, diag) = run(&stub, &member(), input);

    for secret in ["secret-token-one", "secret-token-two"] {
        assert!(!output.contains(secret), "{secret} leaked into output");
        assert!(!diag.contains(secret), "{secret} leaked into diag");
    }
    assert_eq!(stub.mint_count(), 2);
    assert!(diag.starts_with("vivi mcp: "), "{diag}");
    assert_eq!(lines_of(&output).len(), 1);
}
