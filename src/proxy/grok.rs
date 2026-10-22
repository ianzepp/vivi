//! Native xAI Grok adapter: Grok CLI OIDC login file or `key_cmd`, passthrough.
//!
//! The login file is a JSON object keyed `<issuer>::<oidc_client_id>`. The
//! adapter picks the entry for the token endpoint's issuer, refreshes its
//! `key` under an advisory lock, and writes the entry back in place with every
//! other field and entry untouched.

use std::path::{Path, PathBuf};

use axum::http::{HeaderMap, HeaderValue, Method};
use axum::response::Response;
use bytes::Bytes;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};

use super::cmd;
use super::codex_auth::REFRESH_SKEW_SECS;
use super::config::ProviderConfig;
use super::credentials;
use super::dead::{self, RefreshClass};
use super::forward;
use super::http_error;
use super::jwt;
use super::oauth;
use super::paths::expand_user_str;
use super::route::Action;
use super::usage::Usage;
use crate::VivariumError;

const DEFAULT_UPSTREAM: &str = "https://api.x.ai/v1";
const DEFAULT_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const DEFAULT_CREDENTIALS: &str = "~/.grok/auth.json";
const LABEL: &str = "grok";

/// Why no bearer token could be produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unavailable {
    /// The grant was refused; the user must log in again.
    Dead,
    /// Unreadable credentials or a transient refresh failure.
    Failed,
}

/// Handle one inbound Grok request.
///
/// # Errors
///
/// Returns an error only when the upstream exchange itself cannot be built.
/// Credential failures become redacted 502 responses and never echo tokens.
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
        let status = health_status(provider);
        return Ok((forward::provider_health(LABEL, status), Usage::default()));
    }
    if matches!(action, Action::Models) && !provider.models.is_empty() {
        let list = forward::models_body(&provider.models);
        return Ok((forward::json_ok(list), Usage::default()));
    }
    let token = match bearer(client, provider, None).await {
        Ok(token) => token,
        Err(why) => return Ok((unavailable(why, request_id), Usage::default())),
    };
    let url = forward::upstream_url(upstream(provider), action, query);
    let send = |token: String| {
        forward::proxy_request(
            client,
            method.clone(),
            url.clone(),
            headers.clone(),
            body.clone(),
            auth_header(&token),
            request_id,
        )
    };
    let first = send(token.clone()).await?;
    if first.0.status() != axum::http::StatusCode::UNAUTHORIZED || provider.key_cmd.is_some() {
        return Ok(first);
    }
    match bearer(client, provider, Some(&token)).await {
        Ok(fresh) => send(fresh).await,
        Err(why) => Ok((unavailable(why, request_id), first.1)),
    }
}

fn unavailable(why: Unavailable, request_id: &str) -> Response {
    match why {
        Unavailable::Dead => http_error::credential_dead(request_id),
        Unavailable::Failed => http_error::credential_failed(request_id),
    }
}

fn upstream(provider: &ProviderConfig) -> &str {
    provider.upstream.as_deref().unwrap_or(DEFAULT_UPSTREAM)
}

fn auth_header(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}")) {
        headers.insert(axum::http::header::AUTHORIZATION, value);
    }
    headers
}

fn failed(error: &VivariumError) -> Unavailable {
    tracing::warn!(%error, "grok credentials unavailable");
    Unavailable::Failed
}

/// Bearer token for one request. `rejected` is a token upstream just refused.
async fn bearer(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    rejected: Option<&str>,
) -> Result<String, Unavailable> {
    if let Some(command) = provider.key_cmd.as_deref() {
        return cmd::run_secret_cmd("grok key_cmd", command).map_err(|error| failed(&error));
    }
    let path = credential_path(provider);
    if dead::is_dead(&path) {
        return Err(Unavailable::Dead);
    }
    ensure_fresh(client, provider, &path, rejected).await
}

fn credential_path(provider: &ProviderConfig) -> PathBuf {
    let configured = provider
        .credentials
        .as_deref()
        .or(provider.fallback_credentials.as_deref());
    expand_user_str(configured.unwrap_or(DEFAULT_CREDENTIALS))
}

/// Resolve the credential path for diagnostics when the file exists.
#[must_use]
pub fn credential_path_if_present(provider: &ProviderConfig) -> Option<PathBuf> {
    Some(credential_path(provider)).filter(|path| path.is_file())
}

fn token_url(provider: &ProviderConfig) -> &str {
    provider.token_url.as_deref().unwrap_or(DEFAULT_TOKEN_URL)
}

fn issuer(provider: &ProviderConfig) -> Result<String, VivariumError> {
    let url = reqwest::Url::parse(token_url(provider))
        .map_err(|_| VivariumError::Config("grok token_url is not a valid URL".into()))?;
    Ok(url.origin().ascii_serialization())
}

/// Name of the login entry for this provider's issuer (and `client_id`, if set).
fn entry_name(root: &Value, provider: &ProviderConfig) -> Result<String, VivariumError> {
    let issuer = issuer(provider)?;
    let prefix = format!("{issuer}::");
    let suffix = provider.client_id.as_deref().map(|id| format!("::{id}"));
    let object = root
        .as_object()
        .ok_or_else(|| VivariumError::Config("grok credentials are not a JSON object".into()))?;
    let mut names = object.keys().filter(|name| {
        name.starts_with(&prefix) && suffix.as_ref().is_none_or(|end| name.ends_with(end))
    });
    let first = names.next().cloned();
    let extra = names.count();
    match (first, extra) {
        (Some(name), 0) => Ok(name),
        (Some(_), more) => Err(VivariumError::Config(format!(
            "grok credentials hold {} entries for issuer {issuer}; set client_id to pick one",
            more + 1
        ))),
        (None, _) => Err(VivariumError::Config(format!(
            "grok credentials have no entry for issuer {issuer}"
        ))),
    }
}

fn access_token(entry: &Value) -> Option<&str> {
    entry
        .get("key")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
}

fn expiry_unix(entry: &Value) -> Option<i64> {
    entry
        .get("expires_at")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339)
        .or_else(|| access_token(entry).and_then(jwt::exp_unix))
}

#[allow(clippy::manual_ok_err)]
fn parse_rfc3339(text: &str) -> Option<i64> {
    match DateTime::parse_from_rfc3339(text) {
        Ok(time) => Some(time.timestamp()),
        Err(_) => None,
    }
}

/// True when the token is absent or expires within the refresh skew.
/// A token with no known expiry is trusted until upstream returns 401.
fn needs_refresh(entry: &Value, now: i64) -> bool {
    match (access_token(entry), expiry_unix(entry)) {
        (None, _) => true,
        (Some(_), Some(exp)) => exp - now <= REFRESH_SKEW_SECS,
        (Some(_), None) => false,
    }
}

/// The entry's token when it is fresh and not the one upstream refused.
fn usable(entry: &Value, rejected: Option<&str>, now: i64) -> Option<String> {
    let token = access_token(entry)?;
    (!needs_refresh(entry, now) && rejected != Some(token)).then(|| token.to_owned())
}

async fn ensure_fresh(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    path: &Path,
    rejected: Option<&str>,
) -> Result<String, Unavailable> {
    let now = Utc::now().timestamp();
    let root = credentials::read_json(path).map_err(|error| failed(&error))?;
    let name = entry_name(&root, provider).map_err(|error| failed(&error))?;
    if let Some(token) = usable(&root[&name], rejected, now) {
        return Ok(token);
    }
    let _lock = lock(path).await.map_err(|error| failed(&error))?;
    let mut root = credentials::read_json(path).map_err(|error| failed(&error))?;
    let name = entry_name(&root, provider).map_err(|error| failed(&error))?;
    if let Some(token) = usable(&root[&name], rejected, now) {
        return Ok(token);
    }
    refresh_locked(client, provider, path, &mut root, &name, rejected).await
}

async fn lock(path: &Path) -> Result<std::fs::File, VivariumError> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || credentials::lock_path(&path))
        .await
        .map_err(|error| VivariumError::Other(format!("credential lock task: {error}")))?
}

async fn refresh_locked(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    path: &Path,
    root: &mut Value,
    name: &str,
    rejected: Option<&str>,
) -> Result<String, Unavailable> {
    let entry = &root[name];
    let Some(refresh_token) = entry
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
    else {
        return unrefreshable(path, entry, rejected);
    };
    let client_id = client_id(provider, entry, name);
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token.as_str()),
        ("client_id", client_id.as_str()),
    ];
    match oauth::post_refresh(client, token_url(provider), &form, LABEL).await {
        Ok(payload) => {
            apply_refresh(&mut root[name], &payload, Utc::now()).map_err(|error| failed(&error))?;
            credentials::write_json_atomic(path, root).map_err(|error| failed(&error))?;
            Ok(access_token(&root[name]).unwrap_or_default().to_owned())
        }
        Err(RefreshClass::Refused) => rotated_elsewhere(path, provider, &refresh_token),
        Err(_) => Err(Unavailable::Failed),
    }
}

/// No refresh token: a still-unrejected token is used as is, else the login is dead.
fn unrefreshable(
    path: &Path,
    entry: &Value,
    rejected: Option<&str>,
) -> Result<String, Unavailable> {
    match access_token(entry) {
        Some(token) if rejected.is_none() => Ok(token.to_owned()),
        _ => {
            dead::mark_dead(path);
            Err(Unavailable::Dead)
        }
    }
}

/// A refusal may mean another process rotated the refresh token first.
fn rotated_elsewhere(
    path: &Path,
    provider: &ProviderConfig,
    used_refresh: &str,
) -> Result<String, Unavailable> {
    let now = Utc::now().timestamp();
    if let Ok(root) = credentials::read_json(path)
        && let Ok(name) = entry_name(&root, provider)
        && root[&name].get("refresh_token").and_then(Value::as_str) != Some(used_refresh)
        && let Some(token) = usable(&root[&name], None, now)
    {
        return Ok(token);
    }
    dead::mark_dead(path);
    Err(Unavailable::Dead)
}

fn client_id(provider: &ProviderConfig, entry: &Value, name: &str) -> String {
    provider
        .client_id
        .clone()
        .or_else(|| {
            entry
                .get("oidc_client_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| name.rsplit("::").next().unwrap_or_default().to_owned())
}

/// Write a refresh response into one login entry, leaving other fields alone.
fn apply_refresh(
    entry: &mut Value,
    payload: &Value,
    now: DateTime<Utc>,
) -> Result<(), VivariumError> {
    let access = payload
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .filter(|_| entry.is_object())
        .ok_or_else(|| VivariumError::Message("grok token refresh failed".into()))?;
    entry["key"] = json!(access);
    if let Some(refresh) = payload.get("refresh_token").and_then(Value::as_str) {
        entry["refresh_token"] = json!(refresh);
    }
    let expiry = oauth::expires_in_secs(payload)
        .and_then(|secs| now.checked_add_signed(chrono::Duration::seconds(secs)))
        .or_else(|| jwt::exp_unix(access).and_then(|exp| DateTime::from_timestamp(exp, 0)));
    match expiry {
        Some(at) => {
            entry["expires_at"] = json!(at.to_rfc3339_opts(SecondsFormat::Micros, true));
        }
        None => {
            if let Some(object) = entry.as_object_mut() {
                object.remove("expires_at");
            }
        }
    }
    Ok(())
}

/// Health label for `/health` and `vivi proxy providers`.
#[must_use]
pub fn health_status(provider: &ProviderConfig) -> &'static str {
    if provider.key_cmd.is_some() {
        return "ok";
    }
    let path = credential_path(provider);
    if dead::is_dead(&path) {
        return "credential_dead";
    }
    match credentials::read_json(&path) {
        Ok(root) if entry_name(&root, provider).is_ok() => "ok",
        _ => "missing",
    }
}

/// Expiry label for `vivi proxy providers` (never the token).
#[must_use]
pub fn credential_state(provider: &ProviderConfig) -> String {
    if provider.key_cmd.is_some() {
        return "key_cmd".into();
    }
    let path = credential_path(provider);
    if dead::is_dead(&path) {
        return "dead".into();
    }
    let Ok(root) = credentials::read_json(&path) else {
        return "missing".into();
    };
    let Ok(name) = entry_name(&root, provider) else {
        return "missing".into();
    };
    expiry_unix(&root[&name])
        .and_then(|exp| DateTime::from_timestamp(exp, 0))
        .map_or_else(|| "present".into(), |at| at.to_rfc3339())
}

#[cfg(test)]
#[path = "grok_test.rs"]
mod tests;
