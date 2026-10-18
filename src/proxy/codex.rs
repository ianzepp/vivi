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
use super::forward;
use super::http_error::{self, request_id};
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
pub async fn handle(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    action: &Action,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(Response, Usage), VivariumError> {
    match action {
        Action::Health => Ok((health_response(provider), Usage::default())),
        Action::Models => Ok((
            forward::json_ok(models_body(&provider.models)),
            Usage::default(),
        )),
        Action::ChatCompletions => chat_shim(client, provider, headers, body).await,
        Action::Responses => responses(client, provider, method, headers, body).await,
        Action::Usage => Ok(usage_stub()),
        other => {
            let id = request_id();
            tracing::info!(action = other.as_str(), request_id = %id, "codex path not supported");
            Ok((
                http_error::json_error(
                    StatusCode::NOT_FOUND,
                    "unsupported",
                    "codex native adapter serves /responses and /chat/completions",
                    &id,
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
) -> Result<(Response, Usage), VivariumError> {
    let id = request_id();
    let tokens = match codex_auth::ensure_fresh(client, provider).await {
        Ok(tokens) => tokens,
        Err(error) => {
            tracing::warn!(%error, request_id = %id, "codex credentials unusable");
            return Ok((http_error::credential_failed(&id), Usage::default()));
        }
    };
    let sanitized = sanitize_body(&body);
    stream_responses(
        client, provider, method, headers, sanitized, tokens, id, false,
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
    builder = forward::copy_headers(builder, &headers);
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
    let headers = upstream.headers().clone();
    let mut rebuild = Rebuild::default();
    let mut usage = Usage::default();
    let stream = sse::map_events(
        upstream.bytes_stream().map_err(|error| error.to_string()),
        move |event| {
            if let Some(data) = event.data()
                && let Some(found) = super::usage::from_sse_data(&data)
            {
                usage.merge(found);
            }
            codex_sse::filter(event, &mut rebuild)
        },
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
) -> Result<(Response, Usage), VivariumError> {
    let id = request_id();
    let chat: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
    let model = chat
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("gpt-5")
        .to_string();
    let responses_body =
        Bytes::from(serde_json::to_vec(&codex_chat::chat_to_responses(&chat)).unwrap_or_default());
    let tokens = match codex_auth::ensure_fresh(client, provider).await {
        Ok(tokens) => tokens,
        Err(error) => {
            tracing::warn!(%error, request_id = %id, "codex credentials unusable");
            return Ok((http_error::credential_failed(&id), Usage::default()));
        }
    };
    let events = collect_stream(client, provider, headers, responses_body, tokens, &id).await?;
    let text = codex_chat::text_from_events(&events);
    let payload = codex_chat::responses_to_chat(&model, &text, &id);
    Ok((
        (StatusCode::OK, axum::Json(payload)).into_response(),
        Usage::default(),
    ))
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
    builder = forward::copy_headers(builder, &headers);
    for (name, value) in &extra {
        builder = builder.header(name.as_str(), value);
    }
    let upstream = builder.body(body).send().await.map_err(|error| {
        tracing::warn!(%error, request_id = %id, "codex chat upstream failed");
        VivariumError::Message("upstream request failed".into())
    })?;
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

fn sanitize_body(body: &[u8]) -> Bytes {
    match serde_json::from_slice::<Value>(body) {
        Ok(value) => Bytes::from(
            serde_json::to_vec(&codex_chat::sanitize_responses(value)).unwrap_or_default(),
        ),
        Err(_) => Bytes::from(body.to_vec()),
    }
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

fn health_response(provider: &ProviderConfig) -> Response {
    let ok = codex_auth::path_if_present(provider).is_some();
    forward::json_ok(json!({ "ok": ok, "provider": "codex" }))
}

fn models_body(models: &[String]) -> Value {
    let data: Vec<Value> = models
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
