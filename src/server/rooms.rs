//! Room store and v1 room endpoints: one persistent vivi mailspace per room
//! under `DATA_DIR/rooms/<room_id>/mailspace/`, opened through
//! `vivi::storage::Storage::open_mailspace`. Seats/auth land in VC-3.

use std::path::{Path, PathBuf};

use axum::extract::rejection::JsonRejection;
use axum::extract::{State, Path as AxumPath};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use vivi::storage::Storage;

use crate::ids::{mint_room_id, normalize_room_id};

pub const MIN_CAPACITY: u32 = 2;
pub const MAX_CAPACITY: u32 = 8;
pub const DEFAULT_CAPACITY: u32 = 4;

#[derive(Debug)]
pub enum RoomError {
    /// Capacity outside 2..8.
    Capacity(u32),
    /// Id not canonical Crockford-base32.
    InvalidRoomId,
    /// Unknown room id.
    RoomNotFound,
    /// Storage or IO failure.
    Storage(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomMeta {
    pub room_id: String,
    pub capacity: u32,
}

/// Per-room storage root under `DATA_DIR/rooms`. Opening a room (re)creates
/// its mailspace; room metadata persists in `room.json` beside it.
#[derive(Debug, Clone)]
pub struct RoomStore {
    rooms_root: PathBuf,
}

impl RoomStore {
    pub fn new(data_dir: &Path) -> Self {
        Self { rooms_root: data_dir.join("rooms") }
    }

    /// Mint a collision-retried room id and create its mailspace.
    pub fn create_room(&self, capacity: u32) -> Result<RoomMeta, RoomError> {
        if !(MIN_CAPACITY..=MAX_CAPACITY).contains(&capacity) {
            return Err(RoomError::Capacity(capacity));
        }
        for _ in 0..8 {
            let room_id = mint_room_id()
                .map_err(|_| RoomError::Storage("entropy source unavailable".into()))?;
            let room_dir = self.rooms_root.join(&room_id);
            if room_dir.exists() {
                continue;
            }
            let mailspace = room_dir.join("mailspace");
            Storage::open_mailspace(&mailspace).map_err(|e| RoomError::Storage(e.to_string()))?;
            let meta = RoomMeta { room_id, capacity };
            let bytes = serde_json::to_vec(&meta)
                .map_err(|e| RoomError::Storage(format!("failed to encode room.json: {e}")))?;
            std::fs::write(room_dir.join("room.json"), &bytes)
                .map_err(|e| RoomError::Storage(format!("failed to write room.json: {e}")))?;
            return Ok(meta);
        }
        Err(RoomError::Storage("room id mint retries exhausted".into()))
    }

    /// Canonical room directory for a raw id.
    pub fn room_dir(&self, raw_id: &str) -> Result<PathBuf, RoomError> {
        let id = normalize_room_id(raw_id).map_err(|_| RoomError::InvalidRoomId)?;
        Ok(self.rooms_root.join(id))
    }

    /// Room metadata, or `RoomNotFound` when the room was never created.
    pub fn room_meta(&self, raw_id: &str) -> Result<RoomMeta, RoomError> {
        let dir = self.room_dir(raw_id)?;
        let bytes = std::fs::read(dir.join("room.json")).map_err(|_| RoomError::RoomNotFound)?;
        serde_json::from_slice(&bytes).map_err(|e| RoomError::Storage(e.to_string()))
    }

    /// Reopen a room's mailspace storage.
    pub fn open_mailspace(&self, raw_id: &str) -> Result<Storage, RoomError> {
        let meta = self.room_meta(raw_id)?;
        let mailspace = self.rooms_root.join(&meta.room_id).join("mailspace");
        Storage::open_mailspace(&mailspace).map_err(|e| RoomError::Storage(e.to_string()))
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct CreateRoomRequest {
    pub seats: Option<u32>,
}

fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "error": { "code": code, "message": message } }))).into_response()
}

fn map_room_error(err: RoomError) -> Response {
    match err {
        RoomError::Capacity(n) => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            &format!("seats must be {MIN_CAPACITY}..{MAX_CAPACITY}, got {n}"),
        ),
        RoomError::InvalidRoomId => {
            error_response(StatusCode::BAD_REQUEST, "invalid_room_id", "id not canonical Crockford-base32")
        }
        RoomError::RoomNotFound => error_response(StatusCode::NOT_FOUND, "room_not_found", "unknown room id"),
        RoomError::Storage(msg) => {
            tracing::error!("room storage failure: {msg}");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal", "storage failure")
        }
    }
}

/// `POST /v1/rooms` — create a room. VC-2 returns the shareable id; the
/// seat/token fields of the contract response land with VC-3.
pub async fn create_room_handler(
    State(store): State<RoomStore>,
    payload: Result<Json<CreateRoomRequest>, JsonRejection>,
) -> Response {
    let Json(req) = match payload {
        Ok(body) => body,
        Err(_) => return error_response(StatusCode::BAD_REQUEST, "invalid_request", "malformed JSON body"),
    };
    let capacity = req.seats.unwrap_or(DEFAULT_CAPACITY);
    let result = tokio::task::spawn_blocking(move || store.create_room(capacity))
        .await
        .unwrap_or_else(|e| Err(RoomError::Storage(format!("create task failed: {e}"))));
    match result {
        Ok(meta) => (StatusCode::CREATED, Json(json!({ "room_id": meta.room_id }))).into_response(),
        Err(err) => map_room_error(err),
    }
}

/// `GET /v1/rooms/:id` — room metadata.
pub async fn room_meta_handler(State(store): State<RoomStore>, AxumPath(id): AxumPath<String>) -> Response {
    match tokio::task::spawn_blocking(move || store.room_meta(&id))
        .await
        .unwrap_or_else(|e| Err(RoomError::Storage(format!("metadata task failed: {e}"))))
    {
        Ok(meta) => Json(json!({ "room_id": meta.room_id, "capacity": meta.capacity })).into_response(),
        Err(err) => map_room_error(err),
    }
}
