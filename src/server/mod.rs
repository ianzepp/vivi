//! vivi-server library: room ids, the per-room mailspace store, seats/auth,
//! safety rails, and the v1 HTTP router. Wire contract in docs/contract.md
//! (frozen at U1; auth hardening summary authoritative for VC-3).

pub mod auth;
pub mod ids;
pub mod rooms;
pub mod safety;
pub mod seats;

use std::path::PathBuf;

use axum::Router;
use axum::routing::{get, post};

use auth::ChallengeBook;
use rooms::RoomStore;
use safety::RateLimiter;
use seats::SeatRegistry;

/// Shared router state: room store, seat registry, challenges, rate limiter.
#[derive(Clone)]
pub struct AppState {
    pub rooms: RoomStore,
    pub seats: SeatRegistry,
    pub challenges: ChallengeBook,
    pub limiter: RateLimiter,
}

/// v1 routes over `DATA_DIR/rooms/`.
pub fn api_router(data_dir: PathBuf) -> Router {
    let state = AppState {
        rooms: RoomStore::new(&data_dir),
        seats: SeatRegistry::new(&data_dir),
        challenges: ChallengeBook::default(),
        limiter: RateLimiter::default(),
    };
    Router::new()
        .route("/v1/rooms", post(rooms::create_room_handler))
        .route("/v1/rooms/{id}", get(rooms::room_meta_handler))
        .route("/v1/rooms/{id}/challenge", post(auth::challenge_handler))
        .route("/v1/rooms/{id}/join", post(auth::join_handler))
        .route("/v1/rooms/{id}/auth/token", post(auth::token_handler))
        .with_state(state)
}
