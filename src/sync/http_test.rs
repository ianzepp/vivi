use serde_json::json;

use super::*;
use crate::sync::SendFilter;

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
    for status in [401, 403, 404, 500, 503] {
        let stopped = classify_append(status, &error_body("x", "y"));
        assert!(
            matches!(stopped, Err(AppendFailure::Stop(_))),
            "{status}: {stopped:?}"
        );
    }
    let limited = classify_append(429, &error_body("x", "y"));
    assert!(
        matches!(limited, Err(AppendFailure::RateLimited(_))),
        "{limited:?}"
    );
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
        send: SendFilter::All,
        mailspace_id: "3f9a1c7e-5b2d-4e8f-9a01-6c4d7e8f9a0b".into(),
    }
}

#[test]
fn project_base_url_joins_the_server_and_the_project_path() {
    assert_eq!(
        project_base_url(&settings_for("https://sync.example")),
        "https://sync.example/v1/projects/abcde-fghjk"
    );
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
    let challenge = crate::b64::encode(&[7u8; 32]);
    match (method, path.rsplit('/').next().unwrap_or_default()) {
        ("POST", "challenge") => (
            200,
            format!(r#"{{"challenge_id":"c1","challenge":"{challenge}","expires_in":60}}"#),
        ),
        ("POST", "token") => (
            200,
            r#"{"token":"tok-1","token_expires_at":"later"}"#.into(),
        ),
        ("GET", "directory") => (
            200,
            r#"{"members":[
                {"name":"alice","fingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","mailspace_id":"3f9a1c7e-5b2d-4e8f-9a01-6c4d7e8f9a0b","extra":1},
                {"name":"old","fingerprint":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","mailspace_id":null},
                {"name":"older","fingerprint":"cc"}
            ],"project":"x"}"#
                .into(),
        ),
        ("POST", "identities") => (
            201,
            r#"{"name":"alice","token":"t","token_expires_at":"later","fingerprint":"f","mailspace_id":"3f9a1c7e-5b2d-4e8f-9a01-6c4d7e8f9a0b","extra":true}"#
                .into(),
        ),
        ("POST", "projects") => (201, r#"{"project_id":"prj-123","extra":1}"#.into()),
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
    let identity = SyncIdentity::ensure(dir.path()).unwrap();
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
fn transport_errors_name_the_project_server() {
    // A port nothing listens on: bind, note it, drop.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let url = format!("http://127.0.0.1:{port}/x");

    let error = request("GET", &url, None, &[]).unwrap_err().to_string();

    assert!(error.contains("project server unreachable"), "{error}");
}

#[test]
fn join_registers_the_mailspace_id_and_tolerates_extra_response_fields() {
    let (port, listener) = serve(2, project_server);
    let settings = settings_for(&format!("http://127.0.0.1:{port}"));
    let dir = tempfile::tempdir().unwrap();
    let identity = SyncIdentity::ensure(dir.path()).unwrap();

    let receipt = join(&settings, &identity).unwrap();

    assert_eq!(receipt.name, "alice");
    let seen = listener.join().unwrap();
    assert_eq!(seen[1].method, "POST");
    assert_eq!(seen[1].path, "/v1/projects/abcde-fghjk/identities");
    let body: Value = serde_json::from_slice(&seen[1].body).unwrap();
    assert_eq!(body["mailspace_id"], "3f9a1c7e-5b2d-4e8f-9a01-6c4d7e8f9a0b");
    assert_eq!(body["name"], "alice");
}

#[test]
fn a_taken_prefix_is_explained_at_join() {
    let body = error_body("mailspace_prefix_taken", "taken");
    let settings = settings_for("https://sync.example");
    let dir = tempfile::tempdir().unwrap();
    let identity = SyncIdentity::ensure(dir.path()).unwrap();

    let error = join_error(409, &body, &settings, &identity).to_string();

    assert!(error.contains("3f9a1c7e"), "{error}");
    assert!(error.contains("mailspace.toml"), "{error}");
    assert!(
        error.contains(".vivi/mailspace.id"),
        "the sidecar restores the id line, so both files must go: {error}"
    );
    assert!(error.contains("new prefix"), "{error}");
}

#[test]
fn a_lost_key_is_told_the_name_is_bound_and_who_can_help() {
    let body = error_body("admission_required", "not admitted");
    let settings = settings_for("https://sync.example");
    let dir = tempfile::tempdir().unwrap();
    let identity = SyncIdentity::ensure(dir.path()).unwrap();

    let error = join_error(409, &body, &settings, &identity).to_string();

    assert!(error.contains("has no way to re-key"), "{error}");
    assert!(error.contains("project creator"), "{error}");
    assert!(error.contains(&identity.wire_fingerprint()), "{error}");
}

#[test]
fn the_directory_lists_members_with_their_address_domains() {
    let (port, listener) = serve(3, project_server);
    let settings = settings_for(&format!("http://127.0.0.1:{port}"));
    let dir = tempfile::tempdir().unwrap();
    let identity = SyncIdentity::ensure(dir.path()).unwrap();
    let transport = HttpTransport::new(&settings, &identity);

    let members = transport.directory().unwrap();

    let lines: Vec<String> = members.iter().map(Member::line).collect();
    assert_eq!(
        lines,
        [
            "alice  3f9a1c7e.local  aaaaaaaaaaaaaaaa",
            "old  -  bbbbbbbbbbbbbbbb",
            "older  -  cc",
        ]
    );
    let seen = listener.join().unwrap();
    assert_eq!(seen[2].method, "GET");
    assert_eq!(seen[2].path, "/v1/projects/abcde-fghjk/directory");
    let auth = seen[2].headers.iter().find(|(k, _)| k == "authorization");
    assert_eq!(auth.map(|(_, v)| v.as_str()), Some("Bearer tok-1"));
}

#[test]
fn a_directory_without_members_is_an_empty_list() {
    assert!(parse_directory(&json!({})).unwrap().is_empty());
    assert!(parse_directory(&json!({"members": "no"})).is_err());
}

#[test]
fn create_project_posts_the_key_and_creator_without_a_bearer() {
    let (port, listener) = serve(1, project_server);
    let server = format!("http://127.0.0.1:{port}");
    let dir = tempfile::tempdir().unwrap();
    let identity = SyncIdentity::ensure(dir.path()).unwrap();

    let project_id = create_project(&server, &identity, "alice").unwrap();

    assert_eq!(project_id, "prj-123");
    let seen = listener.join().unwrap();
    assert_eq!(seen.len(), 1, "one request, no challenge or token");
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].path, "/v1/projects");
    assert!(
        seen[0]
            .headers
            .iter()
            .all(|(key, _)| key != "authorization"),
        "the creator has no bearer yet"
    );
    let body: Value = serde_json::from_slice(&seen[0].body).unwrap();
    assert_eq!(body["creator_name"], "alice");
    assert_eq!(body["public_key_pem"], identity.public_key_pem());
}

fn refused_project_server(_method: &str, _path: &str) -> (u16, String) {
    (
        403,
        error_body("forbidden", "only three projects").to_string(),
    )
}

#[test]
fn a_refused_project_creation_names_the_status_and_the_reason() {
    let (port, listener) = serve(1, refused_project_server);
    let server = format!("http://127.0.0.1:{port}");
    let dir = tempfile::tempdir().unwrap();
    let identity = SyncIdentity::ensure(dir.path()).unwrap();

    let error = create_project(&server, &identity, "alice")
        .unwrap_err()
        .to_string();

    assert!(error.contains("403"), "{error}");
    assert!(error.contains("forbidden"), "{error}");
    assert!(error.contains("only three projects"), "{error}");
    listener.join().unwrap();
}

fn challenge_reply() -> String {
    let challenge = crate::b64::encode(&[7u8; 32]);
    format!(r#"{{"challenge_id":"c1","challenge":"{challenge}","expires_in":60}}"#)
}

/// `/auth/token` answers with a token and no `token_expires_at` field.
fn bare_token_server(method: &str, path: &str) -> (u16, String) {
    match (method, path.rsplit('/').next().unwrap_or_default()) {
        ("POST", "challenge") => (200, challenge_reply()),
        ("POST", "token") => (200, r#"{"token":"t"}"#.into()),
        _ => (500, error_body("x", "y").to_string()),
    }
}

/// `/auth/token` refuses the mint: this name is not admitted.
fn refused_token_server(method: &str, path: &str) -> (u16, String) {
    match (method, path.rsplit('/').next().unwrap_or_default()) {
        ("POST", "challenge") => (200, challenge_reply()),
        ("POST", "token") => (401, error_body("unauthorized", "not admitted").to_string()),
        _ => (500, error_body("x", "y").to_string()),
    }
}

fn accepted_empty_server(_method: &str, _path: &str) -> (u16, String) {
    (202, String::new())
}

fn unauthorized_raw_server(_method: &str, _path: &str) -> (u16, String) {
    (401, error_body("unauthorized", "not admitted").to_string())
}

#[test]
fn mint_token_returns_the_token_and_the_expiry_the_server_sends() {
    let (port, listener) = serve(2, project_server);
    let settings = settings_for(&format!("http://127.0.0.1:{port}"));
    let dir = tempfile::tempdir().unwrap();
    let identity = SyncIdentity::ensure(dir.path()).unwrap();

    let minted = mint_token(&settings, &identity).unwrap();

    assert_eq!(minted.token, "tok-1");
    assert_eq!(minted.expires_at.as_deref(), Some("later"));
    let seen = listener.join().unwrap();
    assert_eq!(seen[1].path, "/v1/projects/abcde-fghjk/auth/token");
}

#[test]
fn mint_token_gives_no_expiry_when_the_field_is_absent() {
    let (port, listener) = serve(2, bare_token_server);
    let settings = settings_for(&format!("http://127.0.0.1:{port}"));
    let dir = tempfile::tempdir().unwrap();
    let identity = SyncIdentity::ensure(dir.path()).unwrap();

    let minted = mint_token(&settings, &identity).unwrap();

    assert_eq!(minted.token, "t");
    assert_eq!(minted.expires_at, None);
    listener.join().unwrap();
}

#[test]
fn a_refused_mint_still_names_the_bearer_mint_and_mint_bearer_still_works() {
    let (port, listener) = serve(2, refused_token_server);
    let settings = settings_for(&format!("http://127.0.0.1:{port}"));
    let dir = tempfile::tempdir().unwrap();
    let identity = SyncIdentity::ensure(dir.path()).unwrap();

    let error = mint_bearer(&settings, &identity).unwrap_err().to_string();

    assert!(error.contains("refused the bearer mint"), "{error}");
    listener.join().unwrap();

    let (port, listener) = serve(2, project_server);
    let settings = settings_for(&format!("http://127.0.0.1:{port}"));

    let token = mint_bearer(&settings, &identity).unwrap();

    assert_eq!(token, "tok-1");
    listener.join().unwrap();
}

#[test]
fn a_202_with_an_empty_body_is_a_fine_raw_answer() {
    let (port, listener) = serve(1, accepted_empty_server);
    let url = format!("http://127.0.0.1:{port}/v1/projects/abcde-fghjk/mcp");

    let raw = send_raw(
        "POST",
        &url,
        &[("Authorization", "Bearer tok".to_string())],
        None,
    )
    .unwrap();

    assert_eq!(raw.status, 202);
    assert!(raw.body.is_empty(), "no error, no bytes");
    listener.join().unwrap();
}

#[test]
fn a_401_json_body_comes_back_as_bytes_with_its_status() {
    let (port, listener) = serve(1, unauthorized_raw_server);
    let url = format!("http://127.0.0.1:{port}/v1/projects/abcde-fghjk/mcp");

    let raw = send_raw("POST", &url, &[], None).unwrap();

    assert_eq!(raw.status, 401);
    assert_eq!(raw.content_type.as_deref(), Some("application/json"));
    let body: Value = serde_json::from_slice(&raw.body).unwrap();
    assert_eq!(body["error"]["code"], "unauthorized");
    listener.join().unwrap();
}

#[test]
fn send_raw_sends_exactly_the_bytes_and_headers_given() {
    let (port, listener) = serve(1, accepted_empty_server);
    let url = format!("http://127.0.0.1:{port}/v1/projects/abcde-fghjk/mcp");
    let bytes = b"{\"jsonrpc\":\"2.0\",\"id\":1} \xff\x00".to_vec();

    let raw = send_raw(
        "POST",
        &url,
        &[
            ("Authorization", "Bearer tok-9".to_string()),
            ("Content-Type", "application/vnd.vivi.mcp+json".to_string()),
        ],
        Some(bytes.clone()),
    )
    .unwrap();

    assert_eq!(raw.status, 202);
    let seen = listener.join().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].path, "/v1/projects/abcde-fghjk/mcp");
    assert_eq!(seen[0].body, bytes, "the request body is the exact bytes");
    let get = |name: &str| {
        seen[0]
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(get("authorization"), Some("Bearer tok-9"));
    assert_eq!(
        get("content-type"),
        Some("application/vnd.vivi.mcp+json"),
        "the caller sets Content-Type; send_raw adds none"
    );
}
