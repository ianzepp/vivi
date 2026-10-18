//! Native `chatgpt-codex` adapter.

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt};
use serde_json::{Value, json};

use super::codex_auth::{self, Tokens};
use super::codex_chat;
use super::codex_sse::{self, Rebuild};
use super::config::ProviderConfig;
use super::dead;
use super::forward;
use super::http_error;
use super::route::Action;
use super::sse::{self, Event};
use super::usage::Usage;
use crate::VivariumError;

const DEFAULT_UPSTREAM: &str = "https://chatgpt.com/backend-api/codex/responses";

/// Handle one inbound Codex request.
///
/// # Errors
///
/// Returns a configuration error when the adapter is misconfigured.
#[allow(clippy::too_many_arguments)]
pub async fn handle(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    action: &Action,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
    request_id: &str,
) -> Result<(Response, Usage), VivariumError> {
    match action {
        Action::Health => Ok((health_response(provider), Usage::default())),
        Action::Models => Ok((
            forward::json_ok(models_body(&provider.models)),
            Usage::default(),
        )),
        Action::ChatCompletions => chat_shim(client, provider, headers, body, request_id).await,
        Action::Responses => responses(client, provider, method, headers, body, request_id).await,
        Action::Usage => Ok(usage_stub()),
        other => {
            tracing::info!(action = other.as_str(), request_id = %request_id, "codex path not supported");
            Ok((
                http_error::json_error(
                    StatusCode::NOT_FOUND,
                    "unsupported",
                    "codex native adapter serves /responses and /chat/completions",
                    request_id,
                ),
                Usage::default(),
            ))
        }
    }
}

async fn responses(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
    request_id: &str,
) -> Result<(Response, Usage), VivariumError> {
    let tokens = match fresh_tokens(client, provider, request_id).await {
        Ok(tokens) => tokens,
        Err(response) => return Ok((response, Usage::default())),
    };
    let Ok(sanitized) = sanitize_body(&body) else {
        return Ok((http_error::invalid_json(request_id), Usage::default()));
    };
    stream_responses(
        client,
        provider,
        method,
        headers,
        sanitized,
        tokens,
        request_id.to_string(),
        false,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn stream_responses(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
    tokens: Tokens,
    id: String,
    _retried: bool,
) -> Result<(Response, Usage), VivariumError> {
    let url = provider
        .upstream
        .as_deref()
        .unwrap_or(DEFAULT_UPSTREAM)
        .to_string();
    let extra = outbound_headers(&headers, &tokens);
    let mut builder = client.request(method, url);
    builder = forward::copy_headers_rebuilt(builder, &headers);
    for (name, value) in &extra {
        builder = builder.header(name.as_str(), value);
    }
    builder = builder.body(body);
    let upstream = match builder.send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, request_id = %id, "codex upstream failed");
            return Ok((http_error::upstream_failed(&id), Usage::default()));
        }
    };
    if upstream.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Ok((http_error::credential_failed(&id), Usage::default()));
    }
    Ok(stream_filtered(upstream, &id))
}

fn stream_filtered(upstream: reqwest::Response, request_id: &str) -> (Response, Usage) {
    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    if !status.is_success() {
        return forward::to_axum_response(upstream, request_id);
    }
    let headers = upstream.headers().clone();
    let mut rebuild = Rebuild::default();
    let stream = sse::map_events(
        upstream.bytes_stream().map_err(|error| error.to_string()),
        move |event| codex_sse::filter(event, &mut rebuild),
    );
    let mut response = Response::builder()
        .status(status)
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| http_error::upstream_failed(request_id));
    copy_sse_headers(response.headers_mut(), &headers, request_id);
    (response, Usage::default())
}

fn copy_sse_headers(dest: &mut HeaderMap, src: &HeaderMap, request_id: &str) {
    dest.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    dest.insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    if let Some(value) = src.get("x-request-id") {
        dest.insert("x-request-id", value.clone());
    }
    if let Ok(value) = HeaderValue::from_str(request_id) {
        dest.insert("x-vivi-request-id", value);
    }
}

async fn chat_shim(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    headers: HeaderMap,
    body: Bytes,
    request_id: &str,
) -> Result<(Response, Usage), VivariumError> {
    let chat: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return Ok((http_error::invalid_json(request_id), Usage::default())),
    };
    let model = chat
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("gpt-6.1-sol")
        .to_string();
    let responses_body =
        Bytes::from(serde_json::to_vec(&codex_chat::chat_to_responses(&chat)).unwrap_or_default());
    let tokens = match fresh_tokens(client, provider, request_id).await {
        Ok(tokens) => tokens,
        Err(response) => return Ok((response, Usage::default())),
    };
    let events = collect_stream(
        client,
        provider,
        headers,
        responses_body,
        tokens,
        request_id,
    )
    .await?;
    let text = codex_chat::text_from_events(&events);
    let payload = codex_chat::responses_to_chat(&model, &text, request_id);
    let mut response = (StatusCode::OK, axum::Json(payload)).into_response();
    http_error::attach_request_id(&mut response, request_id);
    Ok((response, Usage::default()))
}

async fn collect_stream(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    headers: HeaderMap,
    body: Bytes,
    tokens: Tokens,
    id: &str,
) -> Result<Vec<Event>, VivariumError> {
    let url = provider.upstream.as_deref().unwrap_or(DEFAULT_UPSTREAM);
    let extra = outbound_headers(&headers, &tokens);
    let mut builder = client.post(url);
    builder = forward::copy_headers_rebuilt(builder, &headers);
    for (name, value) in &extra {
        builder = builder.header(name.as_str(), value);
    }
    let upstream = builder.body(body).send().await.map_err(|error| {
        tracing::warn!(%error, request_id = %id, "codex chat upstream failed");
        VivariumError::Message("upstream request failed".into())
    })?;
    if !upstream.status().is_success() {
        return Err(VivariumError::Message("codex chat upstream failed".into()));
    }
    let mut rebuild = Rebuild::default();
    let mut events = Vec::new();
    let mut stream = sse::map_events(
        upstream.bytes_stream().map_err(|error| error.to_string()),
        move |event| codex_sse::filter(event, &mut rebuild),
    );
    while let Some(chunk) = stream.next().await {
        if let Ok(bytes) = chunk
            && let Ok(text) = String::from_utf8(bytes.to_vec())
        {
            events.push(Event { raw: text });
        }
    }
    Ok(events)
}

fn sanitize_body(body: &[u8]) -> Result<Bytes, ()> {
    let value = serde_json::from_slice::<Value>(body).map_err(|_| ())?;
    Ok(Bytes::from(
        serde_json::to_vec(&codex_chat::sanitize_responses(value)).unwrap_or_default(),
    ))
}

fn outbound_headers(incoming: &HeaderMap, tokens: &Tokens) -> HeaderMap {
    let mut headers = HeaderMap::new();
    insert(
        &mut headers,
        "authorization",
        &format!("Bearer {}", tokens.access_token),
    );
    if let Some(account) = tokens.account_id.as_deref() {
        insert(&mut headers, "chatgpt-account-id", account);
    }
    insert(&mut headers, "openai-beta", "responses=experimental");
    insert(&mut headers, "accept", "text/event-stream");
    let originator = incoming
        .get("originator")
        .and_then(|value| {
            #[allow(clippy::manual_ok_err)]
            match value.to_str() {
                Ok(text) => Some(text),
                Err(_) => None,
            }
        })
        .unwrap_or("grok");
    insert(&mut headers, "originator", originator);
    headers
}

fn insert(headers: &mut HeaderMap, name: &'static str, value: &str) {
    if let (Ok(name), Ok(value)) = (
        axum::http::HeaderName::from_bytes(name.as_bytes()),
        HeaderValue::from_str(value),
    ) {
        headers.insert(name, value);
    }
}

async fn fresh_tokens(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    request_id: &str,
) -> Result<Tokens, Response> {
    if let Some(response) = refuse_if_dead(provider, request_id) {
        return Err(response);
    }
    match codex_auth::ensure_fresh(client, provider).await {
        Ok(tokens) => Ok(tokens),
        Err(error) => {
            tracing::warn!(%error, request_id = %request_id, "codex credentials unusable");
            Err(refresh_error(provider, request_id))
        }
    }
}

fn refuse_if_dead(provider: &ProviderConfig, request_id: &str) -> Option<Response> {
    let Ok(path) = codex_auth::credential_path(provider) else {
        return None;
    };
    if !dead::is_dead(&path) {
        return None;
    }
    tracing::debug!(
        request_id = %request_id,
        "rejecting request for a dead codex credential"
    );
    Some(http_error::credential_dead(request_id))
}

fn refresh_error(provider: &ProviderConfig, request_id: &str) -> Response {
    match codex_auth::credential_path(provider) {
        Ok(path) if dead::is_dead(&path) => http_error::credential_dead(request_id),
        _ => http_error::credential_failed(request_id),
    }
}

fn health_response(provider: &ProviderConfig) -> Response {
    let status = health_status(provider);
    let body = json!({ "ok": status == "ok", "provider": "codex", "status": status });
    if status == "ok" || status == "missing" {
        forward::json_ok(body)
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, axum::Json(body)).into_response()
    }
}

/// Health label for `/health` and `vivi proxy providers`.
#[must_use]
pub fn health_status(provider: &ProviderConfig) -> &'static str {
    match codex_auth::credential_path(provider) {
        Ok(path) if dead::is_dead(&path) => "credential_dead",
        Ok(_) => "ok",
        Err(_) => "missing",
    }
}

const DEFAULT_MODELS: &[&str] = &["gpt-6-luna", "gpt-6.1-sol"];

fn models_body(models: &[String]) -> Value {
    let listed: Vec<String> = if models.is_empty() {
        DEFAULT_MODELS
            .iter()
            .map(|model| (*model).to_string())
            .collect()
    } else {
        models.to_vec()
    };
    let data: Vec<Value> = listed
        .iter()
        .map(|model| json!({ "id": model, "object": "model" }))
        .collect();
    json!({ "object": "list", "data": data })
}

fn usage_stub() -> (Response, Usage) {
    (
        forward::json_ok(json!({
            "object": "usage",
            "redacted": true,
            "note": "plan usage is not fetched by the native adapter; see the access log"
        })),
        Usage::default(),
    )
}

/// Expiry label for `vivi proxy providers`.
#[must_use]
pub fn credential_state(provider: &ProviderConfig) -> String {
    codex_auth::credential_state(provider)
}
