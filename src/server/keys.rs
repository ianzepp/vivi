//! Project-scoped API keys for callers that cannot sign an ED25519
//! challenge (a URL-added MCP connector, a bot). A member mints a key;
//! the raw secret is returned once; only its SHA-256 digest is stored.
//! A presented key is a Bearer, same header as an identity token, and
//! resolves to the minting identity.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;
use crate::auth::{AuthError, constant_time_eq, mint_token, rfc3339, sha256_hex};
use crate::identities::{IdentityRecord, IdentityRegistry};
use crate::projects_api::{json_body, require_existing_project};
use crate::rooms::error_response;

const KEY_PREFIX: &str = "vivi_";
const MAX_KEYS: usize = 32;
const MAX_LABEL: usize = 64;
const KEY_ID_BYTES: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyRecord {
    key_id: String,
    label: String,
    digest: String,
    created_by: String,
    created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revoked_at: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct KeysFile {
    keys: Vec<KeyRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KeySummary {
    pub key_id: String,
    pub label: String,
    pub created_by: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MintedKey {
    pub key_id: String,
    pub key: String,
    pub label: String,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Debug)]
pub enum KeyError {
    InvalidLabel,
    LimitReached,
    NotFound,
    Storage(String),
}

#[derive(Clone)]
pub struct ApiKeyRegistry {
    lock: Arc<Mutex<()>>,
    root: PathBuf,
}

impl ApiKeyRegistry {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            lock: Arc::new(Mutex::new(())),
            root: data_dir.join("projects"),
        }
    }

    fn path(&self, project_id: &str) -> PathBuf {
        self.root.join(project_id).join("api_keys.json")
    }

    fn load(&self, project_id: &str) -> KeysFile {
        std::fs::read(self.path(project_id))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, project_id: &str, file: &KeysFile) -> Result<(), String> {
        let bytes =
            serde_json::to_vec(file).map_err(|e| format!("failed to encode api_keys.json: {e}"))?;
        let path = self.path(project_id);
        let tmp = path.with_extension("json.tmp");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to prepare api_keys dir: {e}"))?;
        }
        write_private(&tmp, &bytes).map_err(|e| format!("failed to write api_keys.json: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("failed to place api_keys.json: {e}"))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Mint a project-scoped key bound to `created_by`. The raw secret is
    /// returned once; only its digest is persisted.
    pub fn mint(
        &self,
        project_id: &str,
        created_by: &str,
        label: &str,
    ) -> Result<MintedKey, KeyError> {
        let label = validate_label(label)?;
        let _guard = self.lock();
        let mut file = self.load(project_id);
        let live = file
            .keys
            .iter()
            .filter(|key| key.revoked_at.is_none())
            .count();
        if live >= MAX_KEYS {
            return Err(KeyError::LimitReached);
        }
        let minted = mint_token().map_err(|err| match err {
            AuthError::Entropy => KeyError::Storage("entropy unavailable".into()),
            _ => KeyError::Storage("key mint failed".into()),
        })?;
        let key = format!("{KEY_PREFIX}{}", minted.token);
        let digest = sha256_hex(key.as_bytes());
        let key_id = mint_key_id().map_err(|err| match err {
            AuthError::Entropy => KeyError::Storage("entropy unavailable".into()),
            _ => KeyError::Storage("key id mint failed".into()),
        })?;
        let created_at = rfc3339(chrono::Utc::now());
        file.keys.push(KeyRecord {
            key_id: key_id.clone(),
            label: label.clone(),
            digest,
            created_by: created_by.to_owned(),
            created_at: created_at.clone(),
            revoked_at: None,
        });
        self.save(project_id, &file).map_err(KeyError::Storage)?;
        Ok(MintedKey {
            key_id,
            key,
            label,
            created_by: created_by.to_owned(),
            created_at,
        })
    }

    pub fn list(&self, project_id: &str) -> Vec<KeySummary> {
        let _guard = self.lock();
        self.load(project_id)
            .keys
            .into_iter()
            .map(|key| KeySummary {
                key_id: key.key_id,
                label: key.label,
                created_by: key.created_by,
                created_at: key.created_at,
                revoked_at: key.revoked_at,
            })
            .collect()
    }

    /// Revoke `key_id` when it was minted by `actor`. Already-revoked is
    /// success. Unknown or someone else's key is `NotFound`.
    pub fn revoke(&self, project_id: &str, key_id: &str, actor: &str) -> Result<(), KeyError> {
        let _guard = self.lock();
        let mut file = self.load(project_id);
        let Some(key) = file
            .keys
            .iter_mut()
            .find(|key| key.key_id == key_id && key.created_by == actor)
        else {
            return Err(KeyError::NotFound);
        };
        if key.revoked_at.is_none() {
            key.revoked_at = Some(rfc3339(chrono::Utc::now()));
            self.save(project_id, &file).map_err(KeyError::Storage)?;
        }
        Ok(())
    }

    /// The minting identity name for a live key, if `presented` matches.
    pub fn verify(&self, project_id: &str, presented: &str) -> Option<String> {
        let _guard = self.lock();
        let digest = sha256_hex(presented.as_bytes());
        self.load(project_id).keys.into_iter().find_map(|key| {
            (key.revoked_at.is_none() && constant_time_eq(&digest, &key.digest))
                .then_some(key.created_by)
        })
    }
}

/// Resolve a Bearer to an identity: a live identity token first, then a
/// live API key bound to a registered name.
pub fn resolve_project_identity(
    identities: &IdentityRegistry,
    keys: &ApiKeyRegistry,
    project_id: &str,
    presented: &str,
) -> Option<IdentityRecord> {
    if let Some(identity) = identities.verify_bearer(project_id, presented) {
        return Some(identity);
    }
    let name = keys.verify(project_id, presented)?;
    identities.by_name(project_id, &name)
}

fn validate_label(label: &str) -> Result<String, KeyError> {
    let trimmed = label.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_LABEL {
        return Err(KeyError::InvalidLabel);
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(KeyError::InvalidLabel);
    }
    Ok(trimmed.to_owned())
}

fn mint_key_id() -> Result<String, AuthError> {
    let mut raw = [0u8; KEY_ID_BYTES];
    getrandom::fill(&mut raw).map_err(|_| AuthError::Entropy)?;
    Ok(format!("key_{}", URL_SAFE_NO_PAD.encode(raw)))
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

#[derive(Debug, Deserialize)]
pub struct MintKeyRequest {
    label: String,
}

fn key_error(err: KeyError) -> Response {
    match err {
        KeyError::InvalidLabel => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "label must be 1-64 characters with no control characters",
        ),
        KeyError::LimitReached => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "this project already has 32 live API keys",
        ),
        KeyError::NotFound => {
            error_response(StatusCode::NOT_FOUND, "key_not_found", "unknown API key")
        }
        KeyError::Storage(message) => {
            tracing::error!("api key storage failure: {message}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "storage failure",
            )
        }
    }
}

/// Minting and revocation require a proof-of-possession identity token, not
/// another API key, so a leaked bot key cannot grow or erase the set.
fn identity_token_only(
    identities: &IdentityRegistry,
    project_id: &str,
    token: &str,
) -> Option<IdentityRecord> {
    identities.verify_bearer(project_id, token)
}

/// `POST /v1/projects/:id/keys` — mint a project-scoped API key (identity token).
pub async fn mint_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    payload: Result<Json<MintKeyRequest>, JsonRejection>,
) -> Response {
    let req = match json_body(payload) {
        Ok(req) => req,
        Err(response) => return response,
    };
    if let Err(response) = require_existing_project(&state, &id).await {
        return response;
    }
    let Some(token) = crate::projects_api::presented_bearer(&headers) else {
        return crate::projects_api::missing_bearer();
    };
    let identities = state.identities.clone();
    let project_id = id.clone();
    let token = token.to_owned();
    let identity = match tokio::task::spawn_blocking(move || {
        identity_token_only(&identities, &project_id, &token)
    })
    .await
    {
        Ok(Some(identity)) => identity,
        Ok(None) => return crate::projects_api::invalid_bearer(),
        Err(_) => return crate::projects_api::identity_lookup_failed(),
    };
    if identity.guest {
        return error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "keyless chat members cannot mint API keys",
        );
    }
    let keys = state.keys.clone();
    match tokio::task::spawn_blocking(move || keys.mint(&id, &identity.name, &req.label)).await {
        Ok(Ok(minted)) => (
            StatusCode::CREATED,
            Json(json!({
                "key_id": minted.key_id,
                "key": minted.key,
                "label": minted.label,
                "created_by": minted.created_by,
                "created_at": minted.created_at,
            })),
        )
            .into_response(),
        Ok(Err(err)) => key_error(err),
        Err(err) => key_error(KeyError::Storage(format!("key task failed: {err}"))),
    }
}

/// `GET /v1/projects/:id/keys` — list keys (no secrets). Identity token or API key.
pub async fn list_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_existing_project(&state, &id).await {
        return response;
    }
    if let Err(response) = crate::projects_api::authenticate(&state, &id, &headers).await {
        return response;
    }
    let keys = state.keys.clone();
    match tokio::task::spawn_blocking(move || keys.list(&id)).await {
        Ok(list) => Json(json!({ "keys": list })).into_response(),
        Err(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "storage failure",
        ),
    }
}

/// `DELETE /v1/projects/:id/keys/:key_id` — revoke a key the caller minted.
pub async fn revoke_handler(
    State(state): State<AppState>,
    AxumPath((id, key_id)): AxumPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_existing_project(&state, &id).await {
        return response;
    }
    let Some(token) = crate::projects_api::presented_bearer(&headers) else {
        return crate::projects_api::missing_bearer();
    };
    let identities = state.identities.clone();
    let project_id = id.clone();
    let token = token.to_owned();
    let identity = match tokio::task::spawn_blocking(move || {
        identity_token_only(&identities, &project_id, &token)
    })
    .await
    {
        Ok(Some(identity)) => identity,
        Ok(None) => return crate::projects_api::invalid_bearer(),
        Err(_) => return crate::projects_api::identity_lookup_failed(),
    };
    let keys = state.keys.clone();
    match tokio::task::spawn_blocking(move || keys.revoke(&id, &key_id, &identity.name)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(err)) => key_error(err),
        Err(err) => key_error(KeyError::Storage(format!("key task failed: {err}"))),
    }
}
