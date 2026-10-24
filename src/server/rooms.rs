//! Lightweight chat rooms (phase-03 item 17): a private, short-lived
//! project with keyless join. The room id is the shareable capability.
//! Storage is the project mailspace (`DATA_DIR/projects/<id>/`), not a
//! second tenant. Chat letters are `kind=chat` (member-visible).

use std::hash::{Hash, Hasher};
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::DateTime;
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::identities::{IdentityRecord, RegisterError};
use crate::mcp::letters::{append_composed, compose_reply, compose_send, handle_of, parse_letter};
use crate::projects::{ListQuery, ProjectError, ProjectMeta, Viewer};
use crate::projects_api::{authenticate, json_body, project_error};

pub const DEFAULT_TTL_SECS: u64 = 24 * 60 * 60;
pub const MIN_TTL_SECS: u64 = 60;
pub const MAX_TTL_SECS: u64 = 7 * 24 * 60 * 60;
pub const MAX_CHAT_MEMBERS: usize = 64;
pub const MAX_WAIT_SECS: u64 = 25;

pub(crate) fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}

fn map_register(err: RegisterError) -> Response {
    match err {
        RegisterError::InvalidName => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "invalid identity name",
        ),
        RegisterError::ReservedName | RegisterError::NameTaken => error_response(
            StatusCode::CONFLICT,
            "name_taken",
            "that name is already in this chat",
        ),
        RegisterError::RoomFull => error_response(
            StatusCode::FORBIDDEN,
            "room_full",
            "this chat is at its member cap",
        ),
        RegisterError::PrefixTaken => error_response(
            StatusCode::CONFLICT,
            "mailspace_prefix_taken",
            "could not mint a unique address prefix",
        ),
        RegisterError::Storage(message) => {
            tracing::error!("chat identity storage failure: {message}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "storage failure",
            )
        }
        RegisterError::InvalidKey | RegisterError::InvalidMailspaceId => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "storage failure",
        ),
    }
}

fn parse_expiry(stamp: &str) -> Option<DateTime<chrono::Utc>> {
    DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|stamp| stamp.with_timezone(&chrono::Utc))
}

fn room_json(meta: &ProjectMeta, members: usize) -> serde_json::Value {
    json!({
        "room_id": meta.project_id,
        "project_id": meta.project_id,
        "mode": "chat",
        "ttl_seconds": meta.ttl_seconds,
        "expires_at": meta.expires_at,
        "closed": meta.closed,
        "expired": meta.is_over() && !meta.closed,
        "members": members,
        "mcp": format!("/v1/projects/{}/mcp", meta.project_id),
    })
}

fn join_json(
    meta: &ProjectMeta,
    name: &str,
    token: &str,
    token_expires_at: &str,
) -> serde_json::Value {
    let mut body = room_json(meta, 0);
    body["name"] = json!(name);
    body["token"] = json!(token);
    body["token_expires_at"] = json!(token_expires_at);
    body
}

#[derive(Debug, Default, Deserialize)]
pub struct CreateRoomRequest {
    pub name: String,
    pub ttl_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct JoinRoomRequest {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct SendChatRequest {
    pub body: String,
    pub subject: Option<String>,
    pub in_reply_to: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PollQuery {
    pub since: Option<u64>,
    pub wait: Option<u64>,
}

fn ttl_of(raw: Option<u64>) -> Result<u64, Response> {
    let ttl = raw.unwrap_or(DEFAULT_TTL_SECS);
    if !(MIN_TTL_SECS..=MAX_TTL_SECS).contains(&ttl) {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            &format!("ttl_seconds must be {MIN_TTL_SECS}..{MAX_TTL_SECS}"),
        ));
    }
    Ok(ttl)
}

/// `POST /v1/rooms` — create a chat project and join the creator by name.
pub async fn create_room_handler(
    State(state): State<AppState>,
    payload: Result<Json<CreateRoomRequest>, JsonRejection>,
) -> Response {
    let req = match json_body(payload) {
        Ok(req) => req,
        Err(response) => return response,
    };
    let ttl = match ttl_of(req.ttl_seconds) {
        Ok(ttl) => ttl,
        Err(response) => return response,
    };
    let name = req.name;
    let result = tokio::task::spawn_blocking(move || {
        let meta = state.projects.create_chat_project(ttl)?;
        let expires_at = meta
            .expires_at
            .as_deref()
            .and_then(parse_expiry)
            .ok_or_else(|| ProjectError::Storage("chat project missing expires_at".into()))?;
        let outcome = state
            .identities
            .register_guest(&meta.project_id, &name, expires_at, MAX_CHAT_MEMBERS)
            .map_err(|err| match err {
                RegisterError::Storage(message) => ProjectError::Storage(message),
                other => ProjectError::BadRequest(format!("{other:?}")),
            })?;
        Ok::<_, ProjectError>((meta, outcome))
    })
    .await
    .unwrap_or_else(|e| Err(ProjectError::Storage(format!("create chat failed: {e}"))));
    match result {
        Ok((meta, outcome)) => {
            let mut body = join_json(&meta, &outcome.name, &outcome.token, &outcome.expires_at);
            body["members"] = json!(1);
            (StatusCode::CREATED, Json(body)).into_response()
        }
        Err(ProjectError::BadRequest(message)) if message.contains("NameTaken") => {
            map_register(RegisterError::NameTaken)
        }
        Err(ProjectError::BadRequest(message)) if message.contains("InvalidName") => {
            map_register(RegisterError::InvalidName)
        }
        Err(ProjectError::BadRequest(message)) if message.contains("ReservedName") => {
            map_register(RegisterError::ReservedName)
        }
        Err(ProjectError::BadRequest(message)) if message.contains("RoomFull") => {
            map_register(RegisterError::RoomFull)
        }
        Err(err) => project_error(err),
    }
}

/// `POST /v1/rooms/:id/join` — keyless join: the room id is the capability.
pub async fn join_room_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    payload: Result<Json<JoinRoomRequest>, JsonRejection>,
) -> Response {
    let req = match json_body(payload) {
        Ok(req) => req,
        Err(response) => return response,
    };
    let name = req.name;
    let identities = state.identities.clone();
    let result = tokio::task::spawn_blocking(move || {
        let meta = state.projects.refuse_if_chat_over(&id)?;
        if !meta.is_chat() {
            return Err(ProjectError::ProjectNotFound);
        }
        let expires_at = meta
            .expires_at
            .as_deref()
            .and_then(parse_expiry)
            .ok_or_else(|| ProjectError::Storage("chat project missing expires_at".into()))?;
        match state
            .identities
            .register_guest(&meta.project_id, &name, expires_at, MAX_CHAT_MEMBERS)
        {
            Ok(outcome) => {
                let members = identities.member_count(&meta.project_id);
                Ok((meta, outcome, members))
            }
            Err(RegisterError::Storage(message)) => Err(ProjectError::Storage(message)),
            Err(other) => Err(ProjectError::BadRequest(format!("{other:?}"))),
        }
    })
    .await
    .unwrap_or_else(|e| Err(ProjectError::Storage(format!("join chat failed: {e}"))));
    match result {
        Ok((meta, outcome, members)) => {
            let mut body = join_json(&meta, &outcome.name, &outcome.token, &outcome.expires_at);
            body["members"] = json!(members);
            (StatusCode::CREATED, Json(body)).into_response()
        }
        Err(ProjectError::BadRequest(message)) if message.contains("NameTaken") => {
            map_register(RegisterError::NameTaken)
        }
        Err(ProjectError::BadRequest(message)) if message.contains("InvalidName") => {
            map_register(RegisterError::InvalidName)
        }
        Err(ProjectError::BadRequest(message)) if message.contains("ReservedName") => {
            map_register(RegisterError::ReservedName)
        }
        Err(ProjectError::BadRequest(message)) if message.contains("RoomFull") => {
            map_register(RegisterError::RoomFull)
        }
        Err(ProjectError::InvalidProjectId) => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_room_id",
            "id not canonical Crockford-base32",
        ),
        Err(ProjectError::ProjectNotFound) => {
            error_response(StatusCode::NOT_FOUND, "room_not_found", "unknown room id")
        }
        Err(err) => project_error(err),
    }
}

/// `GET /v1/rooms/:id` — metadata. No auth: the id is the capability.
/// An expired-but-not-yet-swept room still reports `expired: true`.
pub async fn room_meta_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let projects = state.projects.clone();
    let identities = state.identities.clone();
    match tokio::task::spawn_blocking(move || {
        let meta = projects.load_meta(&id)?;
        if !meta.is_chat() {
            return Err(ProjectError::ProjectNotFound);
        }
        let members = identities.member_count(&meta.project_id);
        Ok((meta, members))
    })
    .await
    .unwrap_or_else(|e| Err(ProjectError::Storage(format!("room meta failed: {e}"))))
    {
        Ok((meta, members)) => Json(room_json(&meta, members)).into_response(),
        Err(err) => match err {
            ProjectError::ProjectNotFound | ProjectError::InvalidProjectId => error_response(
                if matches!(err, ProjectError::InvalidProjectId) {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::NOT_FOUND
                },
                if matches!(err, ProjectError::InvalidProjectId) {
                    "invalid_room_id"
                } else {
                    "room_not_found"
                },
                if matches!(err, ProjectError::InvalidProjectId) {
                    "id not canonical Crockford-base32"
                } else {
                    "unknown room id"
                },
            ),
            other => project_error(other),
        },
    }
}

/// `POST /v1/rooms/:id/close` — any member can close the chat.
pub async fn close_room_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_chat_member(&state, &id, &headers).await {
        return response;
    }
    let projects = state.projects.clone();
    let identities = state.identities.clone();
    match tokio::task::spawn_blocking(move || {
        let meta = projects.close_chat(&id)?;
        let members = identities.member_count(&meta.project_id);
        Ok((meta, members))
    })
    .await
    .unwrap_or_else(|e| Err(ProjectError::Storage(format!("close chat failed: {e}"))))
    {
        Ok((meta, members)) => Json(room_json(&meta, members)).into_response(),
        Err(err) => project_error(err),
    }
}

async fn require_chat_member(
    state: &AppState,
    id: &str,
    headers: &HeaderMap,
) -> Result<(ProjectMeta, IdentityRecord), Response> {
    let projects = state.projects.clone();
    let check_id = id.to_owned();
    let meta = match tokio::task::spawn_blocking(move || projects.load_meta(&check_id)).await {
        Ok(Ok(meta)) if meta.is_chat() => meta,
        Ok(Ok(_)) | Ok(Err(ProjectError::ProjectNotFound)) => {
            return Err(error_response(
                StatusCode::NOT_FOUND,
                "room_not_found",
                "unknown room id",
            ));
        }
        Ok(Err(ProjectError::InvalidProjectId)) => {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_room_id",
                "id not canonical Crockford-base32",
            ));
        }
        Ok(Err(err)) => return Err(project_error(err)),
        Err(err) => {
            return Err(project_error(ProjectError::Storage(format!(
                "room lookup failed: {err}"
            ))));
        }
    };
    if meta.is_over() {
        return Err(project_error(ProjectError::Gone {
            closed: meta.closed,
        }));
    }
    let identity = authenticate(state, id, headers).await?;
    Ok((meta, identity))
}

fn rate_limit(state: &AppState, project_id: &str, name: &str) -> Result<(), Response> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    if state.limiter.allow_send(project_id, hasher.finish() as u32) {
        Ok(())
    } else {
        Err(error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            &format!(
                "over {} sends/min/identity",
                state.limiter.limit().unwrap_or(0)
            ),
        ))
    }
}

/// `POST /v1/rooms/:id/messages` — send a member-visible `kind=chat` letter.
pub async fn send_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    payload: Result<Json<SendChatRequest>, JsonRejection>,
) -> Response {
    let req = match json_body(payload) {
        Ok(req) => req,
        Err(response) => return response,
    };
    if req.body.trim().is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "body must be non-empty",
        );
    }
    let (_, identity) = match require_chat_member(&state, &id, &headers).await {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    if let Err(response) = rate_limit(&state, &id, &identity.name) {
        return response;
    }
    let projects = state.projects.clone();
    let in_reply_to = req.in_reply_to.clone();
    let subject = req
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("chat")
        .to_owned();
    let body = req.body;
    match tokio::task::spawn_blocking(move || {
        let prefix = identity
            .mailspace_prefix()
            .ok_or_else(|| "guest is missing a mailspace prefix".to_owned())?;
        let composed = if let Some(handle) = in_reply_to.as_deref() {
            let viewer = Viewer::of(&identity);
            let found = projects
                .lookup_handles(&id, &[handle.to_owned()], &viewer)
                .map_err(|e| format!("{e:?}"))?;
            let parent = found
                .records
                .into_iter()
                .next()
                .ok_or_else(|| "in_reply_to did not match a visible letter".to_owned())?;
            compose_reply(&identity, &parent, &body)
        } else {
            let to = format!("chat@{prefix}.local");
            compose_send(&identity, &[to], &subject, &body, "chat")
        };
        let composed = composed?;
        append_composed(&projects, &id, &identity, composed).map_err(|e| format!("{e:?}"))
    })
    .await
    {
        Ok(Ok(outcome)) => (
            StatusCode::CREATED,
            Json(json!({
                "message_id": outcome.message_id,
                "handle": handle_of(&outcome.message_id),
                "sequence": outcome.sequence,
                "kind": "chat",
                "duplicate": outcome.duplicate,
            })),
        )
            .into_response(),
        Ok(Err(message)) => error_response(StatusCode::BAD_REQUEST, "invalid_request", &message),
        Err(err) => project_error(ProjectError::Storage(format!("chat send failed: {err}"))),
    }
}

/// `GET /v1/rooms/:id/messages` — poll / long-poll member-visible chat.
pub async fn poll_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    Query(params): Query<PollQuery>,
) -> Response {
    let (_, identity) = match require_chat_member(&state, &id, &headers).await {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let since = params.since.unwrap_or(0);
    let wait = params.wait.unwrap_or(0).min(MAX_WAIT_SECS);
    if let Some(page) = match read_chat(&state, &id, &identity, since).await {
        Ok(page) => page,
        Err(response) => return response,
    } {
        if !page["messages"].as_array().is_some_and(|m| m.is_empty()) || wait == 0 {
            return Json(page).into_response();
        }
    }
    if wait > 0 {
        state
            .projects
            .wake()
            .wait(&id, Duration::from_secs(wait))
            .await;
    }
    match read_chat(&state, &id, &identity, since).await {
        Ok(Some(page)) => Json(page).into_response(),
        Ok(None) => Json(json!({ "messages": [], "next_cursor": since })).into_response(),
        Err(response) => response,
    }
}

async fn read_chat(
    state: &AppState,
    id: &str,
    identity: &IdentityRecord,
    since: u64,
) -> Result<Option<serde_json::Value>, Response> {
    let projects = state.projects.clone();
    let identity = identity.clone();
    let project_id = id.to_owned();
    match tokio::task::spawn_blocking(move || {
        let query = ListQuery::parse(since, None, None, Some("chat"), None)?;
        let page = projects.read_page(&project_id, &query, &Viewer::of(&identity))?;
        let messages: Vec<_> = page
            .records
            .iter()
            .map(|record| {
                let letter = parse_letter(&record.raw_eml);
                json!({
                    "message_id": record.message_id,
                    "handle": handle_of(&record.message_id),
                    "sequence": record.sequence,
                    "from": record.from,
                    "subject": letter.subject,
                    "body": letter.body.trim(),
                    "created_at": record.appended_at,
                })
            })
            .collect();
        Ok::<_, ProjectError>(json!({
            "messages": messages,
            "next_cursor": page.next_cursor,
            "epoch": page.epoch,
        }))
    })
    .await
    {
        Ok(Ok(page)) => Ok(Some(page)),
        Ok(Err(err)) => Err(project_error(err)),
        Err(err) => Err(project_error(ProjectError::Storage(format!(
            "chat poll failed: {err}"
        )))),
    }
}
