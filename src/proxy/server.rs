//! Axum front door: guards, path routing, provider dispatch.

use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use bytes::Bytes;

use super::access::{self, AccessEvent, AccessLog};
use super::cmd;
use super::config::{BackendKind, ProviderConfig, ProxyConfig};
use super::forward;
use super::guard::Guard;
use super::health::{self, HealthTable};
use super::http_error::{self, request_id};
use super::native;
use super::route::{self, Action, Route};
use super::tap;
use super::usage::{self, Usage};
use crate::VivariumError;

/// Shared listener state.
pub struct AppState {
    pub config: ProxyConfig,
    pub client: reqwest::Client,
    pub log: AccessLog,
    pub health: Arc<HealthTable>,
    pub bearer: Option<String>,
}

/// Build the axum router.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(aggregate_health))
        .fallback(any(dispatch))
        .with_state(state)
}

async fn aggregate_health(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(deny) = deny_request(&state, &Method::GET, &headers) {
        return deny;
    }
    let names = provider_kinds(&state.config);
    refresh_upstream_health(&state).await;
    let rows = state.health.snapshot(&names);
    let status = if health::aggregate_ok(&rows) {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, axum::Json(health::aggregate_json(&rows))).into_response()
}

async fn dispatch(
    State(state): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let id = request_id();
    let started = access::start();
    if let Some(deny) = deny_request(&state, &method, &headers) {
        return deny;
    }
    match route::parse(uri.path()) {
        Some(Route::AggregateHealth) => aggregate_health(State(state), headers).await,
        Some(Route::Provider { name, action }) => {
            routed(
                &state, &name, action, method, uri, headers, body, id, started,
            )
            .await
        }
        None => http_error::json_error(StatusCode::NOT_FOUND, "not_found", "not found", &id),
    }
}

#[allow(clippy::too_many_arguments)]
async fn routed(
    state: &AppState,
    name: &str,
    action: Action,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    id: String,
    started: Instant,
) -> Response {
    let Some(provider) = state.config.providers.get(name).cloned() else {
        return http_error::unknown_provider(&id);
    };
    if matches!(action, Action::Health) && provider.kind != BackendKind::Native {
        return backend_health(state, name, &provider).await;
    }
    let model = usage::model_from_body(&body);
    let path = uri.path().to_string();
    let result = dispatch_backend(
        state,
        &provider,
        &action,
        method,
        path,
        uri.query(),
        headers,
        body,
        &id,
    )
    .await;
    finish(state, name, &action, model, &id, started, result)
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_backend(
    state: &AppState,
    provider: &ProviderConfig,
    action: &Action,
    method: Method,
    path: String,
    query: Option<&str>,
    headers: HeaderMap,
    body: Bytes,
    id: &str,
) -> Result<(Response, Usage), VivariumError> {
    match provider.kind {
        BackendKind::Native => {
            native::handle(
                &state.client,
                provider,
                action,
                method,
                query,
                headers,
                body,
                id,
            )
            .await
        }
        BackendKind::Upstream | BackendKind::Child => {
            let forwarded = route::forward_path(&path);
            let backend = provider.backend.as_deref().unwrap_or("http://127.0.0.1");
            let url = forward::join_url(backend, &forwarded, query);
            forward::proxy_request(
                &state.client,
                method,
                url,
                headers,
                body,
                HeaderMap::new(),
                id,
            )
            .await
        }
    }
}

fn finish(
    state: &AppState,
    name: &str,
    action: &Action,
    model: Option<String>,
    id: &str,
    started: Instant,
    result: Result<(Response, Usage), VivariumError>,
) -> Response {
    match result {
        Ok((response, usage)) => {
            finish_ok(state, name, action, model, id, started, response, usage)
        }
        Err(error) => {
            tracing::warn!(provider = %name, %error, request_id = %id, "provider dispatch failed");
            let event = access_event(name, action, model, id, started, 502, &Usage::default());
            state.log.write(&event);
            http_error::upstream_failed(id)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_ok(
    state: &AppState,
    name: &str,
    action: &Action,
    model: Option<String>,
    id: &str,
    started: Instant,
    response: Response,
    usage: Usage,
) -> Response {
    let event = access_event(
        name,
        action,
        model,
        id,
        started,
        response.status().as_u16(),
        &usage,
    );
    tap::wrap_logged(response, state.log.clone(), event, started, usage)
}

#[allow(clippy::too_many_arguments)]
fn access_event(
    name: &str,
    action: &Action,
    model: Option<String>,
    id: &str,
    started: Instant,
    status: u16,
    usage: &Usage,
) -> AccessEvent {
    let ttfb_ms = access::elapsed_ms(started);
    AccessEvent {
        ts: access::now_rfc3339(),
        id: id.to_string(),
        provider: name.to_string(),
        client: None,
        path: action.as_str().to_string(),
        model,
        status,
        ttfb_ms,
        total_ms: ttfb_ms,
        input_tokens: usage.input,
        output_tokens: usage.output,
        cached_tokens: usage.cached,
        upstream_id: usage.upstream_id.clone(),
    }
}

fn deny_request(state: &AppState, method: &Method, headers: &HeaderMap) -> Option<Response> {
    let deny = guard_of(state).check(method, headers, state.bearer.as_deref())?;
    Some(http_error::json_error(
        StatusCode::from_u16(deny.status()).unwrap_or(StatusCode::FORBIDDEN),
        deny.code(),
        deny.message(),
        &request_id(),
    ))
}

fn guard_of(state: &AppState) -> Guard {
    Guard {
        listen: state.config.listen,
        allow_remote: state.config.allow_remote,
        require_bearer: state.bearer.is_some(),
    }
}

fn provider_kinds(config: &ProxyConfig) -> Vec<(String, String)> {
    config
        .providers
        .iter()
        .map(|(name, provider)| (name.clone(), kind_name(provider.kind).into()))
        .collect()
}

fn kind_name(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Native => "native",
        BackendKind::Child => "child",
        BackendKind::Upstream => "upstream",
    }
}

async fn refresh_upstream_health(state: &AppState) {
    for (name, provider) in &state.config.providers {
        match provider.kind {
            BackendKind::Upstream => probe_upstream(state, name, provider).await,
            BackendKind::Native => record_native(state, name, provider),
            BackendKind::Child => {}
        }
    }
}

async fn probe_upstream(state: &AppState, name: &str, provider: &ProviderConfig) {
    let Some(backend) = provider.backend.as_deref() else {
        return;
    };
    let url = forward::join_url(backend, provider.health_path(), None);
    match forward::probe(&state.client, &url).await {
        Ok(()) => state.health.record(name, "upstream", true, None),
        Err(error) => state.health.record(name, "upstream", false, Some(error)),
    }
}

fn record_native(state: &AppState, name: &str, provider: &ProviderConfig) {
    let status = native::health_status(provider);
    let error = match status {
        "ok" => None,
        "credential_dead" => Some(super::dead::DEAD_MESSAGE.into()),
        _ => Some("credentials missing".into()),
    };
    state.health.record_status(name, "native", status, error);
}

async fn backend_health(state: &AppState, name: &str, provider: &ProviderConfig) -> Response {
    let Some(backend) = provider.backend.as_deref() else {
        return http_error::upstream_failed(&request_id());
    };
    let url = forward::join_url(backend, provider.health_path(), None);
    match forward::probe(&state.client, &url).await {
        Ok(()) => {
            state
                .health
                .record(name, kind_name(provider.kind), true, None);
            forward::json_ok(serde_json::json!({ "ok": true, "provider": name, "status": "ok" }))
        }
        Err(error) => {
            state
                .health
                .record(name, kind_name(provider.kind), false, Some(error));
            (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(serde_json::json!({ "ok": false, "provider": name, "status": "down" })),
            )
                .into_response()
        }
    }
}

/// Load the optional inbound bearer via `auth_token_cmd`.
///
/// # Errors
///
/// Returns a configuration error when the command fails.
pub fn load_bearer(config: &ProxyConfig) -> Result<Option<String>, VivariumError> {
    match config.auth_token_cmd.as_deref() {
        Some(command) => Ok(Some(cmd::run_secret_cmd("auth_token_cmd", command)?)),
        None => Ok(None),
    }
}
