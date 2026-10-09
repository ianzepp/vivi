//! Watch tests against a std-lib TCP HTTP stub (no new dev-dependencies).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::mpsc;

use super::*;
use crate::chat::state::ChatState;
use crate::chat::{b64, chat_dir, ensure_chat_dir};
use serde_json::json;

const ROOM: &str = "abcde-fghjk-mnpqr-stvwz-23456";
const CHALLENGE: [u8; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31,
];

type Request = (String, String, String);

fn stub_server(
    count: usize,
    handler: impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static,
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
            let Some(head) = read_request(&mut stream) else {
                continue;
            };
            let content_length = head
                .lines()
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
                .map(|(_, v)| v.trim().parse::<usize>().unwrap_or(0))
                .unwrap_or(0);
            let mut body = String::new();
            let mut chunk = [0u8; 4096];
            while body.len() < content_length {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                body.push_str(&String::from_utf8_lossy(&chunk[..n]));
            }
            let mut lines = head.lines();
            let request_line = lines.next().unwrap_or_default().to_string();
            let mut parts = request_line.split(' ');
            let method = parts.next().unwrap_or_default().to_string();
            let path = parts.next().unwrap_or_default().to_string();
            let _ = tx.send((method, path.clone(), body));
            let (status, payload) = handler(&request_line, &path);
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

fn read_request(stream: &mut std::net::TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        stream.read(&mut byte).ok()?;
        buf.push(byte[0]);
    }
    buf.truncate(buf.len() - 4);
    Some(String::from_utf8_lossy(&buf).into_owned())
}

fn identity_with_seed(dir: &Path, seed: u8) -> crate::chat::identity::ChatIdentity {
    ensure_chat_dir(dir).expect("chat dir");
    std::fs::write(dir.join("identity.ed25519"), [seed; 32]).expect("seed file");
    crate::chat::identity::ChatIdentity::ensure(dir).expect("load seeded identity")
}

fn handler_flow(token: &str) -> impl Fn(&str, &str) -> (u16, String) + '_ {
    move |_line, path| {
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
            (200, json!({ "token": token }).to_string())
        } else {
            (
                200,
                json!({
                    "messages": [{
                        "message_id": "m-42",
                        "cursor": 9,
                        "from": "far-side/vivi",
                        "fingerprint": "0123456789abcdef",
                        "to": null,
                        "subject": "watch",
                        "body": "live body",
                        "created_at": "2026-10-09T19:00:00Z",
                    }],
                    "next_cursor": 9,
                })
                .to_string(),
            )
        }
    }
}

#[test]
fn watch_cycle_prints_arrival_and_once_exits_after_one_cycle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = chat_dir(tmp.path());
    let identity = identity_with_seed(&dir, 7);
    let state = ChatState::open(&dir).expect("state");
    state.join_room(ROOM, 1, "burgus/vivi").expect("joined");
    let rooms = state.rooms().expect("rooms");
    assert_eq!(rooms.len(), 1);

    // 6 requests: 3 for the once-run (challenge, token, poll) + 3 for the
    // direct cycle call below.
    let (server, rx) = stub_server(6, handler_flow("tok-w"));
    run(&server, &state, &identity, &rooms, true).expect("watch --once exits");

    let lines = cycle(&server, &state, &identity, &rooms[0], POLL_WAIT_SECS)
        .expect("cycle polls and renders");
    let requests: Vec<Request> = rx.try_iter().collect();
    assert_eq!(requests.len(), 6, "two full poll cycles");
    let poll_paths: Vec<&String> = requests
        .iter()
        .map(|r| &r.1)
        .filter(|p| p.contains("messages"))
        .collect();
    assert_eq!(poll_paths.len(), 2);
    for path in &poll_paths {
        assert!(
            path.contains(&format!("&wait={POLL_WAIT_SECS}")),
            "poll must long-poll with the wait param: {path}"
        );
    }
    assert!(
        poll_paths[0].contains("since=0"),
        "first cycle starts at the stored cursor zero"
    );
    assert!(
        poll_paths[1].contains("since=9"),
        "second cycle resumes from the advanced cursor"
    );
    assert_eq!(lines.len(), 1, "one arrival printed");
    for needle in ["far-side/vivi", "live body", "watch"] {
        assert!(lines[0].contains(needle), "render should contain {needle}");
    }
    assert_eq!(
        state.cursor(ROOM).expect("cursor"),
        Some("9".into()),
        "watch advances the cursor so nothing replays"
    );
}

#[test]
fn resolve_rooms_rejects_unknown_room() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = ChatState::open(tmp.path()).expect("state");
    let error = resolve_rooms(&state, Some(ROOM)).expect_err("not joined");
    assert!(error.to_string().contains("not joined"));
    let error = resolve_rooms(&state, None).expect_err("no rooms");
    assert!(error.to_string().contains("no rooms joined"));
}
