//! Fixed-message HTTP errors plus a request id. Never echo secrets.

use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use rand_core::{OsRng, RngCore};

/// Allocate a 16-hex request id.
#[must_use]
pub fn request_id() -> String {
    let mut bytes = [0u8; 8];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// JSON error body used for every front-door failure.
#[must_use]
pub fn json_error(status: StatusCode, code: &str, message: &str, request_id: &str) -> Response {
    let body = json!({
        "error": {
            "message": message,
            "type": "proxy_error",
            "code": code,
            "request_id": request_id,
        }
    });
    let mut response = (status, axum::Json(body)).into_response();
    attach_request_id(&mut response, request_id);
    response
}

/// Stamp `x-vivi-request-id` on a response that already has a body.
pub fn attach_request_id(response: &mut Response, request_id: &str) {
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert("x-vivi-request-id", value);
    }
}

/// Map a credential or refresh failure to a redacted 502.
#[must_use]
pub fn credential_failed(request_id: &str) -> Response {
    json_error(
        StatusCode::BAD_GATEWAY,
        "credential_error",
        "credential refresh failed",
        request_id,
    )
}

/// Map an upstream transport failure to a redacted 502.
#[must_use]
pub fn upstream_failed(request_id: &str) -> Response {
    json_error(
        StatusCode::BAD_GATEWAY,
        "upstream_error",
        "upstream request failed",
        request_id,
    )
}

/// Local refusal of a body that is not JSON.
#[must_use]
pub fn invalid_json(request_id: &str) -> Response {
    json_error(
        StatusCode::BAD_REQUEST,
        "invalid_json",
        "request body must be JSON",
        request_id,
    )
}

/// Provider is not in `proxy.toml`.
#[must_use]
pub fn unknown_provider(request_id: &str) -> Response {
    json_error(
        StatusCode::NOT_FOUND,
        "unknown_provider",
        "unknown provider",
        request_id,
    )
}
