//! Native Kimi Code adapter: OAuth file or `key_cmd`, header injection, passthrough.
//!
//! The `openai-passthrough` adapter shares the request path but none of the
//! Kimi credential machinery: no home-directory credential fallbacks, no
//! OAuth refresh, no dead mark. Its bearer is a `key_cmd` or its own
//! configured credential file, and a 401 goes back to the client unchanged.

use std::path::PathBuf;

use axum::http::{HeaderMap, HeaderValue, Method};
use axum::response::Response;
use bytes::Bytes;
use serde_json::{Value, json};

use super::cmd;
use super::config::ProviderConfig;
use super::credentials;
use super::dead::{self, RefreshClass};
use super::forward;
use super::http_error;
use super::oauth;
use super::paths::expand_user_str;
use super::route::Action;
use super::usage::Usage;
use crate::VivariumError;

const DEFAULT_UPSTREAM: &str = "https://api.kimi.com/coding/v1";
const DEFAULT_TOKEN_URL: &str = "https://auth.kimi.com/api/oauth/token";
const DEFAULT_CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const DEFAULT_CLIENT_VERSION: &str = "1.15.0";
const KIMI_ADAPTER: &str = "kimi-coding";
const PASSTHROUGH_ADAPTER: &str = "openai-passthrough";

/// True for the Kimi Code adapter, the only one with an OAuth credential
/// that can be refreshed, refused, and marked dead.
fn is_kimi(provider: &ProviderConfig) -> bool {
    provider.adapter.as_deref() == Some(KIMI_ADAPTER)
}

/// Handle one inbound Kimi request.
///
/// # Errors
///
/// Returns a configuration error when credentials cannot be read. Token
/// endpoint bodies are never included in the error text.
#[allow(clippy::too_many_arguments)]
pub async fn handle(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    action: &Action,
    method: Method,
    query: Option<&str>,
    headers: HeaderMap,
    body: Bytes,
    request_id: &str,
) -> Result<(Response, Usage), VivariumError> {
    if matches!(action, Action::Health) {
        return Ok((health_response(provider), Usage::default()));
    }
    if matches!(action, Action::Models) && !provider.models.is_empty() {
        return Ok((
            forward::json_ok(forward::models_body(&provider.models)),
            Usage::default(),
        ));
    }
    if let Some(response) = refuse_if_dead(provider, request_id) {
        return Ok((response, Usage::default()));
    }
    let first = dispatch(
        client,
        provider,
        action,
        &method,
        query,
        &headers,
        body.clone(),
        request_id,
        false,
    )
    .await;
    retry_after_401(
        first, client, provider, action, method, query, headers, body, request_id,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn retry_after_401(
    first: Result<(Response, Usage), VivariumError>,
    client: &reqwest::Client,
    provider: &ProviderConfig,
    action: &Action,
    method: Method,
    query: Option<&str>,
    headers: HeaderMap,
    body: Bytes,
    request_id: &str,
) -> Result<(Response, Usage), VivariumError> {
    let Ok((response, usage)) = first else {
        return first;
    };
    if !is_kimi(provider) || response.status() != axum::http::StatusCode::UNAUTHORIZED {
        return Ok((response, usage));
    }
    match refresh(client, provider).await {
        Ok(RefreshClass::Ok) => {
            dispatch(
                client, provider, action, &method, query, &headers, body, request_id, true,
            )
            .await
        }
        Ok(RefreshClass::Refused) => Ok((http_error::credential_dead(request_id), usage)),
        Ok(RefreshClass::Transient) | Err(_) => {
            tracing::warn!(request_id = %request_id, "kimi refresh after 401 failed");
            Ok((http_error::credential_failed(request_id), usage))
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    action: &Action,
    method: &Method,
    query: Option<&str>,
    headers: &HeaderMap,
    body: Bytes,
    request_id: &str,
    _retried: bool,
) -> Result<(Response, Usage), VivariumError> {
    let token = match bearer(provider) {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(%error, request_id = %request_id, "kimi credentials unreadable");
            return Ok((http_error::credential_failed(request_id), Usage::default()));
        }
    };
    let url = upstream_url(provider, action, query);
    let mut extra = identity_headers(provider, &token);
    if method != Method::GET
        && let Some((name, value)) = super::session::header_to_add(provider, headers, &body)
    {
        extra.insert(name, value);
    }
    forward::proxy_request(
        client,
        method.clone(),
        url,
        headers.clone(),
        body,
        extra,
        request_id,
    )
    .await
}

fn bearer(provider: &ProviderConfig) -> Result<String, VivariumError> {
    if let Some(cmd) = provider.key_cmd.as_deref() {
        return cmd::run_secret_cmd("kimi key_cmd", cmd);
    }
    let path = credential_path(provider)?;
    let value = credentials::read_json(&path)?;
    value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| VivariumError::Config("kimi credentials missing access_token".into()))
}

fn credential_path(provider: &ProviderConfig) -> Result<PathBuf, VivariumError> {
    credentials::first_existing(&credential_candidates(provider))
}

/// Credential files tried in order. The Kimi home-directory defaults belong
/// to the Kimi adapter alone; another provider uses only what it configures.
pub(crate) fn credential_candidates(provider: &ProviderConfig) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = provider.credentials.as_deref() {
        candidates.push(expand_user_str(path));
    }
    if let Some(path) = provider.fallback_credentials.as_deref() {
        candidates.push(expand_user_str(path));
    }
    if is_kimi(provider) {
        candidates.push(expand_user_str("~/.kimi-code/credentials/kimi-code.json"));
        candidates.push(expand_user_str("~/.kimi/credentials/kimi-code.json"));
    }
    candidates
}

/// Resolve an existing Kimi credential path without reading its contents.
#[must_use]
pub fn credential_path_if_present(provider: &ProviderConfig) -> Option<PathBuf> {
    match credential_path(provider) {
        Ok(path) => Some(path),
        Err(_) => None,
    }
}

fn upstream_url(provider: &ProviderConfig, action: &Action, query: Option<&str>) -> String {
    let base = provider.upstream.as_deref().unwrap_or(DEFAULT_UPSTREAM);
    forward::upstream_url(base, action, query)
}

pub(crate) fn identity_headers(provider: &ProviderConfig, token: &str) -> HeaderMap {
    let version = provider
        .client_version
        .as_deref()
        .unwrap_or(DEFAULT_CLIENT_VERSION);
    let mut headers = HeaderMap::new();
    insert(&mut headers, "authorization", &format!("Bearer {token}"));
    // Kimi's client identity (user agent, device id) goes to Kimi only, never
    // to an `openai-passthrough` upstream such as DeepSeek or OpenCode.
    if provider.adapter.as_deref() != Some("kimi-coding") {
        return headers;
    }
    insert(&mut headers, "user-agent", &format!("KimiCLI/{version}"));
    for (name, value) in msh_headers(version, provider) {
        insert(&mut headers, name, &value);
    }
    headers
}

/// Official-client identity headers (kimi-cli `_common_headers`).
#[must_use]
pub fn msh_headers(version: &str, provider: &ProviderConfig) -> Vec<(&'static str, String)> {
    let device = device_id(provider);
    vec![
        ("x-msh-platform", "kimi_cli".into()),
        ("x-msh-version", version.to_string()),
        ("x-msh-device-name", ascii_header(&hostname())),
        ("x-msh-device-model", ascii_header(&device_model())),
        ("x-msh-os-version", ascii_header(&os_version())),
        ("x-msh-device-id", ascii_header(&device)),
    ]
}

fn hostname() -> String {
    hostname_os().unwrap_or_else(|| "localhost".into())
}

fn hostname_os() -> Option<String> {
    if let Ok(value) = std::env::var("HOSTNAME")
        && !value.is_empty()
    {
        return Some(value);
    }
    let Ok(output) = std::process::Command::new("hostname").output() else {
        return None;
    };
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

fn device_model() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "macos" => format!("macOS {arch}"),
        "linux" => format!("Linux {arch}"),
        other => format!("{other} {arch}"),
    }
}

fn os_version() -> String {
    std::env::consts::OS.to_string()
}

fn device_id(provider: &ProviderConfig) -> String {
    if let Ok(path) = credential_path(provider)
        && let Some(parent) = path.parent()
    {
        let stored = parent.join("device_id");
        if let Ok(text) = std::fs::read_to_string(&stored) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        let minted = crate::proxy::http_error::request_id();
        if credentials::write_atomic(&stored, minted.as_bytes()).is_err() {
            tracing::warn!("could not persist kimi device id");
        }
        return minted;
    }
    "unknown".into()
}

fn ascii_header(value: &str) -> String {
    let ascii: String = value.chars().filter(char::is_ascii).collect();
    let trimmed = ascii.trim();
    if trimmed.is_empty() {
        "unknown".into()
    } else {
        trimmed.to_string()
    }
}

fn insert(headers: &mut HeaderMap, name: &'static str, value: &str) {
    if let (Ok(name), Ok(value)) = (
        axum::http::HeaderName::from_bytes(name.as_bytes()),
        HeaderValue::from_str(value),
    ) {
        headers.insert(name, value);
    }
}

/// The credential file whose dead mark governs this provider: only a Kimi
/// provider without a `key_cmd` has one. Every other provider is never dead.
fn dead_candidate(provider: &ProviderConfig) -> Option<PathBuf> {
    if !is_kimi(provider) || provider.key_cmd.is_some() {
        return None;
    }
    match credential_path(provider) {
        Ok(path) => Some(path),
        Err(error) => {
            tracing::debug!(%error, "kimi credential path unavailable; provider treated as live");
            None
        }
    }
}

fn is_dead(provider: &ProviderConfig) -> bool {
    dead_candidate(provider).is_some_and(|path| dead::is_dead(&path))
}

pub(crate) fn refuse_if_dead(provider: &ProviderConfig, request_id: &str) -> Option<Response> {
    if !is_dead(provider) {
        return None;
    }
    tracing::debug!(
        request_id = %request_id,
        "rejecting request for a dead kimi credential"
    );
    Some(http_error::credential_dead(request_id))
}

fn health_response(provider: &ProviderConfig) -> Response {
    let label = if is_kimi(provider) {
        "kimi"
    } else {
        PASSTHROUGH_ADAPTER
    };
    forward::provider_health(label, health_status(provider))
}

/// Health label for `/health` and `vivi proxy providers`.
#[must_use]
pub fn health_status(provider: &ProviderConfig) -> &'static str {
    if provider.key_cmd.is_some() {
        return "ok";
    }
    if is_dead(provider) {
        return "credential_dead";
    }
    match credential_path(provider) {
        Ok(_) => "ok",
        Err(_) => "missing",
    }
}

async fn refresh(
    client: &reqwest::Client,
    provider: &ProviderConfig,
) -> Result<RefreshClass, VivariumError> {
    if provider.key_cmd.is_some() {
        return Ok(RefreshClass::Ok);
    }
    let path = credential_path(provider)?;
    let _lock = credentials::lock_path(&path)?;
    let mut value = credentials::read_json(&path)?;
    let Some(refresh_token) = value.get("refresh_token").and_then(Value::as_str) else {
        dead::mark_dead(&path);
        return Ok(RefreshClass::Refused);
    };
    let refresh_token = refresh_token.to_string();
    let class = refresh_on_wire(client, provider, &refresh_token, &mut value).await?;
    if class == RefreshClass::Ok {
        credentials::write_json_atomic(&path, &value)?;
    } else if class == RefreshClass::Refused {
        dead::mark_dead(&path);
    }
    Ok(class)
}

async fn refresh_on_wire(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    refresh_token: &str,
    store: &mut Value,
) -> Result<RefreshClass, VivariumError> {
    let token_url = provider.token_url.as_deref().unwrap_or(DEFAULT_TOKEN_URL);
    let client_id = provider.client_id.as_deref().unwrap_or(DEFAULT_CLIENT_ID);
    let form = [
        ("client_id", client_id),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
    ];
    match oauth::post_refresh(client, token_url, &form, "kimi").await {
        Ok(payload) => {
            apply_refresh(store, &payload)?;
            Ok(RefreshClass::Ok)
        }
        Err(class) => Ok(class),
    }
}

fn apply_refresh(store: &mut Value, payload: &Value) -> Result<(), VivariumError> {
    let access = payload
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| VivariumError::Message("kimi token refresh failed".into()))?;
    store["access_token"] = json!(access);
    if let Some(refresh) = payload.get("refresh_token").and_then(Value::as_str) {
        store["refresh_token"] = json!(refresh);
    }
    if let Some(expires_in) = oauth::expires_in_secs(payload) {
        store["expires_at"] = json!(chrono::Utc::now().timestamp().saturating_add(expires_in));
        store["expires_in"] = json!(expires_in);
    }
    if let Some(scope) = payload.get("scope").and_then(Value::as_str) {
        store["scope"] = json!(scope);
    }
    if let Some(token_type) = payload.get("token_type").and_then(Value::as_str) {
        store["token_type"] = json!(token_type);
    }
    Ok(())
}

/// Expiry label for `vivi proxy providers` (never the token).
#[must_use]
pub fn credential_state(provider: &ProviderConfig) -> String {
    if provider.key_cmd.is_some() {
        return "key_cmd".into();
    }
    if is_dead(provider) {
        return "dead".into();
    }
    match credential_path(provider) {
        Ok(path) => match credentials::read_json(&path) {
            Ok(value) => credentials::expiry_label(&value).unwrap_or_else(|| "present".into()),
            Err(_) => "missing".into(),
        },
        Err(_) => "missing".into(),
    }
}
