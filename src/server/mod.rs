//! vivi-server library: project mailspaces, lightweight chat rooms
//! (item 17), identity, MCP, and the v1 HTTP router. Wire contract in
//! docs/contract.md.

pub mod admin;
pub mod admissions;
pub mod auth;
pub mod crypto;
pub mod identities;
pub mod ids;
pub mod keys;
pub mod mcp;
pub mod project_wake;
pub mod projects;
pub mod projects_api;
pub mod rooms;
pub mod safety;

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post};
use tower_http::compression::CompressionLayer;

use auth::ChallengeBook;
use crypto::StoreCipher;
use identities::IdentityRegistry;
use keys::ApiKeyRegistry;
use projects::ProjectStore;
use safety::RateLimiter;

/// Shared router state: project store (durable and chat), identities,
/// challenges, rate limiter, the at-rest cipher (legacy rooms / VC-5),
/// and the moderation token for `/v1/_health`.
#[derive(Clone)]
pub struct AppState {
    pub projects: ProjectStore,
    pub identities: IdentityRegistry,
    pub keys: ApiKeyRegistry,
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
        projects: ProjectStore::new(&data_dir),
        identities: IdentityRegistry::new(&data_dir),
        keys: ApiKeyRegistry::new(&data_dir),
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
            "/v1/projects/{id}/auth/token",
            post(projects_api::token_handler),
        )
        .route(
            "/v1/projects/{id}/directory",
            get(projects_api::directory_handler),
        )
        .route(
            "/v1/projects/{id}/keys",
            post(keys::mint_handler).get(keys::list_handler),
        )
        .route(
            "/v1/projects/{id}/keys/{key_id}",
            delete(keys::revoke_handler),
        )
        .route("/v1/projects/{id}/mcp", post(mcp::mcp_handler))
        .route("/v1/rooms/{id}/mcp", post(mcp::mcp_handler))
        // List and lookup responses are large JSON; gzip them when the
        // client asks for it.
        .route(
            "/v1/projects/{id}/items",
            get(projects_api::items_handler).layer(CompressionLayer::new()),
        )
        .route(
            "/v1/projects/{id}/items/lookup",
            post(projects_api::lookup_handler).layer(CompressionLayer::new()),
        )
        .route(
            "/v1/projects/{id}/items/{message_id}",
            get(projects_api::letter_handler)
                .put(projects_api::append_handler)
                .layer(DefaultBodyLimit::max(projects_api::APPEND_BODY_LIMIT)),
        )
        .route("/v1/rooms", post(rooms::create_room_handler))
        .route("/v1/rooms/{id}", get(rooms::room_meta_handler))
        .route("/v1/rooms/{id}/join", post(rooms::join_room_handler))
        .route("/v1/rooms/{id}/close", post(rooms::close_room_handler))
        .route(
            "/v1/rooms/{id}/messages",
            post(rooms::send_handler).get(rooms::poll_handler),
        );
    // Disabled (unregistered) when no moderation token is configured.
    if state.moderation_token.is_some() {
        router = router.route("/v1/_health", get(admin::health_handler));
    }
    router.with_state(state)
}
