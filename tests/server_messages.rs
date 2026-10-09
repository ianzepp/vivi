//! Integration tests for VC-4: bearer-authenticated send + poll/long-poll,
//! Message-ID dedup, cursor advance, origin verification, and body/rate caps
//! over the v1 router (docs/contract.md). Helper style follows tests/auth.rs.

use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use tower::util::ServiceExt;

use vivi_server::api_router;

fn scratch_data_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vivi-server-vc4-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// Valid `STORE_ENCRYPTION_KEY` (standard base64 of 32 bytes) for routers.
fn store_key() -> String {
    STANDARD.encode([0x42u8; 32])
}

fn spki_pem(key: &SigningKey) -> String {
    const ED25519_SPKI_PREFIX: &[u8] = &[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut der = ED25519_SPKI_PREFIX.to_vec();
    der.extend_from_slice(key.verifying_key().as_bytes());
    format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
        STANDARD.encode(der)
    )
}

async fn send_req(
    app: &Router,
    method: &str,
    uri: &str,
    headers: Vec<(&str, String)>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    let payload = body.map(|v| v.to_string()).unwrap_or_default();
    let req = builder.body(Body::from(payload)).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).expect("JSON body"))
}

async fn create_room(
    app: &Router,
    key: &SigningKey,
    seats: u32,
    label: &str,
) -> (StatusCode, Value) {
    send_req(
        app,
        "POST",
        "/v1/rooms",
        Vec::new(),
        Some(json!({
            "seats": seats,
            "label": label,
            "public_key_pem": spki_pem(key),
        })),
    )
    .await
}

/// Full challenge → sign → join flow for one key.
async fn join(app: &Router, room_id: &str, key: &SigningKey, label: &str) -> (StatusCode, Value) {
    let (status, challenge) = send_req(
        app,
        "POST",
        &format!("/v1/rooms/{room_id}/challenge"),
        Vec::new(),
        Some(json!({ "public_key_pem": spki_pem(key) })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "challenge must issue: {challenge}");
    let bytes = STANDARD
        .decode(challenge["challenge"].as_str().unwrap())
        .unwrap();
    let signature = STANDARD.encode(key.sign(&bytes).to_bytes());
    send_req(
        app,
        "POST",
        &format!("/v1/rooms/{room_id}/join"),
        Vec::new(),
        Some(json!({
            "challenge_id": challenge["challenge_id"],
            "signature_base64": signature,
            "public_key_pem": spki_pem(key),
            "label": label,
        })),
    )
    .await
}

struct Seat {
    token: String,
    label: String,
    fingerprint: String,
}

impl Seat {
    fn headers(&self, message_id: Option<&str>) -> Vec<(&'static str, String)> {
        let mut headers = vec![
            ("authorization", format!("Bearer {}", self.token)),
            ("x-vivi-chat-from", self.label.clone()),
            (
                "x-vivi-chat-fingerprint",
                self.fingerprint.chars().take(16).collect(),
            ),
        ];
        if let Some(id) = message_id {
            headers.push(("message-id", id.to_owned()));
        }
        headers
    }
}

async fn setup_two_seats(name: &str) -> (Router, PathBuf, String, Seat, Seat) {
    let data_dir = scratch_data_dir(name);
    let app = api_router(data_dir.clone(), Some(store_key()), None);
    let (status, room) = create_room(&app, &test_key(1), 4, "creator").await;
    assert_eq!(status, StatusCode::CREATED, "{room}");
    let (status, joined) = join(
        &app,
        room["room_id"].as_str().unwrap(),
        &test_key(2),
        "joiner",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{joined}");
    let room_id = room["room_id"].as_str().unwrap().to_owned();
    let creator = Seat {
        token: room["token"].as_str().unwrap().to_owned(),
        label: room["label"].as_str().unwrap().to_owned(),
        fingerprint: room["fingerprint"].as_str().unwrap().to_owned(),
    };
    let joiner = Seat {
        token: joined["token"].as_str().unwrap().to_owned(),
        label: joined["label"].as_str().unwrap().to_owned(),
        fingerprint: joined["fingerprint"].as_str().unwrap().to_owned(),
    };
    (app, data_dir, room_id, creator, joiner)
}

#[tokio::test]
async fn content_is_encrypted_at_rest_and_decrypts_for_pollers() {
    let (app, data_dir, room_id, creator, joiner) = setup_two_seats("at-rest").await;
    let body_text = "secret plaintext body";
    let subject_text = "hush subject";

    let (status, sent) = send_req(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/messages"),
        creator.headers(None),
        Some(json!({ "subject": subject_text, "body": body_text })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{sent}");

    // The sidecar index carries no plaintext content.
    let sidecar =
        std::fs::read_to_string(data_dir.join("rooms").join(&room_id).join("messages.json"))
            .unwrap();
    assert!(!sidecar.contains(body_text), "body plaintext on disk");
    assert!(!sidecar.contains(subject_text), "subject plaintext on disk");
    assert!(sidecar.contains("\"enc\""), "content sealed as enc");

    // No file under the room tree (mail.sqlite included) holds the plaintext.
    let room_dir = data_dir.join("rooms").join(&room_id);
    let mut stack = vec![room_dir.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_some_and(|e| e == "sqlite-shm") {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            assert!(
                !windows_contains(&bytes, body_text.as_bytes()),
                "plaintext body found in {}",
                path.display()
            );
        }
    }

    // An authorized bearer gets transparent decryption.
    let (status, poll) = send_req(
        &app,
        "GET",
        &format!("/v1/rooms/{room_id}/messages?since=0"),
        joiner.headers(None),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{poll}");
    let messages = poll["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["body"], body_text);
    assert_eq!(messages[0]["subject"], subject_text);

    // Dedup replay also decrypts the stored record.
    let (status, replay) = send_req(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/messages"),
        creator.headers(Some(sent["message_id"].as_str().unwrap())),
        Some(json!({ "subject": subject_text, "body": body_text })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["duplicate"], true);
}

/// Naive substring scan over raw bytes (no std helper for that).
fn windows_contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[tokio::test]
async fn sends_without_encryption_key_are_refused() {
    let data_dir = scratch_data_dir("no-key");
    let app = api_router(data_dir.clone(), None, None);
    let (status, room) = create_room(&app, &test_key(1), 4, "creator").await;
    assert_eq!(status, StatusCode::CREATED, "{room}");
    let room_id = room["room_id"].as_str().unwrap().to_owned();
    let creator = Seat {
        token: room["token"].as_str().unwrap().to_owned(),
        label: room["label"].as_str().unwrap().to_owned(),
        fingerprint: room["fingerprint"].as_str().unwrap().to_owned(),
    };
    let (status, body) = send_req(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/messages"),
        creator.headers(None),
        Some(json!({ "body": "should refuse" })),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"]["code"], "internal");
}

#[tokio::test]
async fn send_then_poll_round_trips_between_two_seats() {
    let (app, _dir, room_id, creator, joiner) = setup_two_seats("round-trip").await;

    let (status, sent) = send_req(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/messages"),
        creator.headers(None),
        Some(json!({ "subject": "hello", "body": "first broadcast" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{sent}");
    assert_eq!(sent["cursor"], 1);
    assert!(sent["message_id"].as_str().is_some());
    assert!(sent["created_at"].as_str().is_some());

    let (status, poll) = send_req(
        &app,
        "GET",
        &format!("/v1/rooms/{room_id}/messages?since=0"),
        joiner.headers(None),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{poll}");
    let messages = poll["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1, "joiner sees the broadcast once");
    assert_eq!(messages[0]["from"], "creator");
    assert_eq!(messages[0]["body"], "first broadcast");
    assert_eq!(messages[0]["subject"], "hello");
    assert_eq!(messages[0]["cursor"], 1);
    let fingerprint = messages[0]["fingerprint"].as_str().unwrap();
    assert_eq!(fingerprint.len(), 16, "short fingerprint form");
    assert_eq!(
        fingerprint,
        &creator.fingerprint[..16],
        "fingerprint echoes the sender seat"
    );
    assert_eq!(poll["next_cursor"], 1);

    // The reply is a direct send; the creator polls past the first cursor.
    let (status, reply) = send_req(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/messages"),
        joiner.headers(None),
        Some(json!({ "to": "creator", "body": "direct reply" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{reply}");
    assert_eq!(reply["cursor"], 2);

    let (status, poll) = send_req(
        &app,
        "GET",
        &format!("/v1/rooms/{room_id}/messages?since=1"),
        creator.headers(None),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{poll}");
    let messages = poll["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1, "since cursor skips delivered items");
    assert_eq!(messages[0]["from"], "joiner");
    assert_eq!(messages[0]["to"], "creator");
    assert_eq!(messages[0]["body"], "direct reply");
    assert_eq!(poll["next_cursor"], 2);

    // An empty poll keeps the since cursor as next_cursor.
    let (status, poll) = send_req(
        &app,
        "GET",
        &format!("/v1/rooms/{room_id}/messages?since=2"),
        creator.headers(None),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{poll}");
    assert_eq!(poll["messages"].as_array().unwrap().len(), 0);
    assert_eq!(poll["next_cursor"], 2);
}

#[tokio::test]
async fn replayed_message_id_is_noop_with_duplicate_true() {
    let (app, _dir, room_id, creator, joiner) = setup_two_seats("replay").await;

    let (status, first) = send_req(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/messages"),
        creator.headers(Some("replay-1")),
        Some(json!({ "body": "only once" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");

    let (status, replay) = send_req(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/messages"),
        creator.headers(Some("replay-1")),
        Some(json!({ "body": "only once" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "replay returns 200");
    assert_eq!(replay["duplicate"], true);
    assert_eq!(replay["message_id"], first["message_id"]);
    assert_eq!(replay["cursor"], first["cursor"]);

    let (status, poll) = send_req(
        &app,
        "GET",
        &format!("/v1/rooms/{room_id}/messages?since=0"),
        joiner.headers(None),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{poll}");
    assert_eq!(
        poll["messages"].as_array().unwrap().len(),
        1,
        "replay never stores twice"
    );
}

#[tokio::test]
async fn unauthorized_send_is_401() {
    let (app, _dir, room_id, _creator, _joiner) = setup_two_seats("unauthorized").await;

    let (status, body) = send_req(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/messages"),
        Vec::new(),
        Some(json!({ "body": "no token" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["code"], "unauthorized");

    let (status, body) = send_req(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/messages"),
        vec![("authorization", "Bearer not-a-token".to_owned())],
        Some(json!({ "body": "bad token" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn body_over_cap_is_413() {
    let (app, _dir, room_id, creator, _joiner) = setup_two_seats("body-cap").await;

    let oversized = "a".repeat(64 * 1024 + 1);
    let (status, body) = send_req(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/messages"),
        creator.headers(None),
        Some(json!({ "body": oversized })),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(body["error"]["code"], "body_too_large");

    // At the cap exactly is accepted.
    let at_cap = "a".repeat(64 * 1024);
    let (status, sent) = send_req(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/messages"),
        creator.headers(None),
        Some(json!({ "body": at_cap })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{sent}");
}

#[tokio::test]
async fn wrong_label_from_is_rejected() {
    let (app, _dir, room_id, creator, _joiner) = setup_two_seats("wrong-label").await;

    let mut headers = creator.headers(None);
    headers[1] = ("x-vivi-chat-from", "someone-else".to_owned());
    let (status, body) = send_req(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/messages"),
        headers,
        Some(json!({ "body": "spoofed from" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn wrong_fingerprint_is_rejected() {
    let (app, _dir, room_id, creator, _joiner) = setup_two_seats("wrong-fp").await;

    let mut headers = creator.headers(None);
    headers[2] = ("x-vivi-chat-fingerprint", "0123456789abcdef".to_owned());
    let (status, body) = send_req(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/messages"),
        headers,
        Some(json!({ "body": "spoofed fingerprint" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["code"], "unauthorized");
}
