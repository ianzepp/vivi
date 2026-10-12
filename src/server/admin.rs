//! `GET /v1/_health` (VC-5): operator health gated by `MODERATION_TOKEN`.
//! Reports backend mode, room count, and storage state only — no message
//! content. Disabled (route unregistered) when the token is unset.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use subtle::ConstantTimeEq;

use crate::AppState;
use crate::rooms::error_response;

/// Backend mode reported by the health endpoint.
const BACKEND_MODE: &str = "vivi-mailspaces";

pub async fn health_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(expected) = state.moderation_token.as_deref() else {
        // Defensive: the route is not registered without a token.
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let authorized = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|provided| provided.as_bytes().ct_eq(expected.as_bytes()).into());
    if !authorized {
        return error_response(
            StatusCode::FORBIDDEN,
            "moderator_unauthorized",
            "a valid moderation token is required",
        );
    }
    let projects = state.projects.clone();
    let (rooms, storage) = tokio::task::spawn_blocking(move || {
        let swept = projects.sweep_expired_chats();
        if swept > 0 {
            tracing::info!("swept {swept} expired chat projects");
        }
        (projects.live_chat_count(), projects.storage_state())
    })
    .await
    .unwrap_or((0, "error: health task failed".into()));
    Json(json!({
        "ok": true,
        "mode": BACKEND_MODE,
        "encryption": if state.store_cipher.is_some() { "aes-256-gcm" } else { "none" },
        "rooms": rooms,
        "storage": storage,
    }))
    .into_response()
}
