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
    /// The vivi identity (such as `operator`) this key acts as, when the
    /// minter named one. Absent on keys minted without one and on key files
    /// written before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity: Option<String>,
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
    pub identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
}

/// What a live API key resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedKey {
    /// The registered identity that minted the key; the key authenticates as it.
    pub created_by: String,
    /// The vivi identity the key acts as, when one was set at mint time.
    pub identity: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MintedKey {
    pub key_id: String,
    pub key: String,
    pub label: String,
    pub created_by: String,
    pub created_at: String,
    pub identity: Option<String>,
}

#[derive(Debug)]
pub enum KeyError {
    InvalidLabel,
    InvalidIdentity,
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

    /// Mint a project-scoped key bound to `created_by`, optionally acting as
    /// the vivi `identity`. The raw secret is returned once; only its digest
    /// is persisted.
    pub fn mint(
        &self,
        project_id: &str,
        created_by: &str,
        label: &str,
        identity: Option<&str>,
    ) -> Result<MintedKey, KeyError> {
        let label = validate_label(label)?;
        let identity = identity.map(validate_identity).transpose()?;
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
            identity: identity.clone(),
            revoked_at: None,
        });
        self.save(project_id, &file).map_err(KeyError::Storage)?;
        Ok(MintedKey {
            key_id,
            key,
            label,
            created_by: created_by.to_owned(),
            created_at,
            identity,
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
                identity: key.identity,
                revoked_at: key.revoked_at,
            })
            .collect()
    }

    /// Record an already-minted server key (same key id, digest and
    /// metadata) so the per-project routes accept the same secret. The
    /// transitional mirror for `/v1/mcp` self-service keys; the registry is
    /// removed with those routes.
    pub fn adopt(
        &self,
        project_id: &str,
        key_id: &str,
        label: &str,
        digest: &str,
        created_by: &str,
        created_at: &str,
        identity: Option<&str>,
    ) -> Result<(), KeyError> {
        let _guard = self.lock();
        let mut file = self.load(project_id);
        if file.keys.iter().any(|key| key.key_id == key_id) {
            return Ok(());
        }
        let live = file
            .keys
            .iter()
            .filter(|key| key.revoked_at.is_none())
            .count();
        if live >= MAX_KEYS {
            return Err(KeyError::LimitReached);
        }
        file.keys.push(KeyRecord {
            key_id: key_id.to_owned(),
            label: label.to_owned(),
            digest: digest.to_owned(),
            created_by: created_by.to_owned(),
            created_at: created_at.to_owned(),
            identity: identity.map(str::to_owned),
            revoked_at: None,
        });
        self.save(project_id, &file).map_err(KeyError::Storage)
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

    /// The minting identity name (and the identity it acts as, if set) for a
    /// live key, if `presented` matches.
    pub fn verify(&self, project_id: &str, presented: &str) -> Option<VerifiedKey> {
        let _guard = self.lock();
        let digest = sha256_hex(presented.as_bytes());
        self.load(project_id).keys.into_iter().find_map(|key| {
            (key.revoked_at.is_none() && constant_time_eq(&digest, &key.digest)).then_some(
                VerifiedKey {
                    created_by: key.created_by,
                    identity: key.identity,
                },
            )
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
    resolve_project_caller(identities, keys, project_id, presented).map(|caller| caller.identity)
}

/// A resolved Bearer: the registered identity that authenticates, and the
/// vivi identity an API key acts as (always `None` for an identity token).
#[derive(Debug, Clone)]
pub struct Caller {
    pub identity: IdentityRecord,
    pub acting_as: Option<String>,
    /// True when the Bearer was an API key, false for an identity token.
    pub via_key: bool,
}

/// As [`resolve_project_identity`], keeping the key's `identity`.
pub fn resolve_project_caller(
    identities: &IdentityRegistry,
    keys: &ApiKeyRegistry,
    project_id: &str,
    presented: &str,
) -> Option<Caller> {
    if let Some(identity) = identities.verify_bearer(project_id, presented) {
        return Some(Caller {
            identity,
            acting_as: None,
            via_key: false,
        });
    }
    let verified = keys.verify(project_id, presented)?;
    Some(Caller {
        identity: identities.by_name(project_id, &verified.created_by)?,
        acting_as: verified.identity,
        via_key: true,
    })
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

/// A vivi identity name: the mail local-part charset, 1-64 characters.
fn validate_identity(identity: &str) -> Result<String, KeyError> {
    let trimmed = identity.trim();
    let valid = !trimmed.is_empty()
        && trimmed.len() <= MAX_LABEL
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if valid {
        Ok(trimmed.to_owned())
    } else {
        Err(KeyError::InvalidIdentity)
    }
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
    /// Optional vivi identity the key acts as; absent means none.
    #[serde(default)]
    identity: Option<String>,
}

fn key_error(err: KeyError) -> Response {
    match err {
        KeyError::InvalidLabel => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "label must be 1-64 characters with no control characters",
        ),
        KeyError::InvalidIdentity => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "identity must be 1-64 characters of letters, digits, '.', '-' or '_'",
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

/// `POST /v1/projects/:id/keys` — mint a server key (identity token). The
/// key's principal is `<member>@<project_id>` with one grant on this
/// project (operator when today's operator rule holds for this mint), and
/// the same secret is mirrored into the per-project registry so the
/// per-project routes keep accepting it until they are removed.
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
    let mcp_state = state.clone();
    let old_keys = state.keys.clone();
    let project_id = id.clone();
    let identity = identity.clone();
    let label = req.label;
    let acting = req.identity.clone();
    let answer_identity = req.identity;
    match tokio::task::spawn_blocking(move || {
        let level = crate::mcp::server::self_service_level(
            &mcp_state,
            &project_id,
            &identity,
            acting.as_deref(),
        );
        let principal = format!("{}@{project_id}", identity.name);
        let grant = crate::grants::GrantRecord {
            project_id: project_id.clone(),
            member: identity.name.clone(),
            identity: acting.clone(),
            level,
        };
        let minted =
            mcp_state
                .server_keys
                .mint(&principal, false, vec![grant], &label, &identity.name)?;
        // The mirror: the per-project registry holds the same secret so
        // `/v1/projects/{id}/mcp` keeps working until it is removed. A
        // failure keeps the server key (it works on /v1/mcp) and is logged.
        let digest = sha256_hex(minted.key.as_bytes());
        if let Err(err) = old_keys.adopt(
            &project_id,
            &minted.key_id,
            &minted.label,
            &digest,
            &minted.created_by,
            &minted.created_at,
            acting.as_deref(),
        ) {
            tracing::warn!(
                "could not mirror the server key {} into the per-project registry: {err:?}",
                minted.key_id
            );
        }
        Ok::<_, crate::grants::GrantsError>(minted)
    })
    .await
    {
        Ok(Ok(minted)) => {
            let mut body = json!({
                "key_id": minted.key_id,
                "key": minted.key,
                "label": minted.label,
                "created_by": minted.created_by,
                "created_at": minted.created_at,
                "grants": minted.grants,
            });
            if let Some(identity) = answer_identity {
                body["identity"] = json!(identity);
            }
            (StatusCode::CREATED, Json(body)).into_response()
        }
        Ok(Err(err)) => grants_error(err),
        Err(err) => grants_error(crate::grants::GrantsError::Storage(format!(
            "key task failed: {err}"
        ))),
    }
}

/// The HTTP answers for a refused server-key mint. The identity text is
/// the old one: the identity is the only free-text field a caller names.
fn grants_error(err: crate::grants::GrantsError) -> Response {
    use crate::grants::GrantsError;
    match err {
        GrantsError::InvalidLabel => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "label must be 1-64 characters with no control characters",
        ),
        GrantsError::InvalidGrant => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "identity must be 1-64 characters of letters, digits, '.', '-' or '_'",
        ),
        GrantsError::LimitReached => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "this member already has 32 live API keys on this project",
        ),
        GrantsError::InvalidPrincipal | GrantsError::HostWithGrants | GrantsError::HostMismatch => {
            error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "invalid principal for a self-service key",
            )
        }
        GrantsError::NotFound => {
            error_response(StatusCode::NOT_FOUND, "key_not_found", "unknown key")
        }
        GrantsError::Storage(message) => {
            tracing::error!("server key storage failure: {message}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "storage failure",
            )
        }
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
    let server_key_id = key_id.clone();
    let project_id = id;
    match tokio::task::spawn_blocking(move || keys.revoke(&project_id, &key_id, &identity.name))
        .await
    {
        Ok(Ok(())) => {}
        Ok(Err(err)) => return key_error(err),
        Err(err) => return key_error(KeyError::Storage(format!("key task failed: {err}"))),
    }
    // The server-store twin of a self-service key dies with it. An
    // old-registry-only key has no twin (`NotFound`).
    let server_keys = state.server_keys.clone();
    match tokio::task::spawn_blocking(move || server_keys.revoke_key(&server_key_id)).await {
        Ok(Ok(())) | Ok(Err(crate::grants::GrantsError::NotFound)) => {
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(Err(crate::grants::GrantsError::Storage(message))) => {
            tracing::error!("server key storage failure: {message}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "storage failure",
            )
        }
        Ok(Err(_)) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => key_error(KeyError::Storage(format!("key task failed: {err}"))),
    }
}
