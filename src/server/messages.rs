//! v1 message endpoints (VC-4): bearer-authenticated send and poll/long-poll
//! per docs/contract.md. Sends store real mail items through the vivi
//! mailspace (`Mailspace::send`); the room cursor, origin records, and
//! Message-ID dedup live in a per-room sidecar index (`messages.json`).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use vivi::mailspace::{Mailspace, SendRequest};

use crate::AppState;
use crate::auth::rfc3339;
use crate::crypto::{StoreCipher, set_owner_only};
use crate::rooms::{RoomError, error_response, map_room_error};
use crate::safety::body_over_cap;
use crate::seats::SeatRecord;

/// Contract: `wait` is clamped to 0..25 seconds.
pub const MAX_WAIT_SECS: u64 = 25;
const POLL_TICK: Duration = Duration::from_millis(250);
const MAX_MESSAGE_ID_LEN: usize = 256;

/// One delivered chat message as served by poll (content already decrypted).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRecord {
    pub cursor: u64,
    pub message_id: String,
    pub from: String,
    pub fingerprint: String,
    pub to: Option<String>,
    pub subject: String,
    pub body: String,
    pub created_at: String,
}

/// Content fields sealed into `enc` at rest (VC-5). The vivi mailspace mail
/// item keeps headers/metadata only; its body field carries the same sealed
/// blob so no plaintext content lands in mail.sqlite either.
#[derive(Debug, Serialize, Deserialize)]
struct StoredContent {
    to: Option<String>,
    subject: String,
    body: String,
}

/// On-disk sidecar row: metadata in the clear, content sealed as `enc`
/// (base64 nonce||ciphertext over `StoredContent`, AES-256-GCM). Rows
/// written before VC-5 carry plaintext content and no `enc` field.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredRecord {
    cursor: u64,
    message_id: String,
    from: String,
    fingerprint: String,
    created_at: String,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    subject: String,
    #[serde(default)]
    body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enc: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ChatIndex {
    records: Vec<StoredRecord>,
}

#[derive(Debug, Deserialize)]
pub struct SendMessageBody {
    pub to: Option<String>,
    pub subject: Option<String>,
    pub body: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct PollQuery {
    pub since: Option<u64>,
    pub wait: Option<u64>,
}

fn messages_lock() -> MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn index_path(room_dir: &Path) -> PathBuf {
    room_dir.join("messages.json")
}

fn read_index(room_dir: &Path) -> Result<ChatIndex, RoomError> {
    let bytes = match std::fs::read(index_path(room_dir)) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(ChatIndex::default()),
    };
    serde_json::from_slice(&bytes)
        .map_err(|e| RoomError::Storage(format!("bad messages.json: {e}")))
}

fn write_index(room_dir: &Path, index: &ChatIndex) -> Result<(), RoomError> {
    let bytes = serde_json::to_vec(index)
        .map_err(|e| RoomError::Storage(format!("failed to encode messages.json: {e}")))?;
    let path = index_path(room_dir);
    std::fs::write(&path, &bytes)
        .map_err(|e| RoomError::Storage(format!("failed to write messages.json: {e}")))?;
    set_owner_only(&path);
    Ok(())
}

/// Decrypt one stored row into the served record. Encrypted rows require the
/// cipher; legacy plaintext rows (no `enc`) pass through untouched.
fn decode_record(
    stored: StoredRecord,
    cipher: Option<&StoreCipher>,
) -> Result<ChatRecord, RoomError> {
    match stored.enc {
        Some(token) => {
            let cipher = cipher.ok_or_else(|| {
                RoomError::Storage(
                    "messages.json holds encrypted records but STORE_ENCRYPTION_KEY is not set"
                        .to_owned(),
                )
            })?;
            let plain = cipher
                .decrypt(&token)
                .map_err(|e| RoomError::Storage(format!("message decrypt failed: {e}")))?;
            let content: StoredContent = serde_json::from_slice(&plain).map_err(|e| {
                RoomError::Storage(format!("sealed message content malformed: {e}"))
            })?;
            Ok(ChatRecord {
                cursor: stored.cursor,
                message_id: stored.message_id,
                from: stored.from,
                fingerprint: stored.fingerprint,
                to: content.to,
                subject: content.subject,
                body: content.body,
                created_at: stored.created_at,
            })
        }
        None => Ok(ChatRecord {
            cursor: stored.cursor,
            message_id: stored.message_id,
            from: stored.from,
            fingerprint: stored.fingerprint,
            to: stored.to,
            subject: stored.subject,
            body: stored.body,
            created_at: stored.created_at,
        }),
    }
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

fn unauthorized(message: &str) -> Response {
    error_response(StatusCode::UNAUTHORIZED, "unauthorized", message)
}

/// Resolve a bearer token to its seat record and verify the origin headers
/// against it (contract: reject messages claiming a label the seat does not
/// hold; fingerprint is the seat fingerprint's short 16-hex form).
fn authenticate(
    state: &AppState,
    room_id: &str,
    headers: &HeaderMap,
) -> Result<SeatRecord, Response> {
    let token = header_value(headers, "authorization")
        .and_then(|v| v.strip_prefix("Bearer ").map(str::to_owned))
        .ok_or_else(|| unauthorized("missing bearer token"))?;
    let seat = state
        .seats
        .bearer_seat(room_id, &token)
        .ok_or_else(|| unauthorized("invalid or expired bearer token"))?;
    let short_fingerprint = &seat.fingerprint[..seat.fingerprint.len().min(16)];
    let from_ok = header_value(headers, "x-vivi-chat-from").as_deref() == Some(seat.label.as_str());
    let fp_ok =
        header_value(headers, "x-vivi-chat-fingerprint").as_deref() == Some(short_fingerprint);
    if !from_ok || !fp_ok {
        return Err(unauthorized(
            "origin headers do not match the authenticated seat",
        ));
    }
    Ok(seat)
}

fn resolve_room(state: &AppState, raw_id: &str) -> Result<(String, PathBuf), Response> {
    let meta = state.rooms.room_meta(raw_id).map_err(map_room_error)?;
    let dir = state.rooms.room_dir(raw_id).map_err(map_room_error)?;
    Ok((meta.room_id, dir))
}

/// `POST /v1/rooms/:id/messages` — send (bearer). Body cap, per-seat rate
/// limit, Message-ID dedup, then a real mailspace send.
pub async fn send_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    payload: Result<Json<SendMessageBody>, JsonRejection>,
) -> Response {
    let req = match payload {
        Ok(Json(req)) => req,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "malformed JSON body",
            );
        }
    };
    let (room_id, dir) = match resolve_room(&state, &id) {
        Ok(room) => room,
        Err(response) => return response,
    };
    let seat = match authenticate(&state, &room_id, &headers) {
        Ok(seat) => seat,
        Err(response) => return response,
    };
    if body_over_cap(req.body.len()) {
        return error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "body_too_large",
            "body over 64 KiB",
        );
    }
    if !state.limiter.allow_send(&room_id, seat.seat) {
        return error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "over 30 sends/min/seat",
        );
    }
    let client_message_id = header_value(&headers, "message-id")
        .filter(|v| !v.is_empty() && v.len() <= MAX_MESSAGE_ID_LEN);
    let result = tokio::task::spawn_blocking(move || {
        store_message(&state, &dir, &room_id, &seat, req, client_message_id)
    })
    .await
    .unwrap_or_else(|e| Err(RoomError::Storage(format!("send task failed: {e}"))));
    match result {
        Ok((record, duplicate)) => {
            let mut body = json!({
                "message_id": record.message_id,
                "cursor": record.cursor,
                "created_at": record.created_at,
            });
            if duplicate {
                body["duplicate"] = json!(true);
            }
            let status = if duplicate {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            (status, Json(body)).into_response()
        }
        Err(err) => map_room_error(err),
    }
}

/// Store one send under the index lock: dedup first, then provision the
/// mailspace and deliver via `Mailspace::send`, then append the record.
fn store_message(
    state: &AppState,
    room_dir: &Path,
    room_id: &str,
    seat: &SeatRecord,
    req: SendMessageBody,
    client_message_id: Option<String>,
) -> Result<(ChatRecord, bool), RoomError> {
    let _guard = messages_lock();
    let index = read_index(room_dir)?;
    if let Some(id) = client_message_id.as_deref() {
        if let Some(stored) = index.records.iter().find(|r| r.message_id == id) {
            let record = decode_record(stored.clone(), state.store_cipher.as_deref())?;
            return Ok((record, true));
        }
    }
    let cipher = state.store_cipher.as_deref().ok_or_else(|| {
        RoomError::Storage(
            "STORE_ENCRYPTION_KEY is not set; refusing to open room storage (U5)".to_owned(),
        )
    })?;
    let recipients = recipients_for(state, room_id, seat, req.to.as_deref())?;
    let mut labels = vec![seat.label.clone()];
    labels.extend(recipients.iter().cloned());
    let mailspace = provision_mailspace(room_dir, &labels)?;
    let subject = req.subject.unwrap_or_default();
    let content = serde_json::to_vec(&StoredContent {
        to: req.to.clone(),
        subject: subject.clone(),
        body: req.body.clone(),
    })
    .map_err(|e| RoomError::Storage(format!("failed to encode message content: {e}")))?;
    let sealed = cipher
        .encrypt(&content)
        .map_err(|e| RoomError::Storage(format!("failed to seal message content: {e}")))?;
    // The mailspace item keeps headers/metadata; its body carries the sealed
    // blob so mail.sqlite holds no plaintext content either.
    mailspace
        .send(SendRequest {
            from: seat.label.clone(),
            to: recipients.clone(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: String::new(),
            body: sealed.clone(),
            role: "inbox".into(),
            kind: None,
            reply_to: None,
            depends_on: Vec::new(),
        })
        .map_err(|e| RoomError::Storage(format!("mailspace send failed: {e}")))?;
    let cursor = index.records.last().map(|r| r.cursor + 1).unwrap_or(1);
    let record = ChatRecord {
        cursor,
        message_id: client_message_id.unwrap_or_else(|| format!("m{cursor}")),
        from: seat.label.clone(),
        fingerprint: seat.fingerprint.chars().take(16).collect(),
        to: req.to,
        subject,
        body: req.body,
        created_at: rfc3339(Utc::now()),
    };
    let mut index = index;
    index.records.push(StoredRecord {
        cursor: record.cursor,
        message_id: record.message_id.clone(),
        from: record.from.clone(),
        fingerprint: record.fingerprint.clone(),
        created_at: record.created_at.clone(),
        to: None,
        subject: String::new(),
        body: String::new(),
        enc: Some(sealed),
    });
    write_index(room_dir, &index)?;
    Ok((record, false))
}

/// Direct sends resolve to the named seat label (validated); broadcast goes
/// to every other bound seat, or self when the room has no other seats yet.
fn recipients_for(
    state: &AppState,
    room_id: &str,
    seat: &SeatRecord,
    to: Option<&str>,
) -> Result<Vec<String>, RoomError> {
    match to {
        Some(label) => {
            if !state.seats.has_label(room_id, label) {
                return Err(RoomError::BadRequest(format!(
                    "unknown recipient label: {label}"
                )));
            }
            Ok(vec![label.to_owned()])
        }
        None => {
            let others = state.seats.labels_except(room_id, seat.seat);
            if others.is_empty() {
                Ok(vec![seat.label.clone()])
            } else {
                Ok(others)
            }
        }
    }
}

/// Open the room's vivi mailspace, creating config + store on first send, and
/// provision one identity per seat label. `Mailspace::init` is idempotent
/// once the config exists; `add_identity` errors on duplicates, hence the
/// resolve-then-add guard.
fn provision_mailspace(room_dir: &Path, labels: &[String]) -> Result<Mailspace, RoomError> {
    let mut mailspace = Mailspace::init(Some(room_dir))
        .map_err(|e| RoomError::Storage(format!("failed to open room mailspace: {e}")))?;
    for label in labels {
        if mailspace.resolve_identity(label).is_err() {
            mailspace.add_identity(label).map_err(|e| {
                RoomError::Storage(format!("failed to provision seat identity: {e}"))
            })?;
        }
    }
    // Sqlite stores, identities, and sidecars stay owner-only (VC-5).
    crate::crypto::harden_tree(room_dir);
    Ok(mailspace)
}

/// `GET /v1/rooms/:id/messages?since=&wait=` — poll / long-poll (bearer).
pub async fn poll_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    query: Result<Query<PollQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Query(params) = match query {
        Ok(query) => query,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "bad query parameters",
            );
        }
    };
    let (room_id, dir) = match resolve_room(&state, &id) {
        Ok(room) => room,
        Err(response) => return response,
    };
    if authenticate(&state, &room_id, &headers).is_err() {
        return unauthorized("missing, invalid, or expired bearer token");
    }
    let since = params.since.unwrap_or(0);
    let wait = params.wait.unwrap_or(0).min(MAX_WAIT_SECS);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait);
    let cipher = state.store_cipher.clone();
    loop {
        let snapshot = tokio::task::spawn_blocking({
            let dir = dir.clone();
            let cipher = cipher.clone();
            move || read_after(&dir, since, cipher.as_deref())
        })
        .await
        .unwrap_or_else(|e| Err(RoomError::Storage(format!("poll task failed: {e}"))));
        match snapshot {
            Ok(messages) if !messages.is_empty() => {
                let next_cursor = messages.last().map(|r| r.cursor).unwrap_or(since);
                return (
                    StatusCode::OK,
                    Json(json!({ "messages": messages, "next_cursor": next_cursor })),
                )
                    .into_response();
            }
            Ok(_) if tokio::time::Instant::now() >= deadline => {
                return (
                    StatusCode::OK,
                    Json(json!({ "messages": [], "next_cursor": since })),
                )
                    .into_response();
            }
            Ok(_) => tokio::time::sleep(POLL_TICK).await,
            Err(err) => return map_room_error(err),
        }
    }
}

fn read_after(
    room_dir: &Path,
    since: u64,
    cipher: Option<&StoreCipher>,
) -> Result<Vec<ChatRecord>, RoomError> {
    let index = read_index(room_dir)?;
    index
        .records
        .into_iter()
        .map(|stored| decode_record(stored, cipher))
        .filter(|result| {
            result
                .as_ref()
                .map(|record| record.cursor > since)
                .unwrap_or(true)
        })
        .collect()
}
