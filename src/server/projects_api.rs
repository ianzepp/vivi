//! Project creation, identity proof-of-possession, and item read endpoints.

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::rejection::{BytesRejection, JsonRejection};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::json;
use std::hash::{Hash, Hasher};
use vivi::storage::is_traveling_id;

use crate::AppState;
use crate::auth::{self, AuthError, canonical_spki_hex, parse_verifying_key};
use crate::identities::{IdentityRecord, RefreshError, RegisterError, valid_mailspace_id};
use crate::keys::{Caller, resolve_project_caller};
use crate::projects::{AppendItem, ListQuery, ProjectError, ProjectRecord, Viewer};
use crate::rooms::error_response;

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
    /// The registering checkout's mailspace id (canonical UUID).
    mailspace_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ItemsQuery {
    #[serde(default)]
    since: u64,
    // Raw strings so a malformed value is our `invalid_request`, not the
    // extractor's plain-text rejection.
    limit: Option<String>,
    after: Option<String>,
    kind: Option<String>,
    to: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LookupRequest {
    handles: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct TokenRequest {
    name: String,
    challenge_id: String,
    signature_base64: String,
}

pub(crate) fn project_error(err: ProjectError) -> Response {
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
        ProjectError::TooLarge => error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "body_too_large",
            "letter over 64 KiB",
        ),
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
        ProjectError::ReadOnly(message) => {
            error_response(StatusCode::FORBIDDEN, "project_read_only", &message)
        }
        ProjectError::Gone { closed: true } => {
            error_response(StatusCode::GONE, "room_closed", "this chat is closed")
        }
        ProjectError::Gone { closed: false } => {
            error_response(StatusCode::GONE, "room_expired", "this chat has expired")
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
        RegisterError::InvalidMailspaceId => invalid_mailspace_id(),
        RegisterError::PrefixTaken => error_response(
            StatusCode::CONFLICT,
            "mailspace_prefix_taken",
            "another identity in this project already holds that mailspace prefix",
        ),
        RegisterError::NameTaken | RegisterError::ReservedName => error_response(
            StatusCode::CONFLICT,
            "conflict",
            "identity name is unavailable",
        ),
        RegisterError::RoomFull => error_response(
            StatusCode::FORBIDDEN,
            "room_full",
            "this chat is at its member cap",
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

fn invalid_mailspace_id() -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "mailspace_id must be a lowercase hyphenated UUID",
    )
}

pub(crate) fn json_body<T: for<'de> Deserialize<'de>>(
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

pub(crate) fn presented_bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

pub(crate) fn missing_bearer() -> Response {
    error_response(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "bearer token required",
    )
}

pub(crate) fn invalid_bearer() -> Response {
    error_response(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "invalid bearer token",
    )
}

pub(crate) fn identity_lookup_failed() -> Response {
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "identity lookup failed",
    )
}

pub(crate) async fn require_existing_project(state: &AppState, id: &str) -> Result<(), Response> {
    let projects = state.projects.clone();
    let check_id = id.to_owned();
    match tokio::task::spawn_blocking(move || projects.existing_project_dir(&check_id)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => Err(project_error(err)),
        Err(err) => Err(project_error(ProjectError::Storage(format!(
            "project lookup failed: {err}"
        )))),
    }
}

/// Durable projects always pass. A closed or expired chat is `410`.
pub(crate) async fn require_live_chat(state: &AppState, id: &str) -> Result<(), Response> {
    let projects = state.projects.clone();
    let check_id = id.to_owned();
    match tokio::task::spawn_blocking(move || projects.refuse_if_chat_over(&check_id)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => Err(project_error(err)),
        Err(err) => Err(project_error(ProjectError::Storage(format!(
            "project lookup failed: {err}"
        )))),
    }
}

/// Identity-token or API-key Bearer, resolved to the bound identity.
pub(crate) async fn authenticate(
    state: &AppState,
    project_id: &str,
    headers: &HeaderMap,
) -> Result<IdentityRecord, Response> {
    authenticate_caller(state, project_id, headers)
        .await
        .map(|caller| caller.identity)
}

/// As [`authenticate`], keeping the vivi identity an API key acts as.
pub(crate) async fn authenticate_caller(
    state: &AppState,
    project_id: &str,
    headers: &HeaderMap,
) -> Result<Caller, Response> {
    let Some(token) = presented_bearer(headers) else {
        return Err(missing_bearer());
    };
    let identities = state.identities.clone();
    let keys = state.keys.clone();
    let project_id = project_id.to_owned();
    let token = token.to_owned();
    match tokio::task::spawn_blocking(move || {
        resolve_project_caller(&identities, &keys, &project_id, &token)
    })
    .await
    {
        Ok(Some(caller)) => Ok(caller),
        Ok(None) => Err(invalid_bearer()),
        Err(_) => Err(identity_lookup_failed()),
    }
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
    match tokio::task::spawn_blocking(move || projects.existing_project_dir(&check_id).map(|_| ()))
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
    // A shape check, made before the challenge is spent.
    if !valid_mailspace_id(&req.mailspace_id) {
        return invalid_mailspace_id();
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
        identities.register(
            &project_id,
            &req.name,
            &req.public_key_pem,
            &req.mailspace_id,
        )
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
                "mailspace_id": outcome.mailspace_id,
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
        projects.existing_project_dir(&project_id).map(|_| ())
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

    if let Err(response) = authenticate(&state, &id, &headers).await {
        return response;
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

/// The item-event capability advertised on every list response as the
/// top-level field `item_events`. `1` means this server folds `X-Vivi-Event`
/// letters (`done`, `reopened`, `absorbed`, `marked`, `unmarked`, `bound`) by
/// the client's rule — the newest event against an item controls — serves
/// them to every member by their header, and never lists, counts, or routes
/// one as work. Advisory: the client sends event letters as kind `mail`
/// (which a server that predates this files as plain mail) and does not
/// read the field.
pub const ITEM_EVENTS: u32 = 1;

/// `GET /v1/projects/:id/items` — the list of letters visible to the caller
/// after `since`, with `next_cursor`, `has_more`, and `item_events`.
pub async fn items_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    Query(query): Query<ItemsQuery>,
) -> Response {
    let identity = match authenticate(&state, &id, &headers).await {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    let list = match ListQuery::parse(
        query.since,
        query.limit.as_deref(),
        query.after.as_deref(),
        query.kind.as_deref(),
        query.to.as_deref(),
    ) {
        Ok(list) => list,
        Err(err) => return project_error(err),
    };
    let projects = state.projects.clone();
    match tokio::task::spawn_blocking(move || {
        projects.read_page(&id, &list, &Viewer::of(&identity))
    })
    .await
    {
        Ok(Ok(page)) => {
            let records: Vec<_> = page.records.into_iter().map(record_json).collect();
            Json(json!({
                "records": records,
                "next_cursor": page.next_cursor,
                "has_more": page.has_more,
                "epoch": page.epoch,
                "item_events": ITEM_EVENTS,
            }))
            .into_response()
        }
        Ok(Err(err)) => project_error(err),
        Err(err) => project_error(ProjectError::Storage(format!(
            "project read task failed: {err}"
        ))),
    }
}

/// The list record shape, shared by the list and the handle lookup.
fn record_json(record: ProjectRecord) -> serde_json::Value {
    json!({
        "sequence": record.sequence,
        "message_id": record.message_id,
        "from": record.from,
        "from_prefix": record.from_prefix,
        "to": record.to,
        "kind": record.kind,
        "content_sha256": record.content_sha256,
        "appended_at": record.appended_at,
        "raw_eml": STANDARD.encode(record.raw_eml),
    })
}

/// `POST /v1/projects/:id/items/lookup` — fetch letters by handle (the 8 hex
/// after `msg_`), scoped by the list's visibility rule. A handle that names
/// nothing the caller may see is `missing`, the same whether the id is
/// unknown or hidden. Never moves a cursor.
pub async fn lookup_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    payload: Result<Json<LookupRequest>, JsonRejection>,
) -> Response {
    let req = match json_body(payload) {
        Ok(req) => req,
        Err(response) => return response,
    };
    let projects = state.projects.clone();
    let check_id = id.clone();
    match tokio::task::spawn_blocking(move || projects.existing_project_dir(&check_id)).await {
        Ok(Ok(_)) => (),
        Ok(Err(err)) => return project_error(err),
        Err(err) => {
            return project_error(ProjectError::Storage(format!(
                "project lookup failed: {err}"
            )));
        }
    }
    let identity = match authenticate(&state, &id, &headers).await {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    let projects = state.projects.clone();
    let looked_up = tokio::task::spawn_blocking(move || {
        projects.lookup_handles(&id, &req.handles, &Viewer::of(&identity))
    })
    .await;
    match looked_up {
        Ok(Ok(found)) => {
            let records: Vec<_> = found.records.into_iter().map(record_json).collect();
            Json(json!({"records": records, "missing": found.missing})).into_response()
        }
        Ok(Err(err)) => project_error(err),
        Err(err) => project_error(ProjectError::Storage(format!(
            "project lookup task failed: {err}"
        ))),
    }
}

/// Body limit for the append route, a little above the 64 KiB letter cap so
/// an oversize body reaches this module's own 413 instead of the extractor's.
pub const APPEND_BODY_LIMIT: usize = 70 * 1024;

/// One optional envelope header: absent is `None`; a value that is not
/// plain ASCII is a 400.
fn envelope_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, Response> {
    match headers.get(name).map(|v| v.to_str()) {
        None => Ok(None),
        Some(Ok(value)) => Ok(Some(value)),
        Some(Err(_)) => Err(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            &format!("{name} must be ASCII"),
        )),
    }
}

/// `PUT /v1/projects/:id/items/:message_id` — append one letter under its
/// client-minted traveling id (bearer). The request body is the letter, kept
/// exactly: no wrapping, no encoding, and `Content-Type` is ignored. The
/// envelope rides in `X-Vivi-To` and `X-Vivi-Kind`; the sender is always the
/// authenticated identity, and a letter addressed to postmaster is a vouch,
/// admitted at submit.
pub async fn append_handler(
    State(state): State<AppState>,
    AxumPath((id, message_id)): AxumPath<(String, String)>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let identity = match authenticate(&state, &id, &headers).await {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    let raw_eml = match body {
        Ok(bytes) => bytes.to_vec(),
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return project_error(ProjectError::TooLarge);
        }
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "request body could not be read",
            );
        }
    };
    let to = match envelope_header(&headers, "x-vivi-to") {
        Ok(to) => to,
        Err(response) => return response,
    };
    let kind = match envelope_header(&headers, "x-vivi-kind") {
        Ok(kind) => kind,
        Err(response) => return response,
    };
    // Id, size, kind, and recipient rules live in `AppendItem::new`.
    let from_prefix = identity.mailspace_prefix().map(str::to_owned);
    let viewer = identity.name;
    let item = match AppendItem::new(&message_id, &viewer, to, kind, raw_eml) {
        Ok(item) => item.with_from_prefix(from_prefix.as_deref()),
        Err(err) => return project_error(err),
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
    let projects = state.projects.clone();
    match tokio::task::spawn_blocking(move || projects.append(&id, &item)).await {
        Ok(Ok(outcome)) => {
            let mut body = json!({
                "message_id": outcome.message_id,
                "sequence": outcome.sequence,
                "content_sha256": outcome.content_sha256,
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

/// `GET /v1/projects/:id/items/:message_id` — fetch one letter as the raw
/// `.eml` body (bearer), with its envelope in `X-Vivi-*` response headers.
/// A letter the caller may not see and an unknown id are the same 404.
pub async fn letter_handler(
    State(state): State<AppState>,
    AxumPath((id, message_id)): AxumPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let projects = state.projects.clone();
    let check_id = id.clone();
    match tokio::task::spawn_blocking(move || projects.existing_project_dir(&check_id)).await {
        Ok(Ok(_)) => (),
        Ok(Err(err)) => return project_error(err),
        Err(err) => {
            return project_error(ProjectError::Storage(format!(
                "project lookup failed: {err}"
            )));
        }
    }
    let identity = match authenticate(&state, &id, &headers).await {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    if !is_traveling_id(&message_id) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "message id must be msg_ plus 24 lowercase hex",
        );
    }
    let projects = state.projects.clone();
    let read = tokio::task::spawn_blocking(move || {
        projects.read_one_scoped(&id, &message_id, &Viewer::of(&identity))
    })
    .await;
    match read {
        Ok(Ok(Some(record))) => letter_response(record),
        Ok(Ok(None)) => error_response(StatusCode::NOT_FOUND, "item_not_found", "no such letter"),
        Ok(Err(err)) => project_error(err),
        Err(err) => project_error(ProjectError::Storage(format!(
            "project read task failed: {err}"
        ))),
    }
}

/// The 200 response for one letter: exact bytes, envelope in headers.
fn letter_response(record: ProjectRecord) -> Response {
    let mut response = Response::new(Body::from(record.raw_eml));
    let headers = response.headers_mut();
    let mut put = |name: &'static str, value: &str| match HeaderValue::from_str(value) {
        Ok(value) => {
            headers.insert(name, value);
            true
        }
        Err(_) => false,
    };
    let mut ok = put("content-type", "message/rfc822")
        && put("x-vivi-message-id", &record.message_id)
        && put("x-vivi-from", &record.from)
        && put("x-vivi-sequence", &record.sequence.to_string())
        && put("x-vivi-content-sha256", &record.content_sha256)
        && put("x-vivi-appended-at", &record.appended_at)
        && put("cache-control", "no-store");
    if let Some(prefix) = &record.from_prefix {
        ok = ok && put("x-vivi-from-prefix", prefix);
    }
    if let Some(to) = &record.to {
        ok = ok && put("x-vivi-to", to);
    }
    if let Some(kind) = &record.kind {
        ok = ok && put("x-vivi-kind", kind);
    }
    if !ok {
        // Names and kinds are validated before they are stored.
        return project_error(ProjectError::Storage(
            "stored envelope is not a valid header value".to_owned(),
        ));
    }
    response
}

/// `POST /v1/projects/:id/auth/token` — renew the bearer for a registered
/// identity. The challenge must have been issued for the key bound to
/// `name` and the signature must verify under it; every mismatch is the
/// same 401, so nothing says which part failed.
pub async fn token_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    payload: Result<Json<TokenRequest>, JsonRejection>,
) -> Response {
    let req = match json_body(payload) {
        Ok(req) => req,
        Err(response) => return response,
    };
    let projects = state.projects.clone();
    let check_id = id.clone();
    match tokio::task::spawn_blocking(move || projects.existing_project_dir(&check_id)).await {
        Ok(Ok(_)) => (),
        Ok(Err(err)) => return project_error(err),
        Err(err) => {
            return project_error(ProjectError::Storage(format!(
                "project lookup failed: {err}"
            )));
        }
    }
    let identities = state.identities.clone();
    let challenges = state.challenges.clone();
    let renewed = tokio::task::spawn_blocking(move || {
        identities.refresh_token(
            &id,
            &req.name,
            &req.challenge_id,
            &req.signature_base64,
            &challenges,
        )
    })
    .await;
    match renewed {
        Ok(Ok((token, expires_at))) => (
            StatusCode::OK,
            Json(json!({"token": token, "token_expires_at": expires_at})),
        )
            .into_response(),
        Ok(Err(RefreshError::Unauthorized)) => error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "token renewal failed",
        ),
        Ok(Err(RefreshError::Storage(message))) => {
            tracing::error!("identity storage failure: {message}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "storage failure",
            )
        }
        Err(err) => register_error(RegisterError::Storage(format!("token task failed: {err}"))),
    }
}
