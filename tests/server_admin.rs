//! Integration tests for VC-5: `GET /v1/_health` gated by
//! `MODERATION_TOKEN` — authorized 200 with mode/rooms/storage, 403
//! `moderator_unauthorized` otherwise, and the route disabled (404) when
//! the token is unset. Helper style follows tests/messages.rs.

use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;
use tower::util::ServiceExt;

use vivi_server::api_router;

fn scratch_data_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-vc5-health-{}-{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn store_key() -> String {
    STANDARD.encode([0x42u8; 32])
}

async fn get_health(app: &Router, token: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("GET").uri("/v1/_health");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let req = builder.body(Body::empty()).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    // A disabled route answers 404 with an empty body.
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("JSON body")
    };
    (status, body)
}

#[tokio::test]
async fn health_with_moderation_token_reports_mode_rooms_storage() {
    let data_dir = scratch_data_dir("ok");
    let app = api_router(
        data_dir.clone(),
        Some(store_key()),
        Some("mod-token".into()),
    );

    let (status, body) = get_health(&app, Some("mod-token")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(body["mode"], "vivi-mailspaces");
    assert_eq!(body["encryption"], "aes-256-gcm");
    assert!(body["rooms"].is_u64(), "room count present: {body}");
    assert_eq!(body["storage"], "ok");
    assert!(body.get("messages").is_none(), "no message content");
}

#[tokio::test]
async fn health_without_or_with_wrong_token_is_403_moderator_unauthorized() {
    let data_dir = scratch_data_dir("forbidden");
    let app = api_router(
        data_dir.clone(),
        Some(store_key()),
        Some("mod-token".into()),
    );

    let (status, body) = get_health(&app, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "moderator_unauthorized");

    let (status, body) = get_health(&app, Some("wrong-token")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "moderator_unauthorized");
}

#[tokio::test]
async fn health_is_disabled_when_moderation_token_unset() {
    let data_dir = scratch_data_dir("disabled");
    let app = api_router(data_dir.clone(), Some(store_key()), None);

    let (status, _) = get_health(&app, Some("mod-token")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "route unregistered");
}
