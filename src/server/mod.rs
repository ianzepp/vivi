//! vivi-server library: room ids, the per-room mailspace store, seats/auth,
//! safety rails, and the v1 HTTP router. Wire contract in docs/contract.md
//! (frozen at U1; auth hardening summary authoritative for VC-3).

pub mod admin;
pub mod admissions;
pub mod auth;
pub mod crypto;
pub mod identities;
pub mod ids;
pub mod messages;
pub mod project_wake;
pub mod projects;
pub mod projects_api;
pub mod rooms;
pub mod safety;
pub mod seats;

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};

use auth::ChallengeBook;
use crypto::StoreCipher;
use identities::IdentityRegistry;
use projects::ProjectStore;
use rooms::RoomStore;
use safety::RateLimiter;
use seats::SeatRegistry;

/// Shared router state: room store, seat registry, challenges, rate limiter,
/// the at-rest cipher (VC-5), and the moderation token for `/v1/_health`.
#[derive(Clone)]
pub struct AppState {
    pub rooms: RoomStore,
    pub seats: SeatRegistry,
    pub projects: ProjectStore,
    pub identities: IdentityRegistry,
    pub challenges: ChallengeBook,
    pub limiter: RateLimiter,
    pub store_cipher: Option<Arc<StoreCipher>>,
    pub moderation_token: Option<Arc<str>>,
}

/// v1 routes over `DATA_DIR/rooms/`. `store_key` is the raw
/// `STORE_ENCRYPTION_KEY` value; `moderation_token` gates `/v1/_health`
/// (route unregistered when absent).
///
/// # Panics
/// Panics on an undecodable `store_key`; `main` validates and exits before
/// calling this.
pub fn api_router(
    data_dir: PathBuf,
    store_key: Option<String>,
    moderation_token: Option<String>,
) -> Router {
    let store_cipher = store_key
        .as_deref()
        .map(StoreCipher::from_base64)
        .transpose()
        .expect("valid STORE_ENCRYPTION_KEY")
        .map(Arc::new);
    let state = AppState {
        rooms: RoomStore::new(&data_dir),
        seats: SeatRegistry::new(&data_dir),
        projects: ProjectStore::new(&data_dir),
        identities: IdentityRegistry::new(&data_dir),
        challenges: ChallengeBook::default(),
        limiter: RateLimiter::default(),
        store_cipher,
        moderation_token: moderation_token
            .filter(|t| !t.trim().is_empty())
            .map(Arc::from),
    };
    let mut router = Router::new()
        .route("/v1/projects", post(projects_api::create_project_handler))
        .route(
            "/v1/projects/{id}/identities/challenge",
            post(projects_api::challenge_handler),
        )
        .route(
            "/v1/projects/{id}/identities",
            post(projects_api::register_handler),
        )
        .route(
            "/v1/projects/{id}/directory",
            get(projects_api::directory_handler),
        )
        .route("/v1/projects/{id}/items", get(projects_api::items_handler))
        .route("/v1/rooms", post(rooms::create_room_handler))
        .route("/v1/rooms/{id}", get(rooms::room_meta_handler))
        .route("/v1/rooms/{id}/challenge", post(auth::challenge_handler))
        .route("/v1/rooms/{id}/join", post(auth::join_handler))
        .route("/v1/rooms/{id}/auth/token", post(auth::token_handler))
        .route(
            "/v1/rooms/{id}/messages",
            post(messages::send_handler).get(messages::poll_handler),
        );
    // Disabled (unregistered) when no moderation token is configured.
    if state.moderation_token.is_some() {
        router = router.route("/v1/_health", get(admin::health_handler));
    }
    router.with_state(state)
}
