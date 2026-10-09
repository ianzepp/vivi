//! Join client tests against a std-lib TCP HTTP stub (no new dev-dependencies).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{Value, json};

use super::*;
use crate::chat::state::ChatState;
use crate::chat::{chat_dir, ensure_chat_dir};

const ROOM: &str = "abcde-fghjk-mnpqr-stvwz-23456";
const CHALLENGE: [u8; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31,
];

/// One recorded request: (method, path, body).
type Request = (String, String, String);

/// Serve `count` requests with `handler`, returning the bound base URL and a
/// channel delivering every recorded request in order.
fn stub_server(
    count: usize,
    handler: impl Fn(&str, &str, &str) -> (u16, String) + Send + Sync + 'static,
) -> (String, mpsc::Receiver<Request>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let addr = listener.local_addr().expect("stub addr");
    let handler = Arc::new(handler);
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
            let content_length = lines
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
                .map(|(_, v)| v.trim().parse::<usize>().unwrap_or(0))
                .unwrap_or(0);
            let body = pad_body(&mut stream, body, content_length);
            let _ = tx.send((method.clone(), path.clone(), body.clone()));
            let (status, payload) = handler(&request_line, &path, &body);
            let reason = match status {
                200 => "OK",
                201 => "Created",
                _ => "Error",
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = stream.write_all(response.as_bytes());
            seen += 1;
        }
    });
    (format!("http://{addr}"), rx)
}

/// Read the request head and whatever body bytes arrived with it.
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

fn challenge_ok() -> (u16, String) {
    (
        200,
        json!({
            "challenge_id": "ch-1",
            "challenge": b64::encode(&CHALLENGE),
            "expires_in": 120,
        })
        .to_string(),
    )
}

fn error_json(status: u16, code: &str) -> (u16, String) {
    (
        status,
        json!({ "error": { "code": code, "message": "stub" } }).to_string(),
    )
}

#[test]
fn join_signs_challenge_bytes_and_stores_room() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let chat_dir = chat_dir(tmp.path());
    let identity = identity_with_seed(&chat_dir, 7);
    let captured = Arc::new(Mutex::new(Vec::<Request>::new()));
    let sink = captured.clone();
    let (server, rx) = stub_server(2, move |_line, path, body| {
        sink.lock()
            .expect("sink")
            .push((String::new(), path.to_string(), body.to_string()));
        if path.ends_with("/challenge") {
            challenge_ok()
        } else {
            let request: Value = serde_json::from_str(&body).expect("join body json");
            let signature = b64::decode(request["signature_base64"].as_str().expect("sig"))
                .expect("sig base64");
            let verifying = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]).verifying_key();
            let signature = Signature::from_slice(&signature).expect("sig len");
            assert!(
                verifying.verify(&CHALLENGE, &signature).is_ok(),
                "client must sign the exact challenge bytes"
            );
            let label = request["label"].as_str().expect("label").to_string();
            (
                201,
                json!({
                    "seat": 2,
                    "label": label,
                    "token": "raw-token-not-persisted",
                    "token_expires_at": "2026-10-09T19:00:00Z",
                    "fingerprint": "0123456789abcdef",
                })
                .to_string(),
            )
        }
    });
    drop(rx);

    let receipt = join_room(&server, ROOM, &identity, "burgus/myproj").expect("join");
    assert_eq!(receipt.seat, 2);
    assert_eq!(receipt.label, "burgus/myproj");

    let requests = captured.lock().expect("sink");
    assert_eq!(requests.len(), 2, "challenge then join");
    assert!(
        requests[0]
            .1
            .ends_with(&format!("/v1/rooms/{ROOM}/challenge")),
        "challenge path: {}",
        requests[0].1
    );
    let challenge_body: Value = serde_json::from_str(&requests[0].2).expect("challenge body json");
    assert_eq!(
        challenge_body["public_key_pem"].as_str().expect("pem"),
        identity.public_key_pem()
    );
    let join_body: Value = serde_json::from_str(&requests[1].2).expect("join body json");
    assert_eq!(join_body["challenge_id"], "ch-1");
    assert_eq!(join_body["label"], "burgus/myproj");

    let state = ChatState::open(&chat_dir).expect("state");
    state
        .join_room(&receipt.room_id, receipt.seat, &receipt.label)
        .expect("record join");
    let room = state.room(ROOM).expect("room row").expect("stored");
    assert_eq!(room.seat, 2);
    assert_eq!(room.label, "burgus/myproj");
    assert!(!room.joined_at.is_empty());
}

#[test]
fn join_maps_contract_error_codes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let identity = identity_with_seed(&chat_dir(tmp.path()), 9);
    for (code, status, needle) in [
        ("room_not_found", 404u16, "not found"),
        ("seat_full", 403, "full"),
        ("label_taken", 409, "already taken"),
    ] {
        let failure = code.to_string();
        let (server, rx) = stub_server(2, move |_line, path, _body| {
            if path.ends_with("/challenge") {
                challenge_ok()
            } else {
                error_json(status, &failure)
            }
        });
        drop(rx);
        let error = join_room(&server, ROOM, &identity, "seat-a").expect_err("mapped error");
        assert!(
            error.to_string().contains(needle),
            "{code} should mention '{needle}': {error}"
        );
    }
}

#[test]
fn join_rejects_non_canonical_room_ids() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let identity = identity_with_seed(&chat_dir(tmp.path()), 3);
    for bad in ["nope", "abcde-fghjk-mnpqr-stvwz-2345u", "abcde-fghjk"] {
        assert!(
            join_room("http://127.0.0.1:1", bad, &identity, "x").is_err(),
            "'{bad}' must be rejected"
        );
    }
}

#[test]
fn default_label_uses_short_host_and_project_dir() {
    let label = default_label(Path::new("/Users/ianzepp/work/myproj"));
    let (host, project) = label.split_once('/').expect("host/project shape");
    assert_eq!(project, "myproj");
    assert!(
        !host.is_empty() && !host.contains('.'),
        "short host: {label}"
    );
}
