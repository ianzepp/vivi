//! In-process tests of the project HTTP API: identity proof-of-possession,
//! token renewal, the directory, and the letter wire (raw `.eml` bytes in,
//! the same bytes out, content-addressed duplicates, scoped visibility).

use std::path::{Path, PathBuf};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::util::ServiceExt;
use vivi_server::api_router;

const STORE_KEY_SEED: u8 = 0x42;

fn store_key() -> String {
    STANDARD.encode([STORE_KEY_SEED; 32])
}

/// A fresh, empty data dir per test so file-walking assertions are exact.
fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-project-api-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn spki_pem(key: &SigningKey) -> String {
    const PREFIX: &[u8] = &[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut der = PREFIX.to_vec();
    der.extend_from_slice(key.verifying_key().as_bytes());
    format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
        STANDARD.encode(der)
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Traveling id shape: `msg_` plus 24 lowercase hex.
fn traveling(n: u64) -> String {
    format!("msg_{n:08x}{:016x}", 0)
}

/// A letter with LF line ends whose bytes are unique per `(id, body)`.
fn eml(id: &str, body: &str) -> Vec<u8> {
    format!("From: a@p.local\nTo: b@p.local\nMessage-ID: <{id}@p.local>\n\n{body}").into_bytes()
}

async fn request(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut headers = Vec::new();
    if body.is_some() {
        headers.push(("content-type".to_owned(), "application/json".to_owned()));
    }
    let bytes = body.map(|v| v.to_string().into_bytes()).unwrap_or_default();
    raw_request(app, method, uri, bytes, token, headers).await
}

/// One request with an arbitrary byte body and explicit headers; the
/// response body is JSON (or empty, as `Value::Null`).
async fn raw_request(
    app: &Router,
    method: &str,
    uri: &str,
    body: Vec<u8>,
    token: Option<&str>,
    headers: Vec<(String, String)>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    // Framework-level rejections (405) carry no body at all.
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    request(app, method, uri, body, token).await
}

/// `PUT` raw letter bytes to `/items/{path_id}` with the given extra headers
/// (envelope fields, a stray Message-ID, ...). `content-type` is always
/// `message/rfc822`, as clients send it.
async fn put_with(
    app: &Router,
    project: &str,
    token: Option<&str>,
    path_id: &str,
    raw: &[u8],
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut all = vec![("content-type".to_owned(), "message/rfc822".to_owned())];
    all.extend(headers.iter().map(|(n, v)| (n.to_string(), v.to_string())));
    raw_request(
        app,
        "PUT",
        &format!("/v1/projects/{project}/items/{path_id}"),
        raw.to_vec(),
        token,
        all,
    )
    .await
}

/// Append raw letter bytes with optional envelope headers.
async fn append(
    app: &Router,
    project: &str,
    token: &str,
    id: &str,
    raw: &[u8],
    to: Option<&str>,
    kind: Option<&str>,
) -> (StatusCode, Value) {
    let mut headers = Vec::new();
    if let Some(to) = to {
        headers.push(("x-vivi-to", to));
    }
    if let Some(kind) = kind {
        headers.push(("x-vivi-kind", kind));
    }
    put_with(app, project, Some(token), id, raw, &headers).await
}

async fn read(app: &Router, project: &str, token: &str, since: u64) -> Value {
    let (status, items) = send(
        app,
        "GET",
        &format!("/v1/projects/{project}/items?since={since}"),
        None,
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{items}");
    items
}

fn ids(items: &Value) -> Vec<String> {
    items["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["message_id"].as_str().unwrap().to_owned())
        .collect()
}

async fn challenge(app: &Router, project: &str, signing_key: &SigningKey) -> (String, Vec<u8>) {
    let (status, challenge) = send(
        app,
        "POST",
        &format!("/v1/projects/{project}/identities/challenge"),
        Some(json!({"public_key_pem": spki_pem(signing_key)})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{challenge}");
    let bytes = STANDARD
        .decode(challenge["challenge"].as_str().unwrap())
        .unwrap();
    (
        challenge["challenge_id"].as_str().unwrap().to_owned(),
        bytes,
    )
}

async fn register(
    app: &Router,
    project: &str,
    signing_key: &SigningKey,
    name: &str,
) -> (StatusCode, Value) {
    let (challenge_id, bytes) = challenge(app, project, signing_key).await;
    let signature = STANDARD.encode(signing_key.sign(&bytes).to_bytes());
    send(
        app,
        "POST",
        &format!("/v1/projects/{project}/identities"),
        Some(json!({
            "name": name,
            "public_key_pem": spki_pem(signing_key),
            "challenge_id": challenge_id,
            "signature_base64": signature,
        })),
        None,
    )
    .await
}

/// Renew a token: fetch a challenge for `challenge_key`, sign it with
/// `signing_key`, and present it for `name`.
async fn renew_with(
    app: &Router,
    project: &str,
    name: &str,
    challenge_key: &SigningKey,
    signing_key: &SigningKey,
) -> (StatusCode, Value) {
    let (challenge_id, bytes) = challenge(app, project, challenge_key).await;
    let signature = STANDARD.encode(signing_key.sign(&bytes).to_bytes());
    send(
        app,
        "POST",
        &format!("/v1/projects/{project}/auth/token"),
        Some(json!({
            "name": name,
            "challenge_id": challenge_id,
            "signature_base64": signature,
        })),
        None,
    )
    .await
}

/// A project with its creator registered.
struct Fixture {
    app: Router,
    dir: PathBuf,
    project: String,
    token: String,
}

async fn fixture(name: &str, creator_seed: u8) -> Fixture {
    let dir = fresh_dir(name);
    let app = api_router(dir.clone(), Some(store_key()), None);
    let creator_key = key(creator_seed);
    let (status, project) = send(
        &app,
        "POST",
        "/v1/projects",
        Some(json!({"public_key_pem": spki_pem(&creator_key), "creator_name": "creator"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{project}");
    let project = project["project_id"].as_str().unwrap().to_owned();
    let (status, creator) = register(&app, &project, &creator_key, "creator").await;
    assert_eq!(status, StatusCode::CREATED, "{creator}");
    let token = creator["token"].as_str().unwrap().to_owned();
    Fixture {
        app,
        dir,
        project,
        token,
    }
}

/// A governance letter: headers, blank line, vouch JSON.
fn vouch_letter(id: &str, name: &str, signing_key: &SigningKey) -> Vec<u8> {
    let body = json!({"vouch": {"name": name, "public_key_pem": spki_pem(signing_key)}});
    eml(id, &body.to_string())
}

impl Fixture {
    async fn append(
        &self,
        id: &str,
        raw: &[u8],
        to: Option<&str>,
        kind: Option<&str>,
    ) -> (StatusCode, Value) {
        append(&self.app, &self.project, &self.token, id, raw, to, kind).await
    }

    /// The creator vouches `name` for `signing_key`, then it registers;
    /// returns its token.
    async fn add_member(&self, vouch_id: &str, name: &str, signing_key: &SigningKey) -> String {
        let raw = vouch_letter(vouch_id, name, signing_key);
        let (status, vouched) = self.append(vouch_id, &raw, Some("postmaster"), None).await;
        assert_eq!(status, StatusCode::CREATED, "{vouched}");
        let (status, identity) = register(&self.app, &self.project, signing_key, name).await;
        assert_eq!(status, StatusCode::CREATED, "{identity}");
        identity["token"].as_str().unwrap().to_owned()
    }
}

#[tokio::test]
async fn project_identity_auth_and_items_wire() {
    let fx = fixture("auth-wire", 1).await;
    let (status, unadmitted) = register(&fx.app, &fx.project, &key(2), "builder").await;
    assert_eq!(status, StatusCode::CONFLICT, "{unadmitted}");
    assert_eq!(unadmitted["error"]["code"], "admission_required");

    let token = fx.add_member(&traveling(99), "builder", &key(2)).await;
    let (status, wrong_key) = register(&fx.app, &fx.project, &key(3), "builder").await;
    assert_eq!(status, StatusCode::CONFLICT, "{wrong_key}");
    assert_eq!(wrong_key["error"]["code"], "admission_required");
    let (status, guest) = register(&fx.app, &fx.project, &key(4), "guest").await;
    assert_eq!(status, StatusCode::CONFLICT, "{guest}");

    for n in 1..=2u64 {
        let id = traveling(n);
        let (status, created) = append(
            &fx.app,
            &fx.project,
            &token,
            &id,
            &eml(&id, &format!("body {n}")),
            None,
            Some("need"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
    }
    let (status, unauthorized) = send(
        &fx.app,
        "GET",
        &format!("/v1/projects/{}/items?since=0", fx.project),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{unauthorized}");

    let items = read(&fx.app, &fx.project, &token, 0).await;
    assert_eq!(items["next_cursor"], 3);
    let records = items["records"].as_array().unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["sequence"], 1);
    assert_eq!(records[0]["from"], "creator");
    assert_eq!(records[0]["to"], "postmaster");
    assert_eq!(records[0]["kind"], Value::Null);
    let first = STANDARD
        .decode(records[0]["raw_eml"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        first,
        vouch_letter(&traveling(99), "builder", &key(2)),
        "the vouch letter is served byte for byte"
    );
    assert_eq!(records[2]["from"], "builder");
    assert_eq!(records[2]["kind"], "need");
    assert_eq!(records[2]["to"], Value::Null);
}

#[tokio::test]
async fn project_directory_lists_authenticated_members_sorted_by_name() {
    let fx = fixture("directory", 21).await;
    let zulu = fx.add_member(&traveling(98), "zulu", &key(20)).await;
    assert!(!zulu.is_empty());
    let uri = format!("/v1/projects/{}/directory", fx.project);

    let (status, missing_auth) = send(&fx.app, "GET", &uri, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{missing_auth}");
    let (status, invalid_auth) = send(&fx.app, "GET", &uri, None, Some("not-a-token")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{invalid_auth}");

    let (status, directory) = send(&fx.app, "GET", &uri, None, Some(&fx.token)).await;
    assert_eq!(status, StatusCode::OK, "{directory}");
    let names: Vec<&str> = directory["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["creator", "zulu"]);
    assert_eq!(
        directory["members"][0]["fingerprint"]
            .as_str()
            .unwrap()
            .len(),
        64
    );

    let unknown_project_id = format!(
        "{}{}",
        if fx.project.starts_with('z') {
            "y"
        } else {
            "z"
        },
        &fx.project[1..]
    );
    let (status, unknown) = send(
        &fx.app,
        "GET",
        &format!("/v1/projects/{unknown_project_id}/directory"),
        None,
        Some(&fx.token),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{unknown}");
}

#[tokio::test]
async fn letter_round_trip_is_byte_exact_and_hashes_match() {
    let fx = fixture("round-trip", 31).await;
    // Non-UTF-8 bytes, CRLF line ends, NUL, and no trailing newline.
    let mut raw = b"From: a@p.local\r\nSubject: bytes\r\n\r\nbody \xff\xfe\x00\x80 end".to_vec();
    raw.extend_from_slice(b"\r\nlast line without newline");
    let expected = sha256_hex(&raw);

    let id = traveling(1);
    let (status, created) = fx.append(&id, &raw, Some("builder"), Some("task")).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["message_id"], id);
    assert_eq!(created["sequence"], 1);
    assert_eq!(created["content_sha256"], expected);
    assert!(created.get("duplicate").is_none());

    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    let record = &items["records"][0];
    assert_eq!(record["content_sha256"], expected);
    assert_eq!(record["to"], "builder");
    assert_eq!(record["kind"], "task");
    let served = STANDARD
        .decode(record["raw_eml"].as_str().unwrap())
        .unwrap();
    assert_eq!(served, raw);
    assert_eq!(sha256_hex(&served), expected);
}

#[tokio::test]
async fn idempotency_matrix() {
    let fx = fixture("idempotency", 41).await;
    let first_id = traveling(1);
    let raw = eml(&first_id, "original");
    let hash = sha256_hex(&raw);

    // Fresh: 201.
    let (status, fresh) = fx.append(&first_id, &raw, None, Some("want")).await;
    assert_eq!(status, StatusCode::CREATED, "{fresh}");
    assert_eq!(fresh["sequence"], 1);

    // Same id, same bytes: 200 duplicate, original sequence.
    let (status, same) = fx.append(&first_id, &raw, None, Some("want")).await;
    assert_eq!(status, StatusCode::OK, "{same}");
    assert_eq!(same["duplicate"], true);
    assert_eq!(same["message_id"], first_id);
    assert_eq!(same["sequence"], 1);
    assert_eq!(same["content_sha256"], hash);

    // Same id, different bytes: 409 and nothing written.
    let (status, conflict) = fx
        .append(&first_id, &eml(&first_id, "tampered"), None, Some("want"))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["error"]["code"], "conflict");

    // Same bytes under a new id: 200 duplicate naming the ORIGINAL id and
    // sequence; the new id is never stored.
    let alias = traveling(2);
    let (status, aliased) = fx.append(&alias, &raw, None, Some("want")).await;
    assert_eq!(status, StatusCode::OK, "{aliased}");
    assert_eq!(aliased["duplicate"], true);
    assert_eq!(aliased["message_id"], first_id);
    assert_eq!(aliased["sequence"], 1);

    // The log holds exactly the first letter, and the next fresh letter
    // takes the next sequence (nothing above consumed one).
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    assert_eq!(ids(&items), vec![first_id.clone()]);
    let next = traveling(3);
    let (status, next_created) = fx.append(&next, &eml(&next, "next"), None, None).await;
    assert_eq!(status, StatusCode::CREATED, "{next_created}");
    assert_eq!(next_created["sequence"], 2);
}

#[tokio::test]
async fn size_cap_and_input_validation() {
    let fx = fixture("validation", 51).await;
    let cap = 64 * 1024;

    // Exactly 64 KiB is accepted; one byte more is 413.
    let id = traveling(1);
    let mut at_cap = eml(&id, "");
    at_cap.resize(cap, b'x');
    let (status, ok) = fx.append(&id, &at_cap, None, None).await;
    assert_eq!(status, StatusCode::CREATED, "{ok}");
    let id = traveling(2);
    let mut over = eml(&id, "");
    over.resize(cap + 1, b'x');
    let (status, too_big) = fx.append(&id, &over, None, None).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{too_big}");
    assert_eq!(too_big["error"]["code"], "body_too_large");

    let letter = eml(&traveling(3), "x");
    let bad = |status: StatusCode, value: &Value| {
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(value["error"]["code"], "invalid_request");
    };

    // Empty body.
    let (status, value) = fx.append(&traveling(3), b"", None, None).await;
    bad(status, &value);

    // Bad kind (header) and bad `to` (header, including present-but-empty).
    let (status, value) = fx.append(&traveling(3), &letter, None, Some("bogus")).await;
    bad(status, &value);
    for to in ["", "two words", "a@b", &"a".repeat(65)] {
        let (status, value) = fx.append(&traveling(3), &letter, Some(to), None).await;
        bad(status, &value);
    }
    // A header value that is not ASCII.
    let non_ascii = Request::builder()
        .method("PUT")
        .uri(format!(
            "/v1/projects/{}/items/{}",
            fx.project,
            traveling(4)
        ))
        .header("authorization", format!("Bearer {}", fx.token))
        .header(
            "x-vivi-to",
            axum::http::HeaderValue::from_bytes(b"caf\xe9").unwrap(),
        )
        .body(Body::from(letter.clone()))
        .unwrap();
    let response = fx.app.clone().oneshot(non_ascii).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    bad(status, &serde_json::from_slice(&bytes).unwrap());

    // Bad path id.
    for id in ["plain-1", "msg_ABCDEF000000000000000000", "msg_0000"] {
        let (status, value) = fx.append(id, &letter, None, None).await;
        bad(status, &value);
    }

    // No token: 401.
    let (status, no_token) =
        put_with(&fx.app, &fx.project, None, &traveling(5), &letter, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{no_token}");

    // None of the rejects consumed a sequence or stored anything: the cap
    // letter is the only one stored.
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    assert_eq!(items["records"].as_array().unwrap().len(), 1);
    let (status, fine) = fx
        .append(
            &traveling(6),
            &eml(&traveling(6), "y"),
            Some("Bob.s-1_x"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{fine}");
    assert_eq!(fine["sequence"], 2);
}

#[tokio::test]
async fn kind_is_recorded_lowercase_and_mail_is_null() {
    let fx = fixture("kinds", 61).await;
    let cases = [
        (1, Some("TASK"), json!("task")),
        (2, Some("Need"), json!("need")),
        (3, Some("want"), json!("want")),
        (4, Some("mail"), Value::Null),
        (5, Some(""), Value::Null),
        (6, None, Value::Null),
    ];
    for (n, kind, _) in &cases {
        let id = traveling(*n);
        let (status, value) = fx.append(&id, &eml(&id, "k"), None, *kind).await;
        assert_eq!(status, StatusCode::CREATED, "{value}");
    }
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    for (i, (_, _, expected)) in cases.iter().enumerate() {
        assert_eq!(&items["records"][i]["kind"], expected, "case {i}");
    }
}

#[tokio::test]
async fn visibility_scopes_mail_and_filtered_items_do_not_advance_the_cursor() {
    let fx = fixture("visibility", 71).await;
    let bob = fx.add_member(&traveling(90), "bob", &key(72)).await;
    let carol = fx.add_member(&traveling(91), "carol", &key(73)).await;
    // Sequences 1 and 2 are the two vouches (governance mail, member-visible).

    // 3: private mail creator -> bob. 4: work item. 5: postmaster mail
    // (member-visible). 6: private mail creator -> bob again.
    let letters: [(u64, Option<&str>, Option<&str>); 4] = [
        (10, Some("bob"), None),
        (11, None, Some("need")),
        (12, Some("POSTMASTER"), None),
        (13, Some("bob"), Some("mail")),
    ];
    for (n, to, kind) in letters {
        let id = traveling(n);
        let raw = if to == Some("POSTMASTER") {
            // A postmaster letter must carry a vouch; re-vouch an
            // already-admitted member (idempotent) to keep it valid.
            vouch_letter(&id, "bob", &key(72))
        } else {
            eml(&id, "private")
        };
        let (status, value) = fx.append(&id, &raw, to, kind).await;
        assert_eq!(status, StatusCode::CREATED, "{value}");
    }

    let creator_view = read(&fx.app, &fx.project, &fx.token, 0).await;
    assert_eq!(ids(&creator_view).len(), 6);

    let bob_view = read(&fx.app, &fx.project, &bob, 0).await;
    assert_eq!(ids(&bob_view).len(), 6, "bob is the recipient of both");
    assert_eq!(bob_view["next_cursor"], 6);

    // The bystander sees both vouches, the work item, and the postmaster
    // letter, but neither private mail; the cursor stops at 5.
    let carol_view = read(&fx.app, &fx.project, &carol, 0).await;
    assert_eq!(
        ids(&carol_view),
        vec![traveling(90), traveling(91), traveling(11), traveling(12)]
    );
    assert_eq!(carol_view["next_cursor"], 5);
    let sequences: Vec<u64> = carol_view["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["sequence"].as_u64().unwrap())
        .collect();
    assert_eq!(sequences, vec![1, 2, 4, 5]);

    // Resuming after the last returned record yields nothing and keeps the
    // cursor where it was.
    let tail = read(&fx.app, &fx.project, &carol, 5).await;
    assert!(tail["records"].as_array().unwrap().is_empty());
    assert_eq!(tail["next_cursor"], 5);
}

#[tokio::test]
async fn vouch_wire_admits_refuses_malformed_and_conflicts_without_storing() {
    let fx = fixture("vouch", 81).await;

    // Malformed: not JSON, missing key, no blank line, non-UTF-8 body.
    let mut non_utf8 = b"Subject: v\n\n".to_vec();
    non_utf8.extend_from_slice(&[0xff, 0xfe]);
    let malformed: Vec<Vec<u8>> = vec![
        eml(&traveling(1), "not a vouch"),
        eml(&traveling(1), r#"{"vouch":{"name":"builder"}}"#),
        b"Subject: v\n{\"vouch\":{}}".to_vec(),
        non_utf8,
    ];
    for raw in malformed {
        let (status, value) = fx
            .append(&traveling(1), &raw, Some("postmaster"), None)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
    }

    // A valid vouch (CRLF letter) still lands at sequence 1: nothing above
    // was stored.
    let body = json!({"vouch": {"name": "builder", "public_key_pem": spki_pem(&key(82))}});
    let crlf = format!("Subject: vouch\r\n\r\n{body}\r\n").into_bytes();
    let (status, vouched) = fx
        .append(&traveling(2), &crlf, Some("postmaster"), Some("mail"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{vouched}");
    assert_eq!(vouched["sequence"], 1);
    let (status, identity) = register(&fx.app, &fx.project, &key(82), "builder").await;
    assert_eq!(status, StatusCode::CREATED, "{identity}");

    // The same name for a different key conflicts; nothing is stored, so the
    // next letter is sequence 2.
    let other = vouch_letter(&traveling(3), "builder", &key(83));
    let (status, conflict) = fx
        .append(&traveling(3), &other, Some("postmaster"), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    let (status, next) = fx
        .append(&traveling(4), &eml(&traveling(4), "next"), None, None)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{next}");
    assert_eq!(next["sequence"], 2);

    // The vouch letter is served exactly as sent.
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    let served = STANDARD
        .decode(items["records"][0]["raw_eml"].as_str().unwrap())
        .unwrap();
    assert_eq!(served, crlf);
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[tokio::test]
async fn letters_are_sealed_at_rest() {
    let fx = fixture("sealed", 91).await;
    let marker = "PLAINTEXT-MARKER-7f3a9c41d2e8";
    let id = traveling(1);
    let raw = eml(&id, &format!("secret {marker} secret"));
    let (status, created) = fx.append(&id, &raw, Some("builder"), Some("need")).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let project_dir = fx.dir.join("projects").join(&fx.project);
    let mut files = Vec::new();
    walk_files(&project_dir, &mut files);
    let blob = project_dir.join("letters").join(format!(
        "{}.sealed",
        created["content_sha256"].as_str().unwrap()
    ));
    assert!(files.contains(&blob), "blob named by content hash exists");
    for path in files {
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes.windows(marker.len()).any(|w| w == marker.as_bytes()),
            "{} holds plaintext letter bytes",
            path.display()
        );
        // The raw letter bytes (even sealed-then-b64 would not match).
        assert!(
            !bytes.windows(raw.len()).any(|w| w == raw.as_slice()),
            "{} holds the whole letter",
            path.display()
        );
    }
}

#[tokio::test]
async fn restart_converges_over_the_same_data_dir() {
    let fx = fixture("restart", 101).await;
    let ids_and_bytes: Vec<(String, Vec<u8>)> = (1..=3u64)
        .map(|n| {
            let id = traveling(n);
            let mut raw = eml(&id, &format!("letter {n}"));
            raw.extend_from_slice(&[0xff, b'\r', b'\n', 0x00]);
            (id, raw)
        })
        .collect();
    for (id, raw) in &ids_and_bytes {
        let (status, value) = fx.append(id, raw, None, Some("need")).await;
        assert_eq!(status, StatusCode::CREATED, "{value}");
    }
    let before = read(&fx.app, &fx.project, &fx.token, 0).await;

    // A second router over the same data dir (a process restart): the
    // persisted bearer still opens, sequences and bytes are identical.
    let restarted = api_router(fx.dir.clone(), Some(store_key()), None);
    let after = read(&restarted, &fx.project, &fx.token, 0).await;
    assert_eq!(before, after);

    // Replays are duplicates with the original sequences; a fresh letter
    // continues the counter.
    for (n, (id, raw)) in ids_and_bytes.iter().enumerate() {
        let (status, value) = append(
            &restarted,
            &fx.project,
            &fx.token,
            id,
            raw,
            None,
            Some("need"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["duplicate"], true);
        assert_eq!(value["sequence"], n as u64 + 1);
    }
    let id = traveling(4);
    let (status, fresh) = append(
        &restarted,
        &fx.project,
        &fx.token,
        &id,
        &eml(&id, "after restart"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{fresh}");
    assert_eq!(fresh["sequence"], 4);
}

#[tokio::test]
async fn unreadable_blob_is_an_internal_error_not_a_skip() {
    let fx = fixture("missing-blob", 111).await;
    let id = traveling(1);
    let (status, created) = fx.append(&id, &eml(&id, "one"), None, Some("need")).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let blob = fx
        .dir
        .join("projects")
        .join(&fx.project)
        .join("letters")
        .join(format!(
            "{}.sealed",
            created["content_sha256"].as_str().unwrap()
        ));
    std::fs::remove_file(blob).unwrap();
    let (status, error) = send(
        &fx.app,
        "GET",
        &format!("/v1/projects/{}/items?since=0", fx.project),
        None,
        Some(&fx.token),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{error}");
    assert_eq!(error["error"]["code"], "internal");
}

#[tokio::test]
async fn token_renewal_issues_a_new_token_and_keeps_the_old_one_valid() {
    let fx = fixture("renew", 121).await;
    let creator_key = key(121);
    let (status, renewed) =
        renew_with(&fx.app, &fx.project, "creator", &creator_key, &creator_key).await;
    assert_eq!(status, StatusCode::OK, "{renewed}");
    let new_token = renewed["token"].as_str().unwrap();
    assert_ne!(new_token, fx.token);
    assert!(renewed["token_expires_at"].as_str().unwrap().ends_with('Z'));

    // Both tokens authenticate an items read; the new one is the creator.
    for token in [new_token, fx.token.as_str()] {
        let items = read(&fx.app, &fx.project, token, 0).await;
        assert!(items["records"].as_array().unwrap().is_empty());
    }
    let id = traveling(1);
    let (status, created) = append(
        &fx.app,
        &fx.project,
        new_token,
        &id,
        &eml(&id, "via renewed token"),
        None,
        Some("need"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let items = read(&fx.app, &fx.project, new_token, 0).await;
    assert_eq!(items["records"][0]["from"], "creator");
}

#[tokio::test]
async fn token_renewal_fails_closed_with_one_generic_401() {
    let fx = fixture("renew-fail", 131).await;
    let creator_key = key(131);
    let builder_key = key(132);
    fx.add_member(&traveling(90), "builder", &builder_key).await;
    let generic = |status: StatusCode, value: &Value| {
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
        assert_eq!(value["error"]["code"], "unauthorized");
        value["error"]["message"].clone()
    };

    // Wrong signing key for the challenge it fetched.
    let (s, wrong_sig) =
        renew_with(&fx.app, &fx.project, "creator", &creator_key, &builder_key).await;
    let m1 = generic(s, &wrong_sig);
    // Challenge issued for another identity's key, presented for `creator`.
    let (s, other_key) =
        renew_with(&fx.app, &fx.project, "creator", &builder_key, &builder_key).await;
    let m2 = generic(s, &other_key);
    // Unknown name.
    let (s, unknown_name) =
        renew_with(&fx.app, &fx.project, "nobody", &creator_key, &creator_key).await;
    let m3 = generic(s, &unknown_name);
    // Unknown challenge id.
    let (s, unknown_challenge) = send(
        &fx.app,
        "POST",
        &format!("/v1/projects/{}/auth/token", fx.project),
        Some(json!({"name": "creator", "challenge_id": "nope", "signature_base64": "AAAA"})),
        None,
    )
    .await;
    let m4 = generic(s, &unknown_challenge);
    assert!([&m2, &m3, &m4].iter().all(|m| **m == m1), "one message");

    // Replay: a challenge is spent on first use, even a successful one.
    let (challenge_id, bytes) = challenge(&fx.app, &fx.project, &creator_key).await;
    let signature = STANDARD.encode(creator_key.sign(&bytes).to_bytes());
    let body =
        json!({"name": "creator", "challenge_id": challenge_id, "signature_base64": signature});
    let uri = format!("/v1/projects/{}/auth/token", fx.project);
    let (status, first) = send(&fx.app, "POST", &uri, Some(body.clone()), None).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (s, replay) = send(&fx.app, "POST", &uri, Some(body), None).await;
    generic(s, &replay);

    // A failed attempt also spends its challenge.
    let (challenge_id, bytes) = challenge(&fx.app, &fx.project, &creator_key).await;
    let bad = json!({"name": "builder", "challenge_id": challenge_id,
        "signature_base64": STANDARD.encode(creator_key.sign(&bytes).to_bytes())});
    let (s, value) = send(&fx.app, "POST", &uri, Some(bad), None).await;
    generic(s, &value);
    let good = json!({"name": "creator", "challenge_id": challenge_id,
        "signature_base64": STANDARD.encode(creator_key.sign(&bytes).to_bytes())});
    let (s, value) = send(&fx.app, "POST", &uri, Some(good), None).await;
    generic(s, &value);
}

#[tokio::test]
async fn token_renewal_is_keyed_to_the_project() {
    let fx = fixture("renew-project", 141).await;
    let creator_key = key(141);

    // A second project where the same key is also the creator.
    let (status, other) = send(
        &fx.app,
        "POST",
        "/v1/projects",
        Some(json!({"public_key_pem": spki_pem(&creator_key), "creator_name": "creator"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{other}");
    let other = other["project_id"].as_str().unwrap();
    let (status, value) = register(&fx.app, other, &creator_key, "creator").await;
    assert_eq!(status, StatusCode::CREATED, "{value}");

    // A challenge issued for project B is refused at project A.
    let (challenge_id, bytes) = challenge(&fx.app, other, &creator_key).await;
    let signature = STANDARD.encode(creator_key.sign(&bytes).to_bytes());
    let (status, value) = send(
        &fx.app,
        "POST",
        &format!("/v1/projects/{}/auth/token", fx.project),
        Some(
            json!({"name": "creator", "challenge_id": challenge_id, "signature_base64": signature}),
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");

    // Unknown project: 404.
    let unknown = format!(
        "{}{}",
        if fx.project.starts_with('z') {
            "y"
        } else {
            "z"
        },
        &fx.project[1..]
    );
    let (status, value) = send(
        &fx.app,
        "POST",
        &format!("/v1/projects/{unknown}/auth/token"),
        Some(json!({"name": "creator", "challenge_id": "x", "signature_base64": "AAAA"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{value}");
    assert_eq!(value["error"]["code"], "project_not_found");
}

#[tokio::test]
async fn raw_body_round_trips_byte_identically_through_put_then_get() {
    let fx = fixture("raw-body", 151).await;
    // Non-UTF-8, NUL, CRLF, a lone CR, and no trailing newline; also a
    // body that looks like JSON and a base64-ish run, which must stay raw.
    let mut raw = b"From: a@p.local\r\nSubject: raw\r\n\r\n".to_vec();
    raw.extend_from_slice(&[0xff, 0xfe, 0x00, 0x80, b'\r', b'{', b'"', b'=', b'=']);
    raw.extend_from_slice(b"\r\nlast line without newline");
    let id = traveling(1);
    let (status, created) = fx.append(&id, &raw, None, Some("task")).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["content_sha256"], sha256_hex(&raw));

    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    let served = STANDARD
        .decode(items["records"][0]["raw_eml"].as_str().unwrap())
        .unwrap();
    assert_eq!(served, raw);
}

#[tokio::test]
async fn the_content_type_is_ignored() {
    let fx = fixture("content-type", 152).await;
    for (n, content_type) in ["application/json", "text/plain", "application/x-weird"]
        .into_iter()
        .enumerate()
    {
        let id = traveling(n as u64 + 1);
        let raw = eml(&id, "typed oddly");
        let (status, value) = raw_request(
            &fx.app,
            "PUT",
            &format!("/v1/projects/{}/items/{id}", fx.project),
            raw.clone(),
            Some(&fx.token),
            vec![("content-type".to_owned(), content_type.to_owned())],
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{content_type}: {value}");
        assert_eq!(value["content_sha256"], sha256_hex(&raw));
    }
}

#[tokio::test]
async fn the_old_post_append_route_is_gone() {
    let fx = fixture("old-route", 153).await;
    let id = traveling(1);
    let raw = eml(&id, "old wire");
    let (status, _) = raw_request(
        &fx.app,
        "POST",
        &format!("/v1/projects/{}/items", fx.project),
        json!({"raw_eml": STANDARD.encode(&raw)})
            .to_string()
            .into_bytes(),
        Some(&fx.token),
        vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            ("message-id".to_owned(), id.clone()),
        ],
    )
    .await;
    assert!(!status.is_success(), "POST append must not exist: {status}");
    assert!(
        status == StatusCode::METHOD_NOT_ALLOWED || status == StatusCode::NOT_FOUND,
        "{status}"
    );
    // Nothing was stored, and the collection path still reads.
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    assert!(items["records"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn the_path_id_wins_over_a_message_id_header() {
    let fx = fixture("path-id", 154).await;
    let path_id = traveling(1);
    let header_id = traveling(2);
    let raw = eml(&path_id, "path id");
    let (status, created) = put_with(
        &fx.app,
        &fx.project,
        Some(&fx.token),
        &path_id,
        &raw,
        &[("message-id", &header_id)],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["message_id"], path_id);
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    assert_eq!(ids(&items), vec![path_id]);

    // A valid Message-ID header does not rescue a bad path id.
    let (status, value) = put_with(
        &fx.app,
        &fx.project,
        Some(&fx.token),
        "not-an-id",
        &raw,
        &[("message-id", &header_id)],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
    assert_eq!(value["error"]["code"], "invalid_request");
}

/// The extractor limit sits just above the cap, so both the cap's own 413
/// and an extractor-level 413 (far over the limit) use our JSON shape.
#[tokio::test]
async fn oversize_bodies_get_the_json_413_shape() {
    let fx = fixture("cap-413", 155).await;
    let cap = 64 * 1024;
    for (n, len, expected) in [
        (1u64, cap, StatusCode::CREATED),
        (2, cap + 1, StatusCode::PAYLOAD_TOO_LARGE),
        // Past the route's body limit, rejected before the handler's check.
        (3, 200 * 1024, StatusCode::PAYLOAD_TOO_LARGE),
        (4, 3 * 1024 * 1024, StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let id = traveling(n);
        let mut raw = eml(&id, "");
        raw.resize(len, b'x');
        let (status, value) = fx.append(&id, &raw, None, None).await;
        assert_eq!(status, expected, "len {len}: {value}");
        if expected == StatusCode::PAYLOAD_TOO_LARGE {
            assert_eq!(value["error"]["code"], "body_too_large", "len {len}");
        }
    }
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    assert_eq!(ids(&items), vec![traveling(1)]);
}

/// `GET /items/{id}` with the raw response: status, headers, body bytes.
async fn get_letter(
    app: &Router,
    project: &str,
    token: Option<&str>,
    id: &str,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut builder = Request::builder()
        .method("GET")
        .uri(format!("/v1/projects/{project}/items/{id}"));
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, headers, bytes.to_vec())
}

fn header<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).map(|v| v.to_str().unwrap())
}

#[tokio::test]
async fn single_letter_get_returns_raw_bytes_with_envelope_headers() {
    let fx = fixture("get-one", 161).await;
    // Non-UTF-8, NUL, CRLF, no trailing newline.
    let mut raw = b"From: a@p.local\r\nSubject: raw\r\n\r\n".to_vec();
    raw.extend_from_slice(&[0xff, 0xfe, 0x00, 0x80, b'\r', b'\n']);
    raw.extend_from_slice(b"no trailing newline");

    // First letter, no envelope fields: To and Kind headers are omitted.
    let plain_id = traveling(1);
    let (status, value) = fx.append(&plain_id, &raw, None, None).await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    let (status, headers, body) =
        get_letter(&fx.app, &fx.project, Some(&fx.token), &plain_id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, raw, "byte-identical to what was PUT");
    assert_eq!(header(&headers, "content-type"), Some("message/rfc822"));
    assert_eq!(
        header(&headers, "x-vivi-message-id"),
        Some(plain_id.as_str())
    );
    assert_eq!(header(&headers, "x-vivi-from"), Some("creator"));
    assert_eq!(header(&headers, "x-vivi-sequence"), Some("1"));
    assert_eq!(
        header(&headers, "x-vivi-content-sha256"),
        Some(sha256_hex(&body).as_str())
    );
    assert_eq!(header(&headers, "cache-control"), Some("no-store"));
    assert!(headers.get("x-vivi-to").is_none());
    assert!(headers.get("x-vivi-kind").is_none());

    // Second letter with both fields.
    let id = traveling(2);
    let letter = eml(&id, "kinded");
    let (status, value) = fx.append(&id, &letter, Some("builder"), Some("NEED")).await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    let (status, headers, body) = get_letter(&fx.app, &fx.project, Some(&fx.token), &id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, letter);
    assert_eq!(header(&headers, "x-vivi-to"), Some("builder"));
    assert_eq!(header(&headers, "x-vivi-kind"), Some("need"));
    assert_eq!(header(&headers, "x-vivi-sequence"), Some("2"));
}

#[tokio::test]
async fn single_letter_get_applies_list_visibility() {
    let fx = fixture("get-visibility", 171).await;
    let bob = fx.add_member(&traveling(90), "bob", &key(172)).await;
    let carol = fx.add_member(&traveling(91), "carol", &key(173)).await;

    let work = traveling(10);
    let mail = traveling(11);
    let gov = traveling(12);
    assert_eq!(
        fx.append(&work, &eml(&work, "w"), None, Some("task"))
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(
        fx.append(&mail, &eml(&mail, "m"), Some("bob"), None)
            .await
            .0,
        StatusCode::CREATED
    );
    // A governance letter must carry a vouch; re-vouching bob is idempotent.
    let gov_raw = vouch_letter(&gov, "bob", &key(172));
    assert_eq!(
        fx.append(&gov, &gov_raw, Some("postmaster"), None).await.0,
        StatusCode::CREATED
    );

    // Work items and postmaster mail: every member.
    for token in [&fx.token, &bob, &carol] {
        let (status, _, _) = get_letter(&fx.app, &fx.project, Some(token), &work).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _, body) = get_letter(&fx.app, &fx.project, Some(token), &gov).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, gov_raw);
    }
    // Mail: sender and recipient only.
    for token in [&fx.token, &bob] {
        let (status, _, _) = get_letter(&fx.app, &fx.project, Some(token), &mail).await;
        assert_eq!(status, StatusCode::OK);
    }

    // A not-visible letter and an unknown id are byte-identical 404s.
    let (hidden_status, hidden_headers, hidden_body) =
        get_letter(&fx.app, &fx.project, Some(&carol), &mail).await;
    let (unknown_status, unknown_headers, unknown_body) =
        get_letter(&fx.app, &fx.project, Some(&carol), &traveling(999)).await;
    assert_eq!(hidden_status, StatusCode::NOT_FOUND);
    assert_eq!(unknown_status, StatusCode::NOT_FOUND);
    assert_eq!(hidden_body, unknown_body);
    assert_eq!(hidden_headers, unknown_headers);
    let value: Value = serde_json::from_slice(&hidden_body).unwrap();
    assert_eq!(value["error"]["code"], "item_not_found");
    assert_eq!(value["error"]["message"], "no such letter");
}

#[tokio::test]
async fn single_letter_get_error_cases() {
    let fx = fixture("get-errors", 181).await;
    let id = traveling(1);
    assert_eq!(
        fx.append(&id, &eml(&id, "x"), None, Some("need")).await.0,
        StatusCode::CREATED
    );
    let json_of = |body: &[u8]| serde_json::from_slice::<Value>(body).unwrap();

    // Malformed id: 400.
    let (status, _, body) = get_letter(&fx.app, &fx.project, Some(&fx.token), "plain-1").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json_of(&body)["error"]["code"], "invalid_request");

    // Missing and invalid bearer: 401.
    let (status, _, body) = get_letter(&fx.app, &fx.project, None, &id).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json_of(&body)["error"]["code"], "unauthorized");
    let (status, _, _) = get_letter(&fx.app, &fx.project, Some("not-a-token"), &id).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Unknown project: 404 project_not_found.
    let unknown = format!(
        "{}{}",
        if fx.project.starts_with('z') {
            "y"
        } else {
            "z"
        },
        &fx.project[1..]
    );
    let (status, _, body) = get_letter(&fx.app, &unknown, Some(&fx.token), &id).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json_of(&body)["error"]["code"], "project_not_found");

    // A token from another project does not open this one.
    let other = fixture("get-errors-other", 182).await;
    let (status, _, _) = get_letter(&fx.app, &fx.project, Some(&other.token), &id).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// An indexed letter whose blob is gone or does not match its hash is a 500,
/// never a 404 (a 404 would say "no such letter" about a letter we hold).
#[tokio::test]
async fn single_letter_get_with_an_unreadable_blob_is_500_not_404() {
    let fx = fixture("get-missing-blob", 191).await;
    let first = traveling(1);
    let second = traveling(2);
    let (_, created) = fx
        .append(&first, &eml(&first, "one"), None, Some("need"))
        .await;
    let (_, other) = fx
        .append(&second, &eml(&second, "two"), None, Some("need"))
        .await;
    let letters = fx.dir.join("projects").join(&fx.project).join("letters");
    let blob = |value: &Value| {
        letters.join(format!(
            "{}.sealed",
            value["content_sha256"].as_str().unwrap()
        ))
    };

    // Swapped blob: unseals fine, hash mismatch.
    std::fs::copy(blob(&other), blob(&created)).unwrap();
    let (status, _, body) = get_letter(&fx.app, &fx.project, Some(&fx.token), &first).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["error"]["code"],
        "internal"
    );

    // Missing blob.
    std::fs::remove_file(blob(&created)).unwrap();
    let (status, _, _) = get_letter(&fx.app, &fx.project, Some(&fx.token), &first).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    // The other letter is unaffected.
    let (status, _, _) = get_letter(&fx.app, &fx.project, Some(&fx.token), &second).await;
    assert_eq!(status, StatusCode::OK);
}
