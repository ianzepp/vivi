//! Reverse-proxy a request to an upstream or child HTTP server.

use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::TryStreamExt;

use super::http_error;
use super::sse;
use super::usage::Usage;
use crate::VivariumError;

/// Headers that must not be copied hop-to-hop.
const HOP: [&str; 9] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
    "host",
];

/// Build the URL `backend` + `path` + optional `query`.
#[must_use]
pub fn join_url(backend: &str, path: &str, query: Option<&str>) -> String {
    let base = backend.trim_end_matches('/');
    let suffix = if path.is_empty() || path == "/" {
        String::new()
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    match query {
        Some(query) if !query.is_empty() => format!("{base}{suffix}?{query}"),
        _ => format!("{base}{suffix}"),
    }
}

/// Copy inbound headers onto a reqwest builder, minus hop-by-hop and auth.
pub fn copy_headers(
    builder: reqwest::RequestBuilder,
    headers: &HeaderMap,
) -> reqwest::RequestBuilder {
    copy_headers_filtered(builder, headers, false)
}

/// Like [`copy_headers`], but drop length and encoding so a rebuilt body
/// is not sent with the client's original `Content-Length`.
pub fn copy_headers_rebuilt(
    builder: reqwest::RequestBuilder,
    headers: &HeaderMap,
) -> reqwest::RequestBuilder {
    copy_headers_filtered(builder, headers, true)
}

fn copy_headers_filtered(
    mut builder: reqwest::RequestBuilder,
    headers: &HeaderMap,
    rebuilt: bool,
) -> reqwest::RequestBuilder {
    for (name, value) in headers {
        if skip_header(name, rebuilt) {
            continue;
        }
        if let Ok(value) = value.to_str() {
            builder = builder.header(name.as_str(), value);
        }
    }
    builder
}

fn skip_header(name: &HeaderName, rebuilt: bool) -> bool {
    is_hop(name) || name == axum::http::header::AUTHORIZATION || (rebuilt && is_rebuilt_skip(name))
}

fn is_hop(name: &HeaderName) -> bool {
    HOP.iter()
        .any(|hop| name.as_str().eq_ignore_ascii_case(hop))
}

fn is_rebuilt_skip(name: &HeaderName) -> bool {
    name.as_str().eq_ignore_ascii_case("content-length")
        || name.as_str().eq_ignore_ascii_case("accept-encoding")
}

/// Forward one request and stream the response body, flushing SSE events.
///
/// # Errors
///
/// Returns a redacted 502 when the upstream cannot be reached.
pub async fn proxy_request(
    client: &reqwest::Client,
    method: Method,
    url: String,
    headers: HeaderMap,
    body: Bytes,
    extra: HeaderMap,
    request_id: &str,
) -> Result<(Response, Usage), VivariumError> {
    let id = request_id.to_string();
    let mut builder = client.request(method, url);
    builder = copy_headers(builder, &headers);
    for (name, value) in &extra {
        builder = builder.header(name.as_str(), value);
    }
    if !body.is_empty() {
        builder = builder.body(body);
    }
    let upstream = match builder.send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, request_id = %id, "upstream transport failed");
            return Ok((http_error::upstream_failed(&id), Usage::default()));
        }
    };
    Ok(to_axum_response(upstream, &id))
}

/// Turn an upstream `reqwest` response into an axum response.
///
/// Usage is harvested later from the streamed body.
#[must_use]
pub fn to_axum_response(upstream: reqwest::Response, request_id: &str) -> (Response, Usage) {
    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let headers = upstream.headers().clone();
    let stream = sse::passthrough(upstream.bytes_stream().map_err(|error| error.to_string()));
    let mut response = Response::builder()
        .status(status)
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| http_error::upstream_failed(request_id));
    copy_response_headers(response.headers_mut(), &headers);
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert("x-vivi-request-id", value);
    }
    (response, Usage::default())
}

fn copy_response_headers(dest: &mut HeaderMap, src: &HeaderMap) {
    for (name, value) in src {
        if is_hop(name) {
            continue;
        }
        dest.append(name, value.clone());
    }
}

/// GET a backend health URL. Returns whether it looks healthy.
///
/// # Errors
///
/// Returns a short, redacted reason when the probe fails.
pub async fn probe(client: &reqwest::Client, url: &str) -> Result<(), String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("health probe failed: {error}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("health probe HTTP {}", response.status().as_u16()))
    }
}

/// JSON response helper for local `/models` and `/health`.
#[must_use]
pub fn json_ok(value: serde_json::Value) -> Response {
    (StatusCode::OK, axum::Json(value)).into_response()
}
