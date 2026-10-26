//! vivi-server library: project mailspaces, lightweight chat rooms
//! (item 17), identity, MCP, and the v1 HTTP router. Wire contract in
//! docs/contract.md.

pub mod admin;
pub mod admissions;
pub mod audit;
pub mod auth;
pub mod cli;
pub mod config;
pub mod crypto;
pub mod entry;
pub mod grants;
pub mod hardening;
pub mod hosted;
pub mod identities;
pub mod ids;
pub mod keys;
pub mod mcp;
pub mod project_wake;
pub mod projects;
pub mod projects_api;
pub mod pump_health;
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
/// the moderation token for `/v1/_health`, the one server key store and
/// audit log for `/v1/mcp`, and the moderation token.
#[derive(Clone)]
pub struct AppState {
    pub projects: ProjectStore,
    pub identities: IdentityRegistry,
    pub keys: ApiKeyRegistry,
    /// The one server key store (`DATA_DIR/keys.json`) behind `/v1/mcp`
    /// and the self-service key mint.
    pub server_keys: grants::ServerKeyStore,
    /// The append-only audit log (`DATA_DIR/audit.jsonl`) for write tool
    /// calls and key/grant changes on `/v1/mcp`.
    pub audit: audit::AuditLog,
    pub challenges: ChallengeBook,
    pub limiter: RateLimiter,
    pub store_cipher: Option<Arc<StoreCipher>>,
    pub moderation_token: Option<Arc<str>>,
    /// The vivi identity an API key without its own `identity` acts as on a
    /// hosted project (`HOSTED_DEFAULT_IDENTITY`, default `operator`).
    pub hosted_default_identity: Arc<str>,
    /// `HOST_OPERATOR_TOKEN`: a built-in host principal on `POST /v1/mcp`
    /// (host tools only). Unset means host tools need a minted host key.
    pub host_operator_token: Option<Arc<str>>,
    /// Where `create_project` puts repositories and projects.
    pub host: mcp::host::HostConfig,
}

/// The identity an unmapped API key acts as on a hosted project.
pub const DEFAULT_HOSTED_IDENTITY: &str = "operator";

/// v1 routes over `DATA_DIR/rooms/`. `store_key` is the raw
/// `STORE_ENCRYPTION_KEY` value; `moderation_token` gates `/v1/_health`
/// (route unregistered when absent).
///
/// # Errors
/// An undecodable `store_key` or server key store, as [`app_state`].
pub fn api_router(
    data_dir: PathBuf,
    store_key: Option<String>,
    moderation_token: Option<String>,
) -> Result<Router, String> {
    Ok(router(app_state(data_dir, store_key, moderation_token)?))
}

/// Router state over `data_dir`. Split from [`router`] so the process can
/// start the hosted-project watcher (`hosted::start`) over the same store
/// the routes serve; `api_router` never reads the host registry.
///
/// # Errors
/// An undecodable `store_key` or an undecodable server key store
/// (`DATA_DIR/keys.json`).
pub fn app_state(
    data_dir: PathBuf,
    store_key: Option<String>,
    moderation_token: Option<String>,
) -> Result<AppState, String> {
    let store_cipher = store_key
        .as_deref()
        .map(StoreCipher::from_base64)
        .transpose()
        .map_err(|err| format!("invalid STORE_ENCRYPTION_KEY: {err:?}"))?
        .map(Arc::new);
    let server_keys = grants::ServerKeyStore::new(&data_dir)
        .map_err(|err| format!("cannot load the server key store: {err:?}"))?;
    // Legacy per-project key files are renamed aside once, at the first
    // boot, and are not read. A fresh self-service mint re-creates
    // `api_keys.json` with just the mirrored key: rooms auth and the letter
    // wire read that file, and a later boot leaves it. That mirror is the
    // only writer of new per-project keys. Keys minted before the upgrade
    // stay dead.
    for (project, dead) in grants::retire_legacy_project_keys(&data_dir) {
        tracing::info!(
            "retired {dead} legacy api key(s) of project {project} \
             (api_keys.json renamed to api_keys.json.retired)"
        );
    }
    Ok(AppState {
        projects: ProjectStore::new(&data_dir),
        identities: IdentityRegistry::new(&data_dir),
        keys: ApiKeyRegistry::new(&data_dir),
        server_keys,
        audit: audit::AuditLog::new(&data_dir),
        challenges: ChallengeBook::default(),
        limiter: RateLimiter::default(),
        store_cipher,
        moderation_token: moderation_token
            .filter(|t| !t.trim().is_empty())
            .map(Arc::from),
        hosted_default_identity: Arc::from(DEFAULT_HOSTED_IDENTITY),
        host_operator_token: None,
        host: mcp::host::HostConfig::default(),
    })
}

/// The v1 routes over `state`.
pub fn router(state: AppState) -> Router {
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
        .route(
            "/v1/projects/{id}/pump/health",
            get(pump_health::pump_health_handler),
        )
        // Chat rooms only. Project and host MCP are `POST /v1/mcp`.
        .route("/v1/rooms/{id}/mcp", post(mcp::rooms_mcp_handler))
        // One server endpoint: a server key (or the host operator token)
        // reaches every granted project. `project` is required on project tools.
        .route("/v1/mcp", post(mcp::server::server_mcp_handler))
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
