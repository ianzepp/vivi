//! Native Kimi Code adapter: OAuth file or `key_cmd`, header injection, passthrough.

use std::path::PathBuf;

use axum::http::{HeaderMap, HeaderValue, Method};
use axum::response::Response;
use bytes::Bytes;
use serde_json::{Value, json};

use super::cmd;
use super::config::ProviderConfig;
use super::credentials;
use super::forward;
use super::http_error::{self, request_id};
use super::paths::expand_user_str;
use super::route::Action;
use super::usage::Usage;
use crate::VivariumError;

const DEFAULT_UPSTREAM: &str = "https://api.kimi.com/coding/v1";
const DEFAULT_TOKEN_URL: &str = "https://auth.kimi.com/api/oauth/token";
const DEFAULT_CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const DEFAULT_CLIENT_VERSION: &str = "1.15.0";

/// Handle one inbound Kimi request.
///
/// # Errors
///
/// Returns a configuration error when credentials cannot be read. Token
/// endpoint bodies are never included in the error text.
pub async fn handle(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    action: &Action,
    method: Method,
    query: Option<&str>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(Response, Usage), VivariumError> {
    let id = request_id();
    if matches!(action, Action::Health) {
        return Ok((health_response(provider), Usage::default()));
    }
    if matches!(action, Action::Models) && !provider.models.is_empty() {
        return Ok((
            forward::json_ok(models_body(&provider.models)),
            Usage::default(),
        ));
    }
    let result = dispatch(
        client,
        provider,
        action,
        &method,
        query,
        &headers,
        body.clone(),
        false,
    )
    .await;
    match result {
        Ok((response, usage)) if response.status() == axum::http::StatusCode::UNAUTHORIZED => {
            if refresh(client, provider).await.is_ok() {
                dispatch(
                    client, provider, action, &method, query, &headers, body, true,
                )
                .await
            } else {
                tracing::warn!(request_id = %id, "kimi refresh after 401 failed");
                Ok((http_error::credential_failed(&id), usage))
            }
        }
        other => other,
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
    _retried: bool,
) -> Result<(Response, Usage), VivariumError> {
    let id = request_id();
    let token = match bearer(provider) {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(%error, request_id = %id, "kimi credentials unreadable");
            return Ok((http_error::credential_failed(&id), Usage::default()));
        }
    };
    let url = upstream_url(provider, action, query);
    let extra = identity_headers(provider, &token);
    forward::proxy_request(client, method.clone(), url, headers.clone(), body, extra).await
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
    let mut candidates = Vec::new();
    if let Some(path) = provider.credentials.as_deref() {
        candidates.push(expand_user_str(path));
    }
    if let Some(path) = provider.fallback_credentials.as_deref() {
        candidates.push(expand_user_str(path));
    }
    candidates.push(expand_user_str("~/.kimi-code/credentials/kimi-code.json"));
    candidates.push(expand_user_str("~/.kimi/credentials/kimi-code.json"));
    credentials::first_existing(&candidates)
}

fn upstream_url(provider: &ProviderConfig, action: &Action, query: Option<&str>) -> String {
    let base = provider.upstream.as_deref().unwrap_or(DEFAULT_UPSTREAM);
    let path = match action {
        Action::Responses => "/responses",
        Action::Messages => "/messages",
        Action::Models => "/models",
        Action::Usage => "/usage",
        Action::Other(path) if !path.is_empty() => path.as_str(),
        Action::ChatCompletions | Action::Health | Action::Other(_) => "/chat/completions",
    };
    forward::join_url(base, path, query)
}

fn identity_headers(provider: &ProviderConfig, token: &str) -> HeaderMap {
    let version = provider
        .client_version
        .as_deref()
        .unwrap_or(DEFAULT_CLIENT_VERSION);
    let mut headers = HeaderMap::new();
    insert(&mut headers, "authorization", &format!("Bearer {token}"));
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

fn health_response(provider: &ProviderConfig) -> Response {
    let ok = provider.key_cmd.is_some() || credential_path(provider).is_ok();
    forward::json_ok(json!({ "ok": ok, "provider": "kimi" }))
}

fn models_body(models: &[String]) -> Value {
    let data: Vec<Value> = models
        .iter()
        .map(|model| json!({ "id": model, "object": "model" }))
        .collect();
    json!({ "object": "list", "data": data })
}

async fn refresh(client: &reqwest::Client, provider: &ProviderConfig) -> Result<(), VivariumError> {
    if provider.key_cmd.is_some() {
        return Ok(());
    }
    let path = credential_path(provider)?;
    let _lock = credentials::lock_path(&path)?;
    let mut value = credentials::read_json(&path)?;
    let refresh_token = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .ok_or_else(|| VivariumError::Config("kimi credentials missing refresh_token".into()))?
        .to_string();
    let token_url = provider.token_url.as_deref().unwrap_or(DEFAULT_TOKEN_URL);
    let client_id = provider.client_id.as_deref().unwrap_or(DEFAULT_CLIENT_ID);
    let form = [
        ("client_id", client_id),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token.as_str()),
    ];
    let response = client
        .post(token_url)
        .form(&form)
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(%error, "kimi token endpoint unreachable");
            VivariumError::Message("kimi token refresh failed".into())
        })?;
    if !response.status().is_success() {
        tracing::warn!(status = %response.status(), "kimi token refresh refused");
        return Err(VivariumError::Message("kimi token refresh failed".into()));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|_| VivariumError::Message("kimi token refresh failed".into()))?;
    apply_refresh(&mut value, &payload)?;
    credentials::write_json_atomic(&path, &value)
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
    if let Some(expires_in) = expires_in_secs(payload) {
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

fn expires_in_secs(payload: &Value) -> Option<i64> {
    payload
        .get("expires_in")
        .and_then(Value::as_i64)
        .or_else(|| {
            payload
                .get("expires_in")
                .and_then(Value::as_u64)
                .and_then(|secs| {
                    #[allow(clippy::manual_ok_err)]
                    match i64::try_from(secs) {
                        Ok(value) => Some(value),
                        Err(_) => None,
                    }
                })
        })
}

/// Expiry label for `vivi proxy providers` (never the token).
#[must_use]
pub fn credential_state(provider: &ProviderConfig) -> String {
    if provider.key_cmd.is_some() {
        return "key_cmd".into();
    }
    match credential_path(provider).and_then(|path| credentials::read_json(&path)) {
        Ok(value) => credentials::expiry_label(&value).unwrap_or_else(|| "present".into()),
        Err(_) => "missing".into(),
    }
}
