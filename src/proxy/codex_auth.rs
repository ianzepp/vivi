//! Codex `auth.json`: JWT expiry, refresh, advisory lock, atomic 0600 write-back.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::config::ProviderConfig;
use super::credentials;
use super::dead::{self, RefreshClass};
use super::jwt;
use super::paths::expand_user_str;
use crate::VivariumError;

/// Official Codex CLI client id (public, not a secret).
pub const DEFAULT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Token endpoint used by the Codex CLI.
pub const DEFAULT_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// Refresh this many seconds before JWT `exp`.
pub const REFRESH_SKEW_SECS: i64 = 30;

/// Resolved on-disk auth file.
#[derive(Debug, Clone)]
pub struct AuthFile {
    pub path: PathBuf,
    pub value: Value,
}

/// Tokens the adapter actually sends.
#[derive(Debug, Clone)]
pub struct Tokens {
    pub access_token: String,
    pub account_id: Option<String>,
}

/// Load `~/.codex/auth.json`, then the configured fallback.
///
/// # Errors
///
/// Returns a configuration error when no readable auth file exists.
pub fn load(provider: &ProviderConfig) -> Result<AuthFile, VivariumError> {
    let path = credential_path(provider)?;
    let value = credentials::read_json(&path)?;
    Ok(AuthFile { path, value })
}

/// Candidate auth paths, first existing wins.
///
/// # Errors
///
/// Returns a configuration error when none of the paths exist.
pub fn credential_path(provider: &ProviderConfig) -> Result<PathBuf, VivariumError> {
    let mut candidates = Vec::new();
    if let Some(path) = provider.credentials.as_deref() {
        candidates.push(expand_user_str(path));
    }
    if let Some(path) = provider.fallback_credentials.as_deref() {
        candidates.push(expand_user_str(path));
    }
    candidates.push(expand_user_str("~/.codex/auth.json"));
    candidates.push(expand_user_str("~/.pi/agent/auth.json"));
    credentials::first_existing(&candidates)
}

/// Pull the access token and account id out of a Codex or flat JSON object.
///
/// # Errors
///
/// Returns a configuration error when `access_token` is missing.
pub fn tokens(value: &Value) -> Result<Tokens, VivariumError> {
    let access = access_token(value)
        .ok_or_else(|| VivariumError::Config("codex auth.json missing access_token".into()))?;
    Ok(Tokens {
        access_token: access,
        account_id: account_id(value),
    })
}

fn access_token(value: &Value) -> Option<String> {
    value
        .pointer("/tokens/access_token")
        .or_else(|| value.get("access_token"))
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}

fn account_id(value: &Value) -> Option<String> {
    value
        .pointer("/tokens/account_id")
        .or_else(|| value.get("account_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

fn refresh_token(value: &Value) -> Option<String> {
    value
        .pointer("/tokens/refresh_token")
        .or_else(|| value.get("refresh_token"))
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}

/// True when the access token is missing `exp` or expires within the skew.
#[must_use]
pub fn needs_refresh(value: &Value, now: i64) -> bool {
    match access_token(value) {
        Some(token) => jwt::expires_within(&token, REFRESH_SKEW_SECS, now),
        None => true,
    }
}

/// Re-read, refresh if needed, write back atomically under an advisory lock.
///
/// # Errors
///
/// Returns a configuration or token-refresh error. The token-endpoint body
/// is never included in the error text.
pub async fn ensure_fresh(
    client: &reqwest::Client,
    provider: &ProviderConfig,
) -> Result<Tokens, VivariumError> {
    let path = credential_path(provider)?;
    if dead::is_dead(&path) {
        return Err(VivariumError::Message(dead::DEAD_MESSAGE.into()));
    }
    let _lock = credentials::lock_path(&path)?;
    let mut value = credentials::read_json(&path)?;
    if !needs_refresh(&value, chrono::Utc::now().timestamp()) {
        return tokens(&value);
    }
    let Some(current_refresh) = refresh_token(&value) else {
        return missing_refresh_token(&path, &value);
    };
    match refresh_on_wire(client, provider, &current_refresh).await {
        Ok(payload) => {
            apply_refresh(&mut value, &payload)?;
            credentials::write_json_atomic(&path, &value)?;
            tokens(&value)
        }
        Err(RefreshClass::Refused) => {
            dead::mark_dead(&path);
            Err(VivariumError::Message(dead::DEAD_MESSAGE.into()))
        }
        Err(RefreshClass::Transient | RefreshClass::Ok) => {
            Err(VivariumError::Message("codex token refresh failed".into()))
        }
    }
}

fn missing_refresh_token(path: &std::path::Path, value: &Value) -> Result<Tokens, VivariumError> {
    if access_token(value).is_some_and(|token| jwt::exp_unix(&token).is_some()) {
        dead::mark_dead(path);
        return Err(VivariumError::Message(dead::DEAD_MESSAGE.into()));
    }
    tokens(value)
}

async fn refresh_on_wire(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    refresh_token: &str,
) -> Result<Value, RefreshClass> {
    let token_url = provider.token_url.as_deref().unwrap_or(DEFAULT_TOKEN_URL);
    let client_id = provider.client_id.as_deref().unwrap_or(DEFAULT_CLIENT_ID);
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
        ("scope", "openid profile email offline_access"),
    ];
    let response = match client.post(token_url).form(&form).send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, "codex token endpoint unreachable");
            return Err(RefreshClass::Transient);
        }
    };
    match dead::classify_status(response.status()) {
        RefreshClass::Ok => response.json().await.map_err(|error| {
            tracing::warn!(%error, "codex token refresh returned non-JSON");
            RefreshClass::Transient
        }),
        RefreshClass::Refused => Err(RefreshClass::Refused),
        RefreshClass::Transient => {
            tracing::warn!(status = %response.status(), "codex token refresh transient");
            Err(RefreshClass::Transient)
        }
    }
}

fn apply_refresh(store: &mut Value, payload: &Value) -> Result<(), VivariumError> {
    let access = payload
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| VivariumError::Message("codex token refresh failed".into()))?;
    write_token_field(store, "access_token", access);
    if let Some(refresh) = payload.get("refresh_token").and_then(Value::as_str) {
        write_token_field(store, "refresh_token", refresh);
    }
    if let Some(id_token) = payload.get("id_token").and_then(Value::as_str) {
        write_token_field(store, "id_token", id_token);
    }
    store["last_refresh"] = json!(chrono::Utc::now().to_rfc3339());
    Ok(())
}

fn write_token_field(store: &mut Value, field: &str, value: &str) {
    if store.get("tokens").is_some() {
        store["tokens"][field] = json!(value);
    } else {
        store[field] = json!(value);
    }
}

/// Expiry label for `vivi proxy providers`.
#[must_use]
pub fn credential_state(provider: &ProviderConfig) -> String {
    match credential_path(provider) {
        Ok(path) if dead::is_dead(&path) => "dead".into(),
        Ok(_) => match load(provider) {
            Ok(auth) => credentials::expiry_label(&auth.value).unwrap_or_else(|| "present".into()),
            Err(_) => "missing".into(),
        },
        Err(_) => "missing".into(),
    }
}

/// Public path helper for health checks.
#[must_use]
pub fn path_if_present(provider: &ProviderConfig) -> Option<PathBuf> {
    #[allow(clippy::manual_ok_err)]
    match credential_path(provider) {
        Ok(path) => Some(path),
        Err(_) => None,
    }
}

/// Test helper: apply a refresh payload to an in-memory auth document.
///
/// # Errors
///
/// Returns an error when the payload has no `access_token`.
pub fn apply_refresh_for_test(store: &mut Value, payload: &Value) -> Result<(), VivariumError> {
    apply_refresh(store, payload)
}
