//! Outbox send/flush/read tests against a std-lib TCP HTTP stub (no new
//! dev-dependencies).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::mpsc;

use super::*;
use crate::chat::state::ChatState;
use crate::chat::{b64, chat_dir, ensure_chat_dir};
use crate::storage::is_traveling_id;
use ed25519_dalek::{Signature, Verifier};
use serde_json::{Value, json};

const ROOM: &str = "abcde-fghjk-mnpqr-stvwz-23456";
const CHALLENGE: [u8; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31,
];

/// One recorded request: (method, path, body, request head).
type Request = (String, String, String, String);

fn stub_server(
    count: usize,
    handler: impl Fn(&str, &str, &str, &str) -> (u16, String) + Send + Sync + 'static,
) -> (String, mpsc::Receiver<Request>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let addr = listener.local_addr().expect("stub addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut seen = 0;
        for stream in listener.incoming() {
            if seen == count {
                break;
            }
            let Ok(mut stream) = stream else { break };
            let Some((head, body)) = read_request(&mut stream) else {
                continue;
            };
            let mut lines = head.lines();
            let request_line = lines.next().unwrap_or_default().to_string();
            let mut parts = request_line.split(' ');
            let method = parts.next().unwrap_or_default().to_string();
            let path = parts.next().unwrap_or_default().to_string();
            let content_length = head
                .lines()
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
                .map(|(_, v)| v.trim().parse::<usize>().unwrap_or(0))
                .unwrap_or(0);
            let body = pad_body(&mut stream, body, content_length);
            let _ = tx.send((method, path.clone(), body.clone(), head.clone()));
            let (status, payload) = handler(&request_line, &path, &body, &head);
            let response = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = stream.write_all(response.as_bytes());
            seen += 1;
        }
    });
    (format!("http://{addr}"), rx)
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<(String, String)> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        stream.read(&mut byte).ok()?;
        buf.push(byte[0]);
    }
    let split = buf.len() - 4;
    Some((
        String::from_utf8_lossy(&buf[..split]).into_owned(),
        String::new(),
    ))
}

fn pad_body(stream: &mut std::net::TcpStream, mut body: String, content_length: usize) -> String {
    let mut chunk = [0u8; 4096];
    while body.len() < content_length {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            break;
        }
        body.push_str(&String::from_utf8_lossy(&chunk[..n]));
    }
    body
}

fn identity_with_seed(dir: &Path, seed: u8) -> ChatIdentity {
    ensure_chat_dir(dir).expect("chat dir");
    std::fs::write(dir.join("identity.ed25519"), [seed; 32]).expect("seed file");
    ChatIdentity::ensure(dir).expect("load seeded identity")
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

fn handler_flow(token: &str) -> impl Fn(&str, &str, &str, &str) -> (u16, String) + '_ {
    let token = token.to_string();
    move |_line, path, body, head| {
        if path.ends_with("/challenge") {
            (
                200,
                json!({
                    "challenge_id": "ch-1",
                    "challenge": b64::encode(&CHALLENGE),
                    "expires_in": 120,
                })
                .to_string(),
            )
        } else if path.ends_with("/auth/token") {
            verify_stub_signature(&body);
            (
                200,
                json!({ "token": token, "token_expires_at": "2026-10-09T20:00:00Z" }).to_string(),
            )
        } else {
            let message_id = header(head, "message-id").unwrap_or("m-stub").to_string();
            (
                201,
                json!({
                    "message_id": message_id,
                    "cursor": 5,
                    "created_at": "2026-10-09T18:00:00Z",
                })
                .to_string(),
            )
        }
    }
}

fn verify_stub_signature(body: &str) {
    let request: Value = serde_json::from_str(body).expect("token body json");
    let signature =
        b64::decode(request["signature_base64"].as_str().expect("sig")).expect("sig base64");
    let verifying = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]).verifying_key();
    let signature = Signature::from_slice(&signature).expect("sig len");
    assert!(
        verifying.verify(&CHALLENGE, &signature).is_ok(),
        "client must sign the exact challenge bytes"
    );
}

#[test]
fn send_happy_path_mints_bearer_and_posts_envelope() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = chat_dir(tmp.path());
    let identity = identity_with_seed(&dir, 7);
    let state = ChatState::open(&dir).expect("state");
    state.join_room(ROOM, 2, "burgus/vivi").expect("joined");

    let (server, rx) = stub_server(3, handler_flow("tok-1"));
    let report = send_message(
        &server,
        &state,
        &identity,
        ROOM,
        Some("seat-b"),
        Some("hello"),
        "body text",
    )
    .expect("send");

    assert!(
        report.delivered,
        "send should deliver: {:?}",
        report.summary.error
    );
    assert_eq!(state.outbox_depth().expect("depth"), 0);
    let requests: Vec<Request> = rx.try_iter().collect();
    assert_eq!(requests.len(), 3, "challenge, token, send");

    let challenge: Value = serde_json::from_str(&requests[0].2).expect("challenge body");
    assert_eq!(
        challenge["fingerprint"].as_str().expect("fp"),
        identity.wire_fingerprint(),
        "refresh challenge targets the wire fingerprint"
    );
    assert!(
        requests[1]
            .1
            .ends_with(&format!("/v1/rooms/{ROOM}/auth/token"))
    );

    let (method, path, body, head) = &requests[2];
    assert_eq!(method, "POST");
    assert!(path.ends_with(&format!("/v1/rooms/{ROOM}/messages")));
    assert_eq!(header(head, "authorization"), Some("Bearer tok-1"));
    assert_eq!(header(head, "x-vivi-chat-from"), Some("burgus/vivi"));
    assert_eq!(
        header(head, "x-vivi-chat-fingerprint"),
        Some(identity.short_wire_fingerprint().as_str())
    );
    assert_eq!(header(head, "message-id"), Some(report.message_id.as_str()));
    assert!(
        is_traveling_id(&report.message_id),
        "the wire Message-ID is the traveling id: {}",
        report.message_id
    );
    let payload: Value = serde_json::from_str(body).expect("send body json");
    assert_eq!(payload["to"], "seat-b");
    assert_eq!(payload["subject"], "hello");
    assert_eq!(payload["body"], "body text");
    assert!(
        payload.get("message_id").is_none(),
        "Message-ID rides the header, not the body"
    );
}

#[test]
fn server_down_enqueues_then_flush_after_restart_delivers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = chat_dir(tmp.path());
    let identity = identity_with_seed(&dir, 7);
    let state = ChatState::open(&dir).expect("state");
    state.join_room(ROOM, 2, "burgus/vivi").expect("joined");

    let report = send_message(
        "http://127.0.0.1:1",
        &state,
        &identity,
        ROOM,
        None,
        None,
        "offline body",
    )
    .expect("send enqueues");
    assert!(!report.delivered, "server down must not deliver");
    assert!(
        report
            .queued_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("unreachable")),
        "queued reason should name the failure: {:?}",
        report.queued_reason
    );
    assert_eq!(state.outbox_depth().expect("depth"), 1);

    let (server, rx) = stub_server(3, handler_flow("tok-2"));
    let summary = deliver_all(&server, &state, &identity).expect("flush");
    drop(rx);
    assert_eq!(summary.sent, 1);
    assert_eq!(summary.queued, 0);
    assert!(summary.sent_ids.contains(&report.message_id));
    assert_eq!(
        state.outbox_depth().expect("depth"),
        0,
        "ack clears the row"
    );
}

#[test]
fn duplicate_ack_still_marks_flushed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = chat_dir(tmp.path());
    let identity = identity_with_seed(&dir, 7);
    let state = ChatState::open(&dir).expect("state");
    state.join_room(ROOM, 2, "burgus/vivi").expect("joined");

    let (server, rx) = stub_server(3, |_line, path, body, head| {
        if path.ends_with("/challenge") {
            (
                200,
                json!({ "challenge_id": "ch-1", "challenge": b64::encode(&CHALLENGE) }).to_string(),
            )
        } else if path.ends_with("/auth/token") {
            verify_stub_signature(&body);
            (200, json!({ "token": "tok-3" }).to_string())
        } else {
            let message_id = header(head, "message-id").unwrap_or("m").to_string();
            (
                200,
                json!({
                    "message_id": message_id,
                    "cursor": 3,
                    "created_at": "2026-10-09T18:00:00Z",
                    "duplicate": true,
                })
                .to_string(),
            )
        }
    });
    drop(rx);
    let (message_id, envelope) = compose_message(None, Some("again"), "retry body", &identity);
    state
        .enqueue(ROOM, &message_id, &envelope.to_string())
        .expect("enqueue");

    let summary = deliver_all(&server, &state, &identity).expect("flush");
    assert_eq!(
        (summary.sent, summary.duplicates, summary.queued),
        (1, 1, 0)
    );
    assert_eq!(state.outbox_depth().expect("depth"), 0);
}

#[test]
fn read_advances_cursor_and_renders_origin_labels() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = chat_dir(tmp.path());
    let identity = identity_with_seed(&dir, 7);
    let state = ChatState::open(&dir).expect("state");
    state.join_room(ROOM, 1, "burgus/vivi").expect("joined");

    let (server, rx) = stub_server(3, |_line, path, body, head| {
        if path.ends_with("/challenge") {
            (
                200,
                json!({ "challenge_id": "ch-1", "challenge": b64::encode(&CHALLENGE) }).to_string(),
            )
        } else if path.ends_with("/auth/token") {
            verify_stub_signature(&body);
            (200, json!({ "token": "tok-4" }).to_string())
        } else {
            assert_eq!(header(head, "authorization"), Some("Bearer tok-4"));
            assert_eq!(header(head, "x-vivi-chat-from"), Some("burgus/vivi"));
            (
                200,
                json!({
                    "messages": [{
                        "message_id": "m-9",
                        "cursor": 7,
                        "from": "other/vivi",
                        "fingerprint": "0123456789abcdef",
                        "to": null,
                        "subject": "hi",
                        "body": "there",
                        "created_at": "2026-10-09T18:00:00Z",
                    }],
                    "next_cursor": 7,
                })
                .to_string(),
            )
        }
    });

    let page = read_room(&server, &state, &identity, ROOM, true, false).expect("read");
    let requests: Vec<Request> = rx.try_iter().collect();
    assert!(
        requests[2].1.contains("since=0"),
        "first read starts at zero"
    );
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.next_cursor, 7);
    assert!(page.advanced);
    assert_eq!(state.cursor(ROOM).expect("cursor"), Some("7".into()));

    let rendered = render_message(&page.messages[0]);
    for needle in [
        "other/vivi",
        "0123456789abcdef",
        "2026-10-09T18:00:00Z",
        "all",
        "hi",
    ] {
        assert!(
            rendered.contains(needle),
            "render should contain '{needle}'"
        );
    }
}

#[test]
fn send_requires_a_joined_room() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = chat_dir(tmp.path());
    let identity = identity_with_seed(&dir, 7);
    let state = ChatState::open(&dir).expect("state");
    let error = send_message(
        "http://127.0.0.1:1",
        &state,
        &identity,
        ROOM,
        None,
        None,
        "x",
    )
    .expect_err("not joined");
    assert!(error.to_string().contains("not joined"));
}

#[test]
fn composed_ids_are_distinct_traveling_ids() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = chat_dir(tmp.path());
    let identity = identity_with_seed(&dir, 7);

    let (first, envelope) = compose_message(Some("seat-b"), None, "one", &identity);
    let (second, _) = compose_message(Some("seat-b"), None, "two", &identity);

    assert!(is_traveling_id(&first), "{first}");
    assert!(is_traveling_id(&second), "{second}");
    assert_ne!(first, second);
    assert_eq!(envelope["message_id"], first.as_str());
}

#[test]
fn queued_send_keeps_its_traveling_id_when_flushed_later() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = chat_dir(tmp.path());
    let identity = identity_with_seed(&dir, 7);
    let state = ChatState::open(&dir).expect("state");
    state.join_room(ROOM, 2, "burgus/vivi").expect("joined");

    let report = send_message(
        "http://127.0.0.1:1",
        &state,
        &identity,
        ROOM,
        None,
        None,
        "offline body",
    )
    .expect("send enqueues");
    assert!(!report.delivered);
    assert!(is_traveling_id(&report.message_id), "{}", report.message_id);
    let pending = state.pending_outbox().expect("pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].message_id, report.message_id);

    let (server, rx) = stub_server(3, handler_flow("tok-5"));
    deliver_all(&server, &state, &identity).expect("flush");
    let requests: Vec<Request> = rx.try_iter().collect();
    let (_, _, _, head) = &requests[2];
    assert_eq!(
        header(head, "message-id"),
        Some(report.message_id.as_str()),
        "the flush re-sends the minted id; nothing re-mints"
    );
}
