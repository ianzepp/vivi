//! `vivi server run` (and the `vivi-server` name): the hosted project server.
//! Formerly the vivi-server binary's main; vivi's own `main` calls [`run`]
//! before it starts its runtime, so this still begins single-threaded.
//!
//! vivi-server: public chat substrate hosting persistent vivi mailspace rooms.
//! Phase 01: env config, axum router, health + room endpoints. Wire contract
//! in docs/contract.md.

use std::net::SocketAddr;
use std::time::Duration;

use crate::server::config::ServerConfig;
use axum::Json;
use axum::routing::get;
use serde_json::json;

async fn healthz() -> Json<serde_json::Value> {
    Json(json!({ "ok": true }))
}

/// Print `message` and exit with `code`. Boot failures end here, not in a
/// panic.
fn die(code: i32, message: &str) -> ! {
    eprintln!("vivi-server: {message}");
    std::process::exit(code)
}

// The runtime is built by hand, after the configuration is read: reading
// removes the secrets from the environment, which is only safe while this
// is the one thread.
pub fn run(args: Vec<String>) {
    // Before anything is read: no same-user process may read this one's
    // memory or environment.
    if let Err(err) = crate::server::hardening::disable_dumping() {
        die(1, &format!("cannot make the process non-dumpable: {err}"));
    }
    match crate::server::cli::parse(args) {
        Ok(crate::server::cli::Invocation::Serve) => {}
        Ok(crate::server::cli::Invocation::Help) => {
            print!("{}", crate::server::cli::help_text());
            return;
        }
        Ok(crate::server::cli::Invocation::Version) => {
            println!("{}", crate::server::cli::version_line());
            return;
        }
        Err(err) => {
            eprintln!("vivi-server: {err}\nTry 'vivi server run --help'.");
            std::process::exit(2);
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "vivi::server=info,tower_http=info".into()),
        )
        .init();

    let config = match ServerConfig::from_env() {
        Ok(c) => c,
        Err(err) => die(2, &err),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|err| die(1, &format!("cannot start the async runtime: {err}")));
    runtime.block_on(serve(config));
}

async fn serve(config: ServerConfig) {
    // Bind first. A second instance on the same address stops here, before
    // it touches the data directory. Connections that arrive while the state
    // below loads wait in the listener's backlog and are served once it is
    // ready.
    let addr = SocketAddr::from((config.bind, config.port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) => die(1, &format!("cannot bind {addr}: {err}")),
    };

    check_store_and_warn(&config);

    let mut state = crate::server::app_state(
        config.data_dir.clone(),
        config.store_encryption_key.clone(),
        config.moderation_token.clone(),
    )
    .unwrap_or_else(|err| die(2, &err));
    if let Some(identity) = &config.hosted_default_identity {
        state.hosted_default_identity = identity.as_str().into();
    }
    state.limiter = crate::server::safety::RateLimiter::new(config.send_rate_per_minute);
    state.host_operator_token = config.host_operator_token.as_deref().map(Into::into);
    state.host = crate::server::mcp::host::HostConfig::from_env();
    // Serve the projects the host registry lists, now and whenever it
    // changes. The first pass finishes before the server accepts a request.
    let _hosted = crate::server::hosted::start(state.projects.clone(), state.identities.clone());
    // Keep each hosted project's transport index current with its mailspace.
    let _indexer = crate::server::hosted::start_indexer(state.projects.clone());
    let app = crate::server::router(state).route("/healthz", get(healthz));

    match config.send_rate_per_minute {
        Some(limit) => tracing::info!("write rate limit: {limit}/min per identity"),
        None => tracing::info!("write rate limit: unlimited"),
    }

    tracing::info!(
        "vivi-server listening on {addr} (data dir {})",
        config.data_dir.display()
    );

    if let Err(err) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(Duration::from_secs(10)))
        .await
    {
        die(1, &format!("server error: {err}"));
    }
}

/// The boot-time key verifier (U5) and the warnings for unset optional
/// secrets: an invalid key, a sealed store met with a missing or wrong key,
/// or an unreadable verifier exits before any request is served.
fn check_store_and_warn(config: &ServerConfig) {
    let cipher = match config.store_encryption_key.as_deref() {
        Some(raw) => match crate::server::crypto::StoreCipher::from_base64(raw) {
            Ok(cipher) => Some(std::sync::Arc::new(cipher)),
            Err(err) => die(2, &format!("{err}; exiting")),
        },
        None => None,
    };
    if let Err(err) = crate::server::crypto::verify_store(&config.data_dir, cipher.as_deref()) {
        die(1, &format!("{err}; exiting"));
    }
    if cipher.is_none() {
        tracing::warn!("STORE_ENCRYPTION_KEY not set; room storage will refuse to open (U5)");
    }
    if config.moderation_token.is_none() {
        tracing::warn!("MODERATION_TOKEN not set; /v1/_health disabled (U3)");
    }

    if config.host_operator_token.is_none() {
        tracing::warn!("HOST_OPERATOR_TOKEN not set; /v1/mcp has no built-in host principal");
    }
}

async fn shutdown_signal(drain: Duration) {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!("cannot listen for ctrl-c: {err}");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => {
                tracing::error!("cannot listen for SIGTERM: {err}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received; draining {drain:?}");
}
