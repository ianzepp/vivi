//! vivi-server: public chat substrate hosting persistent vivi mailspace rooms.
//! Phase 01: env config, axum router, health + room endpoints. Wire contract
//! in docs/contract.md.

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use axum::Json;
use axum::routing::get;
use serde_json::json;

/// Environment-derived configuration. The process refuses to boot without the
/// encryption key once room storage lands (fail-closed posture, U5 enforces the
/// verifier); for now the values are read and surfaced to the health report.
#[derive(Debug, Clone)]
struct ServerConfig {
    /// Listen address (`BIND`, default `0.0.0.0`). A host that must not be
    /// reachable on every interface (a tailnet-only server) sets its own.
    bind: IpAddr,
    port: u16,
    data_dir: PathBuf,
    store_encryption_key: Option<String>,
    moderation_token: Option<String>,
    /// `HOSTED_DEFAULT_IDENTITY`: the vivi identity an API key without its
    /// own identity acts as on a hosted project.
    hosted_default_identity: Option<String>,
    /// `HOST_OPERATOR_TOKEN`: the Bearer for `POST /v1/host/mcp`.
    host_operator_token: Option<String>,
}

impl ServerConfig {
    fn from_env() -> Result<Self, String> {
        let port = match std::env::var("PORT") {
            Ok(v) => v.parse::<u16>().map_err(|_| format!("invalid PORT: {v}"))?,
            Err(_) => 8080,
        };
        let bind = match std::env::var("BIND") {
            Ok(v) => v
                .parse::<IpAddr>()
                .map_err(|_| format!("invalid BIND: {v}"))?,
            Err(_) => IpAddr::from([0, 0, 0, 0]),
        };
        let data_dir = PathBuf::from(std::env::var("DATA_DIR").unwrap_or_else(|_| "data".into()));
        Ok(Self {
            bind,
            port,
            data_dir,
            store_encryption_key: std::env::var("STORE_ENCRYPTION_KEY").ok(),
            moderation_token: std::env::var("MODERATION_TOKEN").ok(),
            host_operator_token: std::env::var("HOST_OPERATOR_TOKEN")
                .ok()
                .filter(|t| !t.trim().is_empty()),
            hosted_default_identity: match std::env::var("HOSTED_DEFAULT_IDENTITY") {
                Ok(v) if !v.trim().is_empty() => {
                    let v = v.trim().to_owned();
                    if v.len() > 64
                        || !v
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                    {
                        return Err(format!("invalid HOSTED_DEFAULT_IDENTITY: {v}"));
                    }
                    Some(v)
                }
                _ => None,
            },
        })
    }
}

async fn healthz() -> Json<serde_json::Value> {
    Json(json!({ "ok": true }))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "vivi_server=info,tower_http=info".into()),
        )
        .init();

    let config = match ServerConfig::from_env() {
        Ok(c) => c,
        Err(err) => {
            eprintln!("vivi-server: {err}");
            std::process::exit(2);
        }
    };
    // Boot-time key verifier (U5): an invalid key, a sealed store met with a
    // missing or wrong key, or an unreadable verifier exits the process
    // before any listener binds.
    let cipher = match config.store_encryption_key.as_deref() {
        Some(raw) => match vivi_server::crypto::StoreCipher::from_base64(raw) {
            Ok(cipher) => Some(std::sync::Arc::new(cipher)),
            Err(err) => {
                eprintln!("vivi-server: {err}; exiting");
                std::process::exit(2);
            }
        },
        None => None,
    };
    if let Err(err) = vivi_server::crypto::verify_store(&config.data_dir, cipher.as_deref()) {
        eprintln!("vivi-server: {err}; exiting");
        std::process::exit(1);
    }
    if cipher.is_none() {
        tracing::warn!("STORE_ENCRYPTION_KEY not set; room storage will refuse to open (U5)");
    }
    if config.moderation_token.is_none() {
        tracing::warn!("MODERATION_TOKEN not set; /v1/_health disabled (U3)");
    }

    if config.host_operator_token.is_none() {
        tracing::warn!("HOST_OPERATOR_TOKEN not set; /v1/host/mcp disabled");
    }

    let mut state = vivi_server::app_state(
        config.data_dir.clone(),
        config.store_encryption_key.clone(),
        config.moderation_token.clone(),
    );
    if let Some(identity) = &config.hosted_default_identity {
        state.hosted_default_identity = identity.as_str().into();
    }
    state.host_operator_token = config.host_operator_token.as_deref().map(Into::into);
    state.host = vivi_server::mcp::host::HostConfig::from_env();
    // Serve the projects the host registry lists, now and whenever it
    // changes. The first pass finishes before the listener binds.
    let _hosted = vivi_server::hosted::start(state.projects.clone(), state.identities.clone());
    // Keep each hosted project's transport index current with its mailspace.
    let _indexer = vivi_server::hosted::start_indexer(state.projects.clone());
    let app = vivi_server::router(state).route("/healthz", get(healthz));

    let addr = std::net::SocketAddr::from((config.bind, config.port));
    tracing::info!(
        "vivi-server listening on {addr} (data dir {})",
        config.data_dir.display()
    );

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|err| panic!("vivi-server: cannot bind {addr}: {err}"));

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(Duration::from_secs(10)))
        .await
        .expect("vivi-server: server error");
}

async fn shutdown_signal(drain: Duration) {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("vivi-server: ctrl-c handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("vivi-server: SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received; draining {drain:?}");
}
