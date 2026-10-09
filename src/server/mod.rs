//! vivi-server library: room ids, the per-room mailspace store, and the v1
//! HTTP router. Wire contract in docs/contract.md (frozen at U1).

pub mod ids;
pub mod rooms;

use std::path::PathBuf;

use axum::routing::{get, post};
use axum::Router;

use rooms::RoomStore;

/// v1 room routes over `DATA_DIR/rooms/`.
pub fn api_router(data_dir: PathBuf) -> Router {
    Router::new()
        .route("/v1/rooms", post(rooms::create_room_handler))
        .route("/v1/rooms/{id}", get(rooms::room_meta_handler))
        .with_state(RoomStore::new(&data_dir))
}
