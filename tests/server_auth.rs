//! Integration tests for VC-3: seats, challenge/join auth, and digest-only
//! bearer tokens over the v1 router (docs/contract.md hardening summary).

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
use vivi_server::auth::sha256_hex;
use vivi_server::seats::SeatRegistry;

fn scratch_data_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vivi-server-vc3-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// Build the SPKI PEM (openssl `BEGIN PUBLIC KEY` shape) for a test key.
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

async fn send(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
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
    send(
        app,
        "POST",
        "/v1/rooms",
        Some(json!({
            "seats": seats,
            "label": label,
            "public_key_pem": spki_pem(key),
        })),
    )
    .await
}

/// Full challenge → sign → join flow for one key.
async fn join(
    app: &Router,
    room_id: &str,
    key: &SigningKey,
    label: Option<&str>,
) -> (StatusCode, Value) {
    let (status, challenge) = send(
        app,
        "POST",
        &format!("/v1/rooms/{room_id}/challenge"),
        Some(json!({ "public_key_pem": spki_pem(key) })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "challenge must issue: {challenge}");
    assert_eq!(challenge["expires_in"], 120);
    let bytes = STANDARD
        .decode(challenge["challenge"].as_str().unwrap())
        .unwrap();
    assert_eq!(bytes.len(), 32, "challenge is 32 random bytes");
    let signature = STANDARD.encode(key.sign(&bytes).to_bytes());
    let mut body = json!({
        "challenge_id": challenge["challenge_id"],
        "signature_base64": signature,
        "public_key_pem": spki_pem(key),
    });
    if let Some(label) = label {
        body["label"] = json!(label);
    }
    send(
        app,
        "POST",
        &format!("/v1/rooms/{room_id}/join"),
        Some(body),
    )
    .await
}

fn seats_json(data_dir: &PathBuf, room_id: &str) -> String {
    std::fs::read_to_string(data_dir.join("rooms").join(room_id).join("seats.json")).unwrap()
}

#[tokio::test]
async fn creator_binds_seat_1_and_token_is_digest_only_at_rest() {
    let data_dir = scratch_data_dir("creator");
    let app = api_router(data_dir.clone(), Some(STANDARD.encode([0x42u8; 32])), None);
    let key = test_key(1);

    let (status, body) = create_room(&app, &key, 4, "creator-seat").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["seat"], 1);
    assert_eq!(body["label"], "creator-seat");
    let token = body["token"].as_str().unwrap().to_owned();
    let fingerprint = body["fingerprint"].as_str().unwrap().to_owned();
    assert_eq!(fingerprint.len(), 64, "full 64-hex fingerprint");
    assert!(body["token_expires_at"].as_str().is_some());

    let raw = seats_json(&data_dir, body["room_id"].as_str().unwrap());
    assert!(!raw.contains(&token), "raw token must never be stored");
    let digest = sha256_hex(token.as_bytes());
    assert_eq!(digest.len(), 64);
    assert!(raw.contains(&digest), "digest record must be stored");
    assert!(
        !raw.contains("BEGIN PUBLIC KEY"),
        "no PEM at rest; SPKI DER only"
    );
}

#[tokio::test]
async fn two_keys_bind_two_seats_and_third_is_refused_at_capacity() {
    let data_dir = scratch_data_dir("capacity-full");
    let app = api_router(data_dir.clone(), Some(STANDARD.encode([0x42u8; 32])), None);
    let creator = test_key(1);
    let (status, room) = create_room(&app, &creator, 2, "creator").await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, joined) = join(
        &app,
        room["room_id"].as_str().unwrap(),
        &test_key(2),
        Some("second"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{joined}");
    assert_eq!(joined["seat"], 2);
    assert_eq!(joined["label"], "second");

    let (status, body) = join(&app, room["room_id"].as_str().unwrap(), &test_key(3), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "seat_full");
}

#[tokio::test]
async fn rebind_held_seat_keeps_seat_and_mints_new_token() {
    let data_dir = scratch_data_dir("rebind");
    let app = api_router(data_dir.clone(), Some(STANDARD.encode([0x42u8; 32])), None);
    let creator = test_key(1);
    let (_, room) = create_room(&app, &creator, 4, "creator").await;
    let room_id = room["room_id"].as_str().unwrap().to_owned();

    // A different key's first join takes the next open seat (201).
    let (status, first) = join(&app, &room_id, &test_key(2), None).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    assert_eq!(first["seat"], 2);

    // The creator rejoining is a rebind: same seat, 200, fresh token.
    let (status, rebinding) = join(&app, &room_id, &creator, None).await;
    assert_eq!(status, StatusCode::OK, "rebind of held seat");
    assert_eq!(rebinding["seat"], 1);
    assert_eq!(rebinding["label"], "creator");
    assert_ne!(
        rebinding["token"], room["token"],
        "rebind mints a fresh token"
    );

    let registry = SeatRegistry::new(&data_dir);
    assert_eq!(
        registry.verify_bearer(&room_id, first["token"].as_str().unwrap()),
        Some(2)
    );
    assert_eq!(
        registry.verify_bearer(&room_id, room["token"].as_str().unwrap()),
        Some(1)
    );
    assert_eq!(
        registry.verify_bearer(&room_id, rebinding["token"].as_str().unwrap()),
        Some(1)
    );
    assert_eq!(registry.verify_bearer(&room_id, "not-a-token"), None);
}

#[tokio::test]
async fn challenges_are_single_use_and_signatures_fail_closed() {
    let data_dir = scratch_data_dir("single-use");
    let app = api_router(data_dir.clone(), Some(STANDARD.encode([0x42u8; 32])), None);
    let key = test_key(1);
    let other = test_key(9);
    let (_, room) = create_room(&app, &key, 4, "creator").await;
    let room_id = room["room_id"].as_str().unwrap().to_owned();

    let (_, challenge) = send(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/challenge"),
        Some(json!({ "public_key_pem": spki_pem(&other) })),
    )
    .await;
    let bytes = STANDARD
        .decode(challenge["challenge"].as_str().unwrap())
        .unwrap();
    let signature = STANDARD.encode(other.sign(&bytes).to_bytes());
    let body = json!({
        "challenge_id": challenge["challenge_id"],
        "signature_base64": signature,
        "public_key_pem": spki_pem(&other),
    });
    let (status, joined) = send(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/join"),
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{joined}");

    let (status, replay) = send(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/join"),
        Some(body),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "replayed challenge must be consumed"
    );
    assert_eq!(replay["error"]["code"], "challenge_failed");

    let (_, challenge) = send(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/challenge"),
        Some(json!({ "public_key_pem": spki_pem(&other) })),
    )
    .await;
    let wrong_signature = STANDARD.encode(key.sign(b"unrelated").to_bytes());
    let body = json!({
        "challenge_id": challenge["challenge_id"],
        "signature_base64": wrong_signature,
        "public_key_pem": spki_pem(&other),
    });
    let (status, signed) = send(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/join"),
        Some(body),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "bad signature must fail closed"
    );
    assert_eq!(signed["error"]["code"], "challenge_failed");
}

#[tokio::test]
async fn token_refresh_re_signs_stored_key_and_keeps_old_tokens_valid() {
    let data_dir = scratch_data_dir("refresh");
    let app = api_router(data_dir.clone(), Some(STANDARD.encode([0x42u8; 32])), None);
    let key = test_key(1);
    let (_, room) = create_room(&app, &key, 4, "creator").await;
    let room_id = room["room_id"].as_str().unwrap().to_owned();
    let fingerprint = room["fingerprint"].as_str().unwrap().to_owned();
    let first_token = room["token"].as_str().unwrap().to_owned();

    let unknown = send(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/challenge"),
        Some(json!({ "fingerprint": "ab".repeat(32) })),
    )
    .await;
    assert_eq!(
        unknown.0,
        StatusCode::UNAUTHORIZED,
        "unknown fingerprint fails closed"
    );
    assert_eq!(unknown.1["error"]["code"], "challenge_failed");

    let (challenge_status, challenge) = send(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/challenge"),
        Some(json!({ "fingerprint": fingerprint })),
    )
    .await;
    assert_eq!(challenge_status, StatusCode::OK, "{challenge}");
    let bytes = STANDARD
        .decode(challenge["challenge"].as_str().unwrap())
        .unwrap();
    let signature = STANDARD.encode(key.sign(&bytes).to_bytes());
    let (status, refreshed) = send(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/auth/token"),
        Some(json!({
            "challenge_id": challenge["challenge_id"],
            "signature_base64": signature,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{refreshed}");
    let second_token = refreshed["token"].as_str().unwrap().to_owned();
    assert_ne!(second_token, first_token, "refresh mints a per-mint record");
    assert!(refreshed["token_expires_at"].as_str().is_some());

    // Old tokens stay valid until their own expiry; both digests persist.
    let registry = SeatRegistry::new(&data_dir);
    assert_eq!(registry.verify_bearer(&room_id, &first_token), Some(1));
    assert_eq!(registry.verify_bearer(&room_id, &second_token), Some(1));
    let raw = seats_json(&data_dir, &room_id);
    assert!(!raw.contains(&second_token), "raw token never at rest");
    assert!(raw.contains(&sha256_hex(second_token.as_bytes())));
    assert!(raw.contains(&sha256_hex(first_token.as_bytes())));

    // Refresh with a signature from the wrong key fails closed.
    let (_, challenge) = send(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/challenge"),
        Some(json!({ "fingerprint": fingerprint })),
    )
    .await;
    let bytes = STANDARD
        .decode(challenge["challenge"].as_str().unwrap())
        .unwrap();
    let wrong = STANDARD.encode(test_key(7).sign(&bytes).to_bytes());
    let (status, body) = send(
        &app,
        "POST",
        &format!("/v1/rooms/{room_id}/auth/token"),
        Some(json!({
            "challenge_id": challenge["challenge_id"],
            "signature_base64": wrong,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["code"], "challenge_failed");
}

#[tokio::test]
async fn label_collision_conflicts_and_overlong_labels_rejected() {
    let data_dir = scratch_data_dir("labels");
    let app = api_router(data_dir.clone(), Some(STANDARD.encode([0x42u8; 32])), None);
    let creator = test_key(1);
    let (_, room) = create_room(&app, &creator, 4, "creator").await;
    let room_id = room["room_id"].as_str().unwrap().to_owned();

    let (status, body) = join(&app, &room_id, &test_key(2), Some("creator")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "label_taken");

    let (status, body) = join(&app, &room_id, &test_key(2), Some(&"x".repeat(65))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_request");

    // Contract R4: labels double as vivi identity local-parts — charset is
    // letters, digits, dot, dash, underscore; a slash must be rejected.
    for bad in ["host/project", "seat 1", "læbel"] {
        let (status, body) = join(&app, &room_id, &test_key(2), Some(bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "label {bad:?}: {body}");
        assert_eq!(body["error"]["code"], "invalid_request");
    }

    let (status, body) = join(&app, &room_id, &test_key(2), Some("second")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["seat"], 2);
}
