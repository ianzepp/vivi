use serde_json::{Value, json};

use super::*;

fn challenge_reply() -> String {
    let challenge = crate::b64::encode(&[7u8; 32]);
    format!(r#"{{"challenge_id":"c1","challenge":"{challenge}","expires_in":60}}"#)
}

fn token_reply() -> String {
    r#"{"token":"tok-1","token_expires_at":"later"}"#.into()
}

/// The happy key surface: challenge, token, mint 201, list 200, revoke 204.
fn key_server(method: &str, path: &str) -> (u16, String) {
    match (method, path.rsplit('/').next().unwrap_or_default()) {
        ("POST", "challenge") => (200, challenge_reply()),
        ("POST", "token") => (200, token_reply()),
        ("POST", "keys") => (
            201,
            r#"{"key_id":"key_1","key":"vivi_secret","label":"ci","created_by":"alice","created_at":"2026-01-01T00:00:00Z","extra":1}"#
                .into(),
        ),
        ("GET", "keys") => (200, json!({ "keys": [] }).to_string()),
        ("DELETE", "key_1") => (204, String::new()),
        _ => (500, json!({ "error": { "code": "x", "message": "y" } }).to_string()),
    }
}

/// The staging list shape: revoked keys stay, live ones have no `revoked_at`.
fn list_object_server(method: &str, path: &str) -> (u16, String) {
    if (method, path.rsplit('/').next()) == ("GET", Some("keys")) {
        let body = json!({ "keys": [
            {"key_id": "key_1", "label": "ci", "created_by": "alice", "created_at": "t1", "extra": 1},
            {"key_id": "key_2", "label": "old", "created_by": "alice", "created_at": "t2", "revoked_at": "2026-01-02T00:00:00Z"},
            {"key_id": "key_3", "revoked_at": null},
        ]});
        return (200, body.to_string());
    }
    key_server(method, path)
}

/// A bare JSON array is accepted too.
fn list_array_server(method: &str, path: &str) -> (u16, String) {
    if (method, path.rsplit('/').next()) == ("GET", Some("keys")) {
        let body = json!([
            {"key_id": "key_9", "label": "deploy", "created_by": "bob", "created_at": "t9"},
        ]);
        return (200, body.to_string());
    }
    key_server(method, path)
}

/// The key routes refuse the bearer: not this mailspace's identity.
fn unauthorized_server(method: &str, path: &str) -> (u16, String) {
    match (method, path.rsplit('/').next().unwrap_or_default()) {
        ("POST", "challenge") => (200, challenge_reply()),
        ("POST", "token") => (200, token_reply()),
        _ => (
            401,
            json!({ "error": { "code": "unauthorized", "message": "no" } }).to_string(),
        ),
    }
}

/// A revoke of an id that never existed.
fn missing_key_server(method: &str, path: &str) -> (u16, String) {
    match (method, path.rsplit('/').next().unwrap_or_default()) {
        ("POST", "challenge") => (200, challenge_reply()),
        ("POST", "token") => (200, token_reply()),
        _ => (
            404,
            json!({ "error": { "code": "key_not_found", "message": "no such key" } }).to_string(),
        ),
    }
}

/// The project is over its live-key budget.
fn limited_server(method: &str, path: &str) -> (u16, String) {
    match (method, path.rsplit('/').next().unwrap_or_default()) {
        ("POST", "challenge") => (200, challenge_reply()),
        ("POST", "token") => (200, token_reply()),
        _ => (
            409,
            json!({ "error": { "code": "key_limit", "message": "at most 32 live keys" } })
                .to_string(),
        ),
    }
}

fn settings_for(server: &str) -> SyncSettings {
    SyncSettings {
        server: server.into(),
        project: "abcde-fghjk".into(),
        name: "alice".into(),
        mailspace_id: "3f9a1c7e-5b2d-4e8f-9a01-6c4d7e8f9a0b".into(),
    }
}

fn identity() -> SyncIdentity {
    let dir = tempfile::tempdir().unwrap();
    SyncIdentity::ensure(dir.path()).unwrap()
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

fn header<'a>(seen: &'a Seen, name: &str) -> Option<&'a str> {
    seen.headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

#[test]
fn a_label_of_zero_or_65_characters_is_refused_before_any_request() {
    let (port, listener) = serve(0, key_server);
    let settings = settings_for(&format!("http://127.0.0.1:{port}"));
    let identity = identity();

    let long = "x".repeat(65);
    for label in ["", "   ", long.as_str()] {
        let error = mint_key(&settings, &identity, label)
            .unwrap_err()
            .to_string();
        assert!(error.contains("label"), "{label:?}: {error}");
        assert!(error.contains("1 to 64"), "{label:?}: {error}");
        assert!(
            !error.contains("unreachable"),
            "{label:?}: the request must not be made: {error}"
        );
    }
    listener.join().unwrap();
}

#[test]
fn mint_posts_the_trimmed_label_with_the_minted_identity_token() {
    let (port, listener) = serve(3, key_server);
    let settings = settings_for(&format!("http://127.0.0.1:{port}"));
    let identity = identity();

    let minted = mint_key(&settings, &identity, "  ci  ").unwrap();

    assert_eq!(minted.key, "vivi_secret");
    assert_eq!(minted.record.key_id, "key_1");
    assert_eq!(minted.record.label, "ci");
    assert_eq!(minted.record.created_by, "alice");
    assert_eq!(minted.record.created_at, "2026-01-01T00:00:00Z");
    let seen = listener.join().unwrap();
    assert_eq!(seen[2].method, "POST");
    assert_eq!(seen[2].path, "/v1/projects/abcde-fghjk/keys");
    assert_eq!(
        header(&seen[2], "authorization"),
        Some("Bearer tok-1"),
        "the Authorization header is the minted identity token"
    );
    let body: Value = serde_json::from_slice(&seen[2].body).unwrap();
    assert_eq!(body, json!({ "label": "ci" }), "sent trimmed, and only it");
}

#[test]
fn list_accepts_the_staging_object_drops_revoked_and_ignores_unknown_fields() {
    let (port, listener) = serve(3, list_object_server);
    let settings = settings_for(&format!("http://127.0.0.1:{port}"));
    let identity = identity();

    let records = list_keys(&settings, &identity).unwrap();

    assert_eq!(
        records,
        vec![
            ApiKeyRecord {
                key_id: "key_1".into(),
                label: "ci".into(),
                created_by: "alice".into(),
                created_at: "t1".into(),
            },
            ApiKeyRecord {
                key_id: "key_3".into(),
                label: String::new(),
                created_by: String::new(),
                created_at: String::new(),
            },
        ],
        "the revoked key is dropped; missing text fields become empty strings"
    );
    listener.join().unwrap();
}

#[test]
fn list_accepts_a_bare_json_array() {
    let (port, listener) = serve(3, list_array_server);
    let settings = settings_for(&format!("http://127.0.0.1:{port}"));
    let identity = identity();

    let records = list_keys(&settings, &identity).unwrap();

    assert_eq!(
        records,
        vec![ApiKeyRecord {
            key_id: "key_9".into(),
            label: "deploy".into(),
            created_by: "bob".into(),
            created_at: "t9".into(),
        }],
    );
    listener.join().unwrap();
}

#[test]
fn a_204_with_an_empty_body_revokes() {
    let (port, listener) = serve(3, key_server);
    let settings = settings_for(&format!("http://127.0.0.1:{port}"));
    let identity = identity();

    revoke_key(&settings, &identity, "key_1").unwrap();

    let seen = listener.join().unwrap();
    assert_eq!(seen[2].method, "DELETE");
    assert_eq!(seen[2].path, "/v1/projects/abcde-fghjk/keys/key_1");
    assert_eq!(header(&seen[2], "authorization"), Some("Bearer tok-1"));
    assert!(seen[2].body.is_empty());
}

#[test]
fn a_404_key_not_found_names_the_id() {
    let (port, listener) = serve(3, missing_key_server);
    let settings = settings_for(&format!("http://127.0.0.1:{port}"));
    let identity = identity();

    let error = revoke_key(&settings, &identity, "key_77")
        .unwrap_err()
        .to_string();

    assert!(error.contains("key_77"), "{error}");
    assert!(error.contains("unknown"), "{error}");
    assert!(error.contains("not minted by this member"), "{error}");
    listener.join().unwrap();
}

#[test]
fn an_id_that_would_change_the_route_is_refused_before_any_request() {
    let (port, listener) = serve(0, key_server);
    let settings = settings_for(&format!("http://127.0.0.1:{port}"));
    let identity = identity();

    for id in ["", "key/1", "key?1", "key 1"] {
        let error = revoke_key(&settings, &identity, id)
            .unwrap_err()
            .to_string();
        assert!(error.contains("key id"), "{id:?}: {error}");
        assert!(
            !error.contains("unreachable"),
            "{id:?}: the request must not be made: {error}"
        );
    }
    listener.join().unwrap();
}

#[test]
fn a_401_says_the_key_routes_need_the_identity_and_never_a_token() {
    let (port, listener) = serve(3, unauthorized_server);
    let settings = settings_for(&format!("http://127.0.0.1:{port}"));
    let identity = identity();

    let error = list_keys(&settings, &identity).unwrap_err().to_string();

    assert!(error.contains("identity"), "{error}");
    assert!(error.contains("vivi sync join"), "{error}");
    assert!(!error.contains("tok-1"), "no token in the message: {error}");
    listener.join().unwrap();
}

#[test]
fn other_statuses_carry_the_server_status_code_and_message() {
    let (port, listener) = serve(3, limited_server);
    let settings = settings_for(&format!("http://127.0.0.1:{port}"));
    let identity = identity();

    let error = mint_key(&settings, &identity, "ci")
        .unwrap_err()
        .to_string();

    assert!(error.contains("HTTP 409 key_limit"), "{error}");
    assert!(error.contains("at most 32 live keys"), "{error}");
    listener.join().unwrap();
}
