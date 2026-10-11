use serde_json::json;

use super::*;

fn error_body(code: &str, message: &str) -> Value {
    json!({ "error": { "code": code, "message": message } })
}

#[test]
fn a_fresh_letter_and_a_replay_are_both_accepted() {
    let fresh = json!({"message_id": "msg_a", "sequence": 7, "content_sha256": "ab"});
    let appended = classify_append(201, &fresh).unwrap();
    assert_eq!(appended.sequence, 7);
    assert!(!appended.duplicate);
    assert_eq!(appended.content_sha256.as_deref(), Some("ab"));

    let replay = json!({"message_id": "msg_a", "sequence": 7, "duplicate": true});
    assert!(classify_append(200, &replay).unwrap().duplicate);

    let bare_200 = json!({"message_id": "msg_a", "sequence": 7});
    assert!(
        classify_append(200, &bare_200).unwrap().duplicate,
        "a 200 is a replay by contract"
    );
}

#[test]
fn per_letter_refusals_continue_the_pass_and_the_rest_stop_it() {
    for status in [400, 409, 413] {
        let refused = classify_append(status, &error_body("conflict", "id names other bytes"));
        match refused {
            Err(AppendFailure::Refused(reason)) => {
                assert!(reason.contains(&status.to_string()), "{reason}");
                assert!(reason.contains("id names other bytes"), "{reason}");
            }
            other => panic!("{status}: {other:?}"),
        }
    }
    for status in [401, 403, 404, 429, 500, 503] {
        let stopped = classify_append(status, &error_body("x", "y"));
        assert!(
            matches!(stopped, Err(AppendFailure::Stop(_))),
            "{status}: {stopped:?}"
        );
    }
}

#[test]
fn an_accepted_response_without_a_sequence_stops_the_pass() {
    let junk = json!({"ok": true});

    assert!(matches!(
        classify_append(201, &junk),
        Err(AppendFailure::Stop(_))
    ));
}

fn settings_for(server: &str) -> SyncSettings {
    SyncSettings {
        server: server.into(),
        project: "abcde-fghjk".into(),
        name: "alice".into(),
        recipient: None,
        names: std::collections::BTreeMap::new(),
    }
}

fn header<'a>(headers: &'a [(&'static str, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[test]
fn the_append_request_is_a_put_of_the_exact_bytes_with_envelope_headers() {
    let bytes = b"From: a\r\n\r\nbody with no trailing newline \xff\x00";
    let settings = settings_for("https://sync.example");
    let envelope = Envelope {
        raw_eml: bytes,
        to: "bob",
        kind: "need",
    };

    let put = append_request(&settings, "tok", "msg_0123456789abcdef01234567", &envelope);

    assert_eq!(
        put.url,
        "https://sync.example/v1/projects/abcde-fghjk/items/msg_0123456789abcdef01234567"
    );
    assert_eq!(put.body, bytes, "no base64, no JSON");
    assert_eq!(header(&put.headers, "Authorization"), Some("Bearer tok"));
    assert_eq!(header(&put.headers, "Content-Type"), Some("message/rfc822"));
    assert_eq!(header(&put.headers, "X-Vivi-To"), Some("bob"));
    assert_eq!(header(&put.headers, "X-Vivi-Kind"), Some("need"));
    assert!(header(&put.headers, "Message-ID").is_none());
}

#[test]
fn an_empty_recipient_omits_the_to_header() {
    let settings = settings_for("https://sync.example");
    let envelope = Envelope {
        raw_eml: b"x",
        to: "",
        kind: "mail",
    };

    let put = append_request(&settings, "tok", "msg_0123456789abcdef01234567", &envelope);

    assert!(header(&put.headers, "X-Vivi-To").is_none());
    assert_eq!(header(&put.headers, "X-Vivi-Kind"), Some("mail"));
}

/// One request as a listener saw it.
struct Seen {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Serve `connections` requests on a loopback port, answering each with
/// `answer(method, path)`, and hand back what was seen.
fn serve(
    connections: usize,
    answer: fn(&str, &str) -> (u16, String),
) -> (u16, std::thread::JoinHandle<Vec<Seen>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for _ in 0..connections {
            let (mut stream, _) = listener.accept().unwrap();
            let mut raw = Vec::new();
            let mut chunk = [0u8; 4096];
            let (head_end, length) = loop {
                let n = stream.read(&mut chunk).unwrap();
                raw.extend_from_slice(&chunk[..n]);
                if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&raw[..end]).to_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .map_or(0, |v| v.trim().parse::<usize>().unwrap());
                    break (end + 4, length);
                }
            };
            while raw.len() < head_end + length {
                let n = stream.read(&mut chunk).unwrap();
                raw.extend_from_slice(&chunk[..n]);
            }
            let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
            let mut lines = head.lines();
            let mut first = lines.next().unwrap().split(' ');
            let (method, path) = (first.next().unwrap(), first.next().unwrap());
            let headers = lines
                .filter_map(|line| line.split_once(':'))
                .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
                .collect();
            let (status, reply) = answer(method, path);
            seen.push(Seen {
                method: method.to_string(),
                path: path.to_string(),
                headers,
                body: raw[head_end..head_end + length].to_vec(),
            });
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
        seen
    });
    (port, handle)
}

fn project_server(method: &str, path: &str) -> (u16, String) {
    let challenge = crate::chat::b64::encode(&[7u8; 32]);
    match (method, path.rsplit('/').next().unwrap_or_default()) {
        ("POST", "challenge") => (
            200,
            format!(r#"{{"challenge_id":"c1","challenge":"{challenge}","expires_in":60}}"#),
        ),
        ("POST", "token") => (
            200,
            r#"{"token":"tok-1","token_expires_at":"later"}"#.into(),
        ),
        _ => (
            201,
            format!(
                r#"{{"message_id":"msg_0123456789abcdef01234567","sequence":3,"content_sha256":"{}"}}"#,
                crate::storage::sha256_hex(b"letter bytes \r\n\xff")
            ),
        ),
    }
}

#[test]
fn append_over_the_wire_is_a_raw_put_with_a_minted_bearer() {
    let (port, listener) = serve(3, project_server);
    let settings = settings_for(&format!("http://127.0.0.1:{port}"));
    let dir = tempfile::tempdir().unwrap();
    let identity = ChatIdentity::ensure(dir.path()).unwrap();
    let transport = HttpTransport::new(&settings, &identity);
    let bytes = b"letter bytes \r\n\xff";

    let appended = transport
        .append(
            "msg_0123456789abcdef01234567",
            &Envelope {
                raw_eml: bytes,
                to: "bob",
                kind: "task",
            },
        )
        .unwrap();

    assert_eq!(appended.sequence, 3);
    assert_eq!(
        appended.content_sha256.as_deref(),
        Some(crate::storage::sha256_hex(bytes).as_str())
    );
    let seen = listener.join().unwrap();
    let paths: Vec<(&str, &str)> = seen
        .iter()
        .map(|s| (s.method.as_str(), s.path.as_str()))
        .collect();
    assert_eq!(
        paths,
        [
            ("POST", "/v1/projects/abcde-fghjk/identities/challenge"),
            ("POST", "/v1/projects/abcde-fghjk/auth/token"),
            (
                "PUT",
                "/v1/projects/abcde-fghjk/items/msg_0123456789abcdef01234567"
            ),
        ]
    );
    let put = &seen[2];
    assert_eq!(put.body, bytes, "the request body is the letter");
    let get = |name: &str| {
        put.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(get("authorization"), Some("Bearer tok-1"));
    assert_eq!(get("content-type"), Some("message/rfc822"));
    assert_eq!(get("x-vivi-to"), Some("bob"));
    assert_eq!(get("x-vivi-kind"), Some("task"));
    assert_eq!(get("message-id"), None, "the id is the path segment");
}

#[test]
fn transport_errors_name_the_project_server_not_the_chat_server() {
    // A port nothing listens on: bind, note it, drop.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let url = format!("http://127.0.0.1:{port}/x");

    let error = request("GET", &url, None, &[]).unwrap_err().to_string();

    assert!(error.contains("project server"), "{error}");
    assert!(!error.contains("chat server"), "{error}");
}

#[test]
fn rebrand_leaves_other_errors_alone() {
    let plain = rebrand(VivariumError::Other("failed to build runtime".into()));
    assert_eq!(plain.to_string(), "failed to build runtime");

    let timed = rebrand(VivariumError::Message(
        "chat server timed out after 15000ms".into(),
    ));
    assert_eq!(
        timed.to_string(),
        "message error: project server timed out after 15000ms"
    );
}
