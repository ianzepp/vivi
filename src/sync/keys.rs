//! Project API keys: mint, list, and revoke against the project server's
//! `/keys` routes — the bearers `vivi mcp` and the letter wire accept.
//!
//! Every call is authorized by an identity token minted fresh for this one
//! invocation, never by another API key. A mint's raw key comes back from
//! the server exactly once; the list route never returns one and
//! [`ApiKeyRecord`] has no field for it. Vivi writes the raw key nowhere: no
//! file, no log, no mailspace state.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::VivariumError;
use crate::sync::SyncSettings;
use crate::sync::http::{MintedToken, RawResponse, mint_token, project_base_url, send_raw};
use crate::sync::identity::SyncIdentity;

/// One live project API key as the list route reports it: never a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyRecord {
    pub key_id: String,
    pub label: String,
    pub created_by: String,
    pub created_at: String,
}

/// A fresh mint: its record and the raw key, which the server shows once.
#[derive(Debug, Clone)]
pub struct MintedKey {
    pub record: ApiKeyRecord,
    pub key: String,
}

/// Mint a project API key named `label`: 1 to 64 characters after trimming,
/// refused before any request is made.
///
/// # Errors
/// Returns a config error for a bad label, a transport error when the
/// server cannot be reached, and a named error for any other response.
pub fn mint_key(
    settings: &SyncSettings,
    identity: &SyncIdentity,
    label: &str,
) -> Result<MintedKey, VivariumError> {
    let label = validated_label(label)?;
    let token = mint_token(settings, identity)?;
    let url = format!("{}/keys", project_base_url(settings));
    let payload = serde_json::to_vec(&json!({ "label": label })).map_err(|error| {
        VivariumError::Message(format!("cannot encode the mint request: {error}"))
    })?;
    let raw = send_raw(
        "POST",
        &url,
        &[
            ("Authorization", bearer(&token)),
            ("Content-Type", "application/json".to_string()),
        ],
        Some(payload),
    )?;
    if raw.status != 201 {
        let (code, message) = error_parts(&raw.body);
        return Err(key_error(raw.status, &code, &message));
    }
    parse_minted(&raw)
}

/// List the project's live API keys. The staging server keeps revoked keys
/// in the list, each with a `revoked_at`; those are dropped here, so only
/// live keys come back. A bare JSON array is accepted too, unknown item
/// fields are ignored, and missing text fields become empty strings. The
/// server never returns a secret.
///
/// # Errors
/// Returns a transport error when the server cannot be reached and a named
/// error for any other response.
pub fn list_keys(
    settings: &SyncSettings,
    identity: &SyncIdentity,
) -> Result<Vec<ApiKeyRecord>, VivariumError> {
    let token = mint_token(settings, identity)?;
    let url = format!("{}/keys", project_base_url(settings));
    let raw = send_raw("GET", &url, &[("Authorization", bearer(&token))], None)?;
    if raw.status != 200 {
        let (code, message) = error_parts(&raw.body);
        return Err(key_error(raw.status, &code, &message));
    }
    parse_key_list(&raw.body)
}

/// Revoke one API key by id. A 204 is success, including the second revoke
/// of an already-revoked own key, which the server answers idempotently. The
/// id is a path segment, so an id with `/`, `?`, or whitespace is refused
/// before any request is made.
///
/// # Errors
/// Returns a config error for a bad id, a transport error when the server
/// cannot be reached, and for a 404 `key_not_found` an error naming the id
/// as unknown or not minted by this member.
pub fn revoke_key(
    settings: &SyncSettings,
    identity: &SyncIdentity,
    key_id: &str,
) -> Result<(), VivariumError> {
    validated_key_id(key_id)?;
    let token = mint_token(settings, identity)?;
    let url = format!("{}/keys/{key_id}", project_base_url(settings));
    let raw = send_raw("DELETE", &url, &[("Authorization", bearer(&token))], None)?;
    if raw.status == 204 {
        return Ok(());
    }
    let (code, message) = error_parts(&raw.body);
    if raw.status == 404 && code == "key_not_found" {
        return Err(VivariumError::Message(format!(
            "api key '{key_id}' is unknown or was not minted by this member \
             (HTTP 404 key_not_found)"
        )));
    }
    Err(key_error(raw.status, &code, &message))
}

/// The label, trimmed: 1 to 64 characters, checked before any request.
fn validated_label(label: &str) -> Result<String, VivariumError> {
    let trimmed = label.trim();
    let count = trimmed.chars().count();
    if count == 0 || count > 64 {
        return Err(VivariumError::Config(format!(
            "api key label must be 1 to 64 characters (got {count})"
        )));
    }
    Ok(trimmed.to_string())
}

/// The key id: a path segment, checked before any request.
fn validated_key_id(key_id: &str) -> Result<(), VivariumError> {
    let bad = key_id.is_empty()
        || key_id.contains('/')
        || key_id.contains('?')
        || key_id.chars().any(char::is_whitespace);
    if bad {
        return Err(VivariumError::Config(format!(
            "invalid api key id '{key_id}': a key id is a path segment and cannot be \
             empty or contain '/', '?', or whitespace"
        )));
    }
    Ok(())
}

/// The Authorization header value: this mailspace's one-invocation token.
fn bearer(token: &MintedToken) -> String {
    format!("Bearer {}", token.token)
}

#[derive(Debug, Deserialize)]
struct MintedKeyBody {
    key_id: String,
    key: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    created_by: String,
    #[serde(default)]
    created_at: String,
}

/// A 201 mint response: the record and the once-only raw key.
fn parse_minted(raw: &RawResponse) -> Result<MintedKey, VivariumError> {
    let body: MintedKeyBody = serde_json::from_slice(&raw.body).map_err(|_| {
        VivariumError::Message("project server returned an unexpected key mint response".into())
    })?;
    Ok(MintedKey {
        record: ApiKeyRecord {
            key_id: body.key_id,
            label: body.label,
            created_by: body.created_by,
            created_at: body.created_at,
        },
        key: body.key,
    })
}

/// The list body: the staging object's `keys`, or a bare array.
fn parse_key_list(body: &[u8]) -> Result<Vec<ApiKeyRecord>, VivariumError> {
    let value: Value = serde_json::from_slice(body).map_err(|error| {
        VivariumError::Message(format!("invalid project server response: {error}"))
    })?;
    let items = match value {
        Value::Array(items) => items,
        Value::Object(map) => map
            .get("keys")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    Ok(items.into_iter().filter_map(live_record).collect())
}

/// One listed key, when it is live: an item whose `revoked_at` is present
/// and not null is a revoked key and is dropped. Unknown fields are ignored;
/// a missing `key_id` leaves nothing to show.
fn live_record(item: Value) -> Option<ApiKeyRecord> {
    if item.get("revoked_at").is_some_and(|at| !at.is_null()) {
        return None;
    }
    Some(ApiKeyRecord {
        key_id: text_field(&item, "key_id")?,
        label: text_field(&item, "label").unwrap_or_default(),
        created_by: text_field(&item, "created_by").unwrap_or_default(),
        created_at: text_field(&item, "created_at").unwrap_or_default(),
    })
}

/// One string field of a list item, empty when absent or not a string.
fn text_field(item: &Value, name: &str) -> Option<String> {
    item.get(name).and_then(Value::as_str).map(String::from)
}

/// One failed key-route call as an error. A 401 asks for this mailspace's
/// identity; every other status carries the server's status, code, and
/// message. Never a token.
fn key_error(status: u16, code: &str, message: &str) -> VivariumError {
    if status == 401 {
        return VivariumError::Message(format!(
            "the key routes need this mailspace's identity: registered and admitted \
             (vivi sync join); the server rejected it (HTTP {status} {code}: {message})"
        ));
    }
    VivariumError::Message(format!("HTTP {status} {code}: {message}"))
}

/// The server's `{"error":{"code","message"}}`, or empties when the body
/// carries none (a 204's empty body included).
fn error_parts(body: &[u8]) -> (String, String) {
    let value = match serde_json::from_slice::<Value>(body) {
        Ok(value) => value,
        Err(_) => return (String::new(), String::new()),
    };
    let part = |pointer: &str| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    (part("/error/code"), part("/error/message"))
}

#[cfg(test)]
#[path = "keys_test.rs"]
mod tests;
