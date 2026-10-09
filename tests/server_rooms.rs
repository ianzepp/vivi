//! Integration tests for VC-2: room store + room endpoints over the real
//! vivi mailspace storage and the v1 router.

use std::path::{Path, PathBuf};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::SigningKey;
use serde_json::Value;
use tower::util::ServiceExt;

use vivi_server::api_router;
use vivi_server::rooms::{RoomError, RoomStore};

fn scratch_data_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vivi-server-vc2-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).expect("JSON body"))
}

fn post_rooms(body: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("POST").uri("/v1/rooms");
    if let Some(body) = body {
        builder = builder.header("content-type", "application/json");
        return builder.body(Body::from(body.to_owned())).unwrap();
    }
    builder.body(Body::empty()).unwrap()
}

fn test_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
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

/// Creator payload per the frozen contract (VC-3 shape): seats + label + key.
fn creator_payload(seats: u32, seed: u8, label: &str) -> String {
    format!(
        r#"{{"seats":{seats},"label":"{label}","public_key_pem":{:?}}}"#,
        spki_pem(&test_key(seed))
    )
}

#[tokio::test]
async fn two_rooms_get_distinct_ids_and_wal_mailspaces() {
    let data_dir = scratch_data_dir("two-rooms");
    let store = RoomStore::new(&data_dir);

    let first = store.create_room(2).unwrap();
    let second = store.create_room(2).unwrap();
    assert_ne!(first.room_id, second.room_id, "two mints must differ");

    for meta in [&first, &second] {
        let room_dir = data_dir.join("rooms").join(&meta.room_id);
        assert!(
            room_dir.join(".vivi").is_dir(),
            ".vivi mailspace dir missing for {}",
            meta.room_id
        );
        assert!(
            !room_dir.join("mailspace").exists(),
            "stray U1 mailspace dir for {}",
            meta.room_id
        );
        // Contract R2: exactly one sqlite store, under the .vivi root.
        let dbs = sqlite_files(&room_dir);
        assert_eq!(
            dbs.len(),
            1,
            "expected one sqlite under rooms/{}, got {dbs:?}",
            meta.room_id
        );
        assert_eq!(
            dbs[0],
            room_dir.join(".vivi").join("mail.sqlite"),
            "sqlite not at the .vivi root for {}",
            meta.room_id
        );
        assert_is_wal(&dbs[0]);
    }
    store
        .open_mailspace(&first.room_id)
        .expect("room mailspace reopens");
}

/// Every `*.sqlite` file under `dir`, recursively.
fn sqlite_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(sqlite_files(&path));
        } else if path.extension().is_some_and(|e| e == "sqlite") {
            out.push(path);
        }
    }
    out
}

/// SQLite persists the journal mode in the header: read/write version bytes
/// at offsets 18/19 are 2 for WAL databases.
fn assert_is_wal(db: &Path) {
    let header = std::fs::read(db).unwrap()[..20].to_vec();
    assert_eq!(
        &header[18..20],
        &[2, 2],
        "db {} not in WAL mode",
        db.display()
    );
}

#[tokio::test]
async fn create_room_handler_validates_capacity_and_mints_dir() {
    let data_dir = scratch_data_dir("create-handler");
    let app = api_router(data_dir.clone());

    let (status, body) = send(&app, post_rooms(Some(&creator_payload(6, 7, "creator-a")))).await;
    assert_eq!(status, StatusCode::CREATED);
    let room_id = body["room_id"].as_str().unwrap().to_owned();
    assert_eq!(room_id.split('-').count(), 5);
    assert_eq!(body["seat"], 1, "creator binds seat 1 (contract)");
    assert!(
        body["token"].as_str().is_some(),
        "creator token in response"
    );
    let room_dir = data_dir.join("rooms").join(&room_id);
    assert!(room_dir.join(".vivi").join("mail.sqlite").is_file());
    assert_eq!(sqlite_files(&room_dir).len(), 1);

    for bad in [r#"{"seats":9}"#, r#"{"seats":1}"#, r#"{"seats":0}"#] {
        let (status, body) = send(&app, post_rooms(Some(bad))).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "seats {bad} must be rejected"
        );
        assert_eq!(body["error"]["code"], "invalid_request");
    }
    let (status, body) = send(&app, post_rooms(None)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn room_metadata_endpoint_reports_and_distinguishes_errors() {
    let data_dir = scratch_data_dir("meta-handler");
    let app = api_router(data_dir.clone());

    let (_, created) = send(&app, post_rooms(Some(&creator_payload(4, 9, "creator-b")))).await;
    let room_id = created["room_id"].as_str().unwrap().to_owned();

    let req = Request::builder()
        .uri(format!("/v1/rooms/{room_id}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["room_id"], room_id.as_str());
    assert_eq!(body["capacity"], 4);

    let unknown = "22222-22222-22222-22222-22222";
    let req = Request::builder()
        .uri(format!("/v1/rooms/{unknown}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "room_not_found");

    let req = Request::builder()
        .uri("/v1/rooms/not-a-crockford-id!")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_room_id");
}

#[tokio::test]
async fn store_rejects_capacity_out_of_range() {
    let data_dir = scratch_data_dir("capacity");
    let store = RoomStore::new(&data_dir);
    assert!(matches!(store.create_room(1), Err(RoomError::Capacity(1))));
    assert!(matches!(store.create_room(9), Err(RoomError::Capacity(9))));
    assert!(
        !data_dir.join("rooms").join("anything").exists(),
        "no dir on rejection"
    );
}
