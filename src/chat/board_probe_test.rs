//! Board badge probe tests against a std-lib TCP HTTP stub (no new
//! dev-dependencies).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;

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

/// Serve `count` requests with `handler`, returning the bound base URL.
fn stub_server(
    count: usize,
    handler: impl Fn(&str, &str, &str) -> (u16, String) + Send + Sync + 'static,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let addr = listener.local_addr().expect("stub addr");
    let handler = std::sync::Arc::new(handler);
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
            let _method = parts.next().unwrap_or_default().to_string();
            let path = parts.next().unwrap_or_default().to_string();
            let content_length = lines
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
                .map(|(_, v)| v.trim().parse::<usize>().unwrap_or(0))
                .unwrap_or(0);
            let body = pad_body(&mut stream, body, content_length);
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
    format!("http://{addr}")
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

/// Stub handler covering the probe's three requests: challenge, token,
/// poll. The poll page holds two unread messages.
fn probe_handler(_line: &str, path: &str, _body: &str) -> (u16, String) {
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
        (200, json!({ "token": "stub-token" }).to_string())
    } else {
        poll_page(2)
    }
}

fn poll_page(count: usize) -> (u16, String) {
    (
        200,
        json!({
            "messages": (0..count)
                .map(|n| {
                    json!({
                        "message_id": format!("m-{}", 6 + n),
                        "cursor": 6 + n,
                        "from": "seat-2",
                        "fingerprint": "0123456789abcdef",
                        "subject": "hello",
                        "body": if count > 1 { "latest body" } else { "old news" },
                        "created_at": "2026-10-09T18:00:00Z",
                    })
                })
                .collect::<Vec<_>>(),
            "next_cursor": 5 + count,
        })
        .to_string(),
    )
}

/// Write a mailspace with a `[chat]` table and return its temp root.
fn mailspace_with_chat(server: &str, notify: Option<bool>) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join(".vivi");
    std::fs::create_dir_all(&dir).expect("vivi dir");
    let notify_line = notify
        .map(|n| format!("notify = {n}\n"))
        .unwrap_or_default();
    std::fs::write(
        dir.join("mailspace.toml"),
        format!("name = \"probe\"\n\n[chat]\nserver = \"{server}\"\n{notify_line}"),
    )
    .expect("mailspace.toml");
    tmp
}

/// Seed one joined room with read cursor 5, returning the chat dir.
fn joined_room(root: &Path) -> std::path::PathBuf {
    let dir = chat_dir(&root.join(".vivi"));
    let state = ChatState::open(&dir).expect("state");
    state.join_room(ROOM, 1, "burgus.probe").expect("join");
    state.set_cursor(ROOM, "5").expect("cursor");
    dir
}

#[test]
fn stub_up_reports_unread_without_advancing_cursor() {
    let server = stub_server(3, probe_handler);
    let tmp = mailspace_with_chat(&server, None);
    let dir = joined_room(tmp.path());

    let summary = unread_summary(tmp.path()).expect("badge line");

    assert_eq!(
        summary,
        format!("chat: 2 unread in {ROOM} (last: \"latest body\")")
    );
    let state = ChatState::open(&dir).expect("state reopen");
    assert_eq!(
        state.cursor(ROOM).expect("cursor").as_deref(),
        Some("5"),
        "probe must not advance the read cursor"
    );
}

#[test]
fn stub_up_read_room_reports_no_new_messages() {
    let server = stub_server(3, |_line, path, _body| {
        if path.ends_with("/challenge") {
            probe_handler("", path, "")
        } else if path.ends_with("/auth/token") {
            probe_handler("", path, "")
        } else {
            poll_page(0)
        }
    });
    let tmp = mailspace_with_chat(&server, None);
    joined_room(tmp.path());

    let summary = unread_summary(tmp.path()).expect("badge line");

    assert_eq!(summary, "chat: no new messages");
    let state = ChatState::open(&chat_dir(&tmp.path().join(".vivi"))).expect("state");
    assert_eq!(
        state.cursor(ROOM).expect("cursor").as_deref(),
        Some("5"),
        "probe must not advance the read cursor"
    );
}

#[test]
fn stub_down_is_silent() {
    // Bind then drop a listener to get a port with nothing listening.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    let tmp = mailspace_with_chat(&format!("http://{addr}"), None);
    joined_room(tmp.path());
    assert_eq!(unread_summary(tmp.path()), None);
}

#[test]
fn no_rooms_is_silent_without_network() {
    let tmp = mailspace_with_chat("http://127.0.0.1:1", None);
    let _state = ChatState::open(&chat_dir(&tmp.path().join(".vivi"))).expect("state");
    assert_eq!(unread_summary(tmp.path()), None);
    // Without any state file the probe must not create chat state either.
    let bare = mailspace_with_chat("http://127.0.0.1:1", None);
    assert_eq!(unread_summary(bare.path()), None);
    assert!(
        !chat_dir(&bare.path().join(".vivi")).exists(),
        "board probe must not mint the chat dir"
    );
}

#[test]
fn notify_false_is_silent_even_with_a_live_server() {
    let server = stub_server(3, probe_handler);
    let tmp = mailspace_with_chat(&server, Some(false));
    joined_room(tmp.path());
    assert_eq!(
        unread_summary(tmp.path()),
        None,
        "notify = false must gate the probe before any network"
    );
}

#[test]
fn badge_fragment_appends_only_when_present() {
    assert_eq!(
        badge_fragment(Some("chat: 1 unread in r")),
        "chat: 1 unread in r\n"
    );
    assert_eq!(badge_fragment(None), "");
    assert_eq!(badge_fragment(Some("")), "");
}

#[test]
fn snippet_flattens_and_truncates() {
    assert_eq!(snippet("one\ntwo   three"), "one two three");
    let long = "x".repeat(60);
    let cut = snippet(&long);
    assert_eq!(cut.chars().count(), SNIPPET_CHARS + 3);
    assert!(cut.ends_with("..."));
}
