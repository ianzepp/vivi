//! Project creation, identity proof-of-possession, and item read endpoints.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::json;
use std::hash::{Hash, Hasher};
use vivi::storage::is_traveling_id;

use crate::AppState;
use crate::auth::{self, AuthError, canonical_spki_hex, parse_verifying_key};
use crate::identities::RegisterError;
use crate::projects::{AppendItem, ProjectError};
use crate::rooms::error_response;
use crate::safety::body_over_cap;

#[derive(Debug, Deserialize)]
pub struct CreateProjectRequest {
    public_key_pem: String,
    creator_name: String,
}

#[derive(Debug, Deserialize)]
pub struct ChallengeRequest {
    public_key_pem: String,
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    name: String,
    public_key_pem: String,
    challenge_id: String,
    signature_base64: String,
}

#[derive(Debug, Deserialize)]
pub struct ItemsQuery {
    #[serde(default)]
    since: u64,
}

/// Append body, mirroring the room-send shape. `from` is never client
/// input: it is always the bearer identity's name.
#[derive(Debug, Deserialize)]
pub struct AppendRequest {
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    subject: Option<String>,
    body: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    in_reply_to: Option<String>,
    #[serde(default)]
    refs: Vec<String>,
}

fn project_error(err: ProjectError) -> Response {
    match err {
        ProjectError::InvalidProjectId => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "invalid project id",
        ),
        ProjectError::ProjectNotFound => error_response(
            StatusCode::NOT_FOUND,
            "project_not_found",
            "unknown project id",
        ),
        ProjectError::BadRequest(message) => {
            error_response(StatusCode::BAD_REQUEST, "invalid_request", &message)
        }
        ProjectError::Conflict(message) => {
            error_response(StatusCode::CONFLICT, "conflict", &message)
        }
        ProjectError::Storage(message) => {
            tracing::error!("project storage failure: {message}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "storage failure",
            )
        }
    }
}

fn register_error(err: RegisterError) -> Response {
    match err {
        RegisterError::InvalidKey | RegisterError::InvalidName => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "invalid identity name or public key",
        ),
        RegisterError::NameTaken | RegisterError::ReservedName => error_response(
            StatusCode::CONFLICT,
            "conflict",
            "identity name is unavailable",
        ),
        RegisterError::Storage(message) => {
            tracing::error!("identity storage failure: {message}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "storage failure",
            )
        }
    }
}

fn json_body<T: for<'de> Deserialize<'de>>(
    payload: Result<Json<T>, JsonRejection>,
) -> Result<T, Response> {
    payload.map(|Json(value)| value).map_err(|_| {
        error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "malformed JSON body",
        )
    })
}

pub async fn create_project_handler(
    State(state): State<AppState>,
    payload: Result<Json<CreateProjectRequest>, JsonRejection>,
) -> Response {
    let req = match json_body(payload) {
        Ok(req) => req,
        Err(response) => return response,
    };
    let key = match parse_verifying_key(&req.public_key_pem) {
        Ok(key) => key,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "invalid public_key_pem",
            );
        }
    };
    let fingerprint = match auth::key_fingerprint(&key) {
        Ok(fp) => fp,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "invalid public_key_pem",
            );
        }
    };
    let creator_name = req.creator_name.clone();
    match tokio::task::spawn_blocking(move || {
        state
            .projects
            .create_project_with_creator(&req.public_key_pem, &req.creator_name)
    })
    .await
    {
        Ok(Ok(meta)) => (
            StatusCode::CREATED,
            Json(json!({
                "project_id": meta.project_id,
                "admission": {"name": creator_name, "fingerprint": fingerprint}
            })),
        )
            .into_response(),
        Ok(Err(err)) => project_error(err),
        Err(err) => project_error(ProjectError::Storage(format!("project task failed: {err}"))),
    }
}

pub async fn challenge_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    payload: Result<Json<ChallengeRequest>, JsonRejection>,
) -> Response {
    let req = match json_body(payload) {
        Ok(req) => req,
        Err(response) => return response,
    };
    let projects = state.projects.clone();
    let check_id = id.clone();
    match tokio::task::spawn_blocking(move || {
        let dir = projects.project_dir(&check_id)?;
        if !dir.join("project.json").is_file() {
            return Err(ProjectError::ProjectNotFound);
        }
        Ok(())
    })
    .await
    {
        Ok(Ok(())) => (),
        Ok(Err(err)) => return project_error(err),
        Err(err) => {
            return project_error(ProjectError::Storage(format!(
                "project lookup failed: {err}"
            )));
        }
    }
    let key = match parse_verifying_key(&req.public_key_pem) {
        Ok(key) => key,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "invalid public_key_pem",
            );
        }
    };
    let fingerprint = match auth::key_fingerprint(&key) {
        Ok(fp) => fp,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "invalid public_key_pem",
            );
        }
    };
    match state.challenges.issue(&id, &fingerprint) {
        Ok(issued) => (
            StatusCode::OK,
            Json(json!({
                "challenge_id": issued.challenge_id,
                "challenge": issued.challenge,
                "expires_in": issued.expires_in,
            })),
        )
            .into_response(),
        Err(AuthError::Entropy) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "entropy unavailable",
        ),
        Err(_) => error_response(StatusCode::UNAUTHORIZED, "unauthorized", "challenge failed"),
    }
}

pub async fn register_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    payload: Result<Json<RegisterRequest>, JsonRejection>,
) -> Response {
    let req = match json_body(payload) {
        Ok(req) => req,
        Err(response) => return response,
    };
    let key = match parse_verifying_key(&req.public_key_pem) {
        Ok(key) => key,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "invalid public_key_pem",
            );
        }
    };
    let fingerprint = match auth::key_fingerprint(&key) {
        Ok(fp) => fp,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "invalid public_key_pem",
            );
        }
    };
    let challenge = match state
        .challenges
        .consume(&id, &req.challenge_id, &fingerprint)
    {
        Ok(challenge) => challenge,
        Err(_) => {
            return error_response(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "unknown, expired, or mismatched challenge",
            );
        }
    };
    if !auth::verify_signature(&key, &challenge, &req.signature_base64) {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid challenge signature",
        );
    }
    let presented_spki = match canonical_spki_hex(&key) {
        Ok(spki) => spki,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "invalid public_key_pem",
            );
        }
    };
    let admissions = state.projects.admissions().clone();
    let project_id = id.clone();
    let name = req.name.clone();
    let admitted_spki =
        tokio::task::spawn_blocking(move || admissions.admitted_spki(&project_id, &name)).await;
    match admitted_spki {
        Ok(Some(admitted)) if admitted == presented_spki => {}
        Ok(_) => {
            return error_response(
                StatusCode::CONFLICT,
                "admission_required",
                "identity name and public key are not admitted for this project",
            );
        }
        Err(err) => {
            return register_error(RegisterError::Storage(format!(
                "admission lookup failed: {err}"
            )));
        }
    }
    let identities = state.identities.clone();
    let project_id = id;
    match tokio::task::spawn_blocking(move || {
        identities.register(&project_id, &req.name, &req.public_key_pem)
    })
    .await
    {
        Ok(Ok(outcome)) => (
            StatusCode::CREATED,
            Json(json!({
                "name": outcome.name,
                "token": outcome.token,
                "token_expires_at": outcome.expires_at,
                "fingerprint": outcome.fingerprint,
            })),
        )
            .into_response(),
        Ok(Err(err)) => register_error(err),
        Err(err) => register_error(RegisterError::Storage(format!(
            "identity task failed: {err}"
        ))),
    }
}

pub async fn directory_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    let projects = state.projects.clone();
    let project_id = id.clone();
    match tokio::task::spawn_blocking(move || {
        let dir = projects.project_dir(&project_id)?;
        if dir.join("project.json").is_file() {
            Ok(())
        } else {
            Err(ProjectError::ProjectNotFound)
        }
    })
    .await
    {
        Ok(Ok(())) => (),
        Ok(Err(err)) => return project_error(err),
        Err(err) => {
            return project_error(ProjectError::Storage(format!(
                "project lookup failed: {err}"
            )));
        }
    }

    let Some(token) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "bearer token required",
        );
    };
    let identities = state.identities.clone();
    let token = token.to_owned();
    let project_id = id.clone();
    match tokio::task::spawn_blocking(move || identities.verify_bearer(&project_id, &token)).await {
        Ok(Some(_)) => (),
        Ok(None) => {
            return error_response(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "invalid bearer token",
            );
        }
        Err(_) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "identity lookup failed",
            );
        }
    }

    let identities = state.identities.clone();
    match tokio::task::spawn_blocking(move || identities.list(&id)).await {
        Ok(members) => Json(json!({"members": members})).into_response(),
        Err(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "identity lookup failed",
        ),
    }
}

pub async fn items_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    Query(query): Query<ItemsQuery>,
) -> Response {
    let Some(token) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "bearer token required",
        );
    };
    let identities = state.identities.clone();
    let token = token.to_owned();
    let project_id = id.clone();
    let viewer =
        match tokio::task::spawn_blocking(move || identities.verify_bearer(&project_id, &token))
            .await
        {
            Ok(Some(identity)) => identity.name,
            Ok(None) => {
                return error_response(
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "invalid bearer token",
                );
            }
            Err(_) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "identity lookup failed",
                );
            }
        };
    let Some(cipher) = state.store_cipher else {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "storage encryption unavailable",
        );
    };
    let projects = state.projects.clone();
    match tokio::task::spawn_blocking(move || {
        projects.read_after_scoped(&cipher, &id, query.since, &viewer)
    })
    .await
    {
        Ok(Ok(page)) => {
            let records: Vec<_> = page
                .records
                .into_iter()
                .map(|record| {
                    json!({
                        "sequence": record.sequence,
                        "message_id": record.message_id,
                        "from": record.from,
                        "to": record.to,
                        "kind": record.kind,
                        "in_reply_to": record.in_reply_to,
                        "refs": record.refs,
                        "body": record.body,
                        "raw_eml": STANDARD.encode(record.raw_eml),
                    })
                })
                .collect();
            Json(json!({"records": records, "next_cursor": page.next_cursor})).into_response()
        }
        Ok(Err(err)) => project_error(err),
        Err(err) => project_error(ProjectError::Storage(format!(
            "project read task failed: {err}"
        ))),
    }
}

/// `POST /v1/projects/:id/items` — append one item under its client-minted
/// traveling Message-ID (bearer). The sender is always the authenticated
/// identity; a vouch addressed to postmaster is admitted at submit.
pub async fn append_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    payload: Result<Json<AppendRequest>, JsonRejection>,
) -> Response {
    let Some(token) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "bearer token required",
        );
    };
    let identities = state.identities.clone();
    let token = token.to_owned();
    let project_id = id.clone();
    let viewer =
        match tokio::task::spawn_blocking(move || identities.verify_bearer(&project_id, &token))
            .await
        {
            Ok(Some(identity)) => identity.name,
            Ok(None) => {
                return error_response(
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "invalid bearer token",
                );
            }
            Err(_) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "identity lookup failed",
                );
            }
        };
    let Some(cipher) = state.store_cipher else {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "storage encryption unavailable",
        );
    };
    let req = match json_body(payload) {
        Ok(req) => req,
        Err(response) => return response,
    };
    if body_over_cap(req.body.len()) {
        return error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "body_too_large",
            "body over 64 KiB",
        );
    }
    // Clients mint every id: a traveling Message-ID header is required for
    // every append, plain or kinded.
    let message_id = headers
        .get("message-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !is_traveling_id(message_id) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "a Message-ID header of the form msg_ plus 24 lowercase hex is required",
        );
    }
    // Kind rules mirror messages::normalize_extras (R8).
    let kind = match req.kind.as_deref().map(|k| k.trim().to_ascii_lowercase()) {
        None => None,
        Some(k) if k.is_empty() || k == "mail" => None,
        Some(k) if matches!(k.as_str(), "need" | "task" | "want") => Some(k),
        Some(k) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &format!("unsupported kind '{k}'; expected need, task, want, or mail"),
            );
        }
    };
    // The limiter keys on (room, u32 seat); fold the identity name into the
    // seat slot so the bucket stays per project plus member.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    viewer.hash(&mut hasher);
    let member_key = hasher.finish() as u32;
    if !state.limiter.allow_send(&id, member_key) {
        return error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "over 30 sends/min/identity",
        );
    }
    let item = AppendItem {
        id: message_id.to_owned(),
        from: viewer,
        to: req.to,
        subject: req.subject.unwrap_or_default(),
        body: req.body,
        kind,
        in_reply_to: req.in_reply_to,
        refs: req.refs,
    };
    let projects = state.projects.clone();
    match tokio::task::spawn_blocking(move || projects.append(&cipher, &id, &item)).await {
        Ok(Ok(outcome)) => {
            let mut body = json!({
                "message_id": outcome.item_id,
                "sequence": outcome.sequence,
            });
            if outcome.duplicate {
                body["duplicate"] = json!(true);
                return (StatusCode::OK, Json(body)).into_response();
            }
            (StatusCode::CREATED, Json(body)).into_response()
        }
        Ok(Err(err)) => project_error(err),
        Err(err) => project_error(ProjectError::Storage(format!(
            "project append task failed: {err}"
        ))),
    }
}
