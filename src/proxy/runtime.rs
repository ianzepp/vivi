//! Foreground supervisor: lock, bind, children, HTTP, signals.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;

use super::access::AccessLog;
use super::child::Children;
use super::config::{self, ProxyConfig};
use super::health::HealthTable;
use super::paths::{self, ensure_private_dir};
use super::server::{self, AppState};
use crate::VivariumError;

const POLL_INTERVAL: Duration = Duration::from_secs(5);
const READY: &str = "VIVI_PROXY_READY";

/// Run until SIGTERM / Ctrl-C.
///
/// # Errors
///
/// Returns a configuration, lock, or bind error.
pub async fn run(explicit: Option<&Path>) -> Result<(), VivariumError> {
    let config = config::load(explicit)?;
    let _lock = take_lock(&config.path)?;
    ensure_private_dir(&config.log_dir)?;
    let listener = TcpListener::bind(config.listen).await.map_err(|error| {
        VivariumError::Message(format!("could not bind {}: {error}", config.listen))
    })?;
    let addr = listener.local_addr().unwrap_or(config.listen);
    announce_ready(addr)?;
    serve(config, listener).await
}

/// A test listener that can be shut down.
pub struct TestServer {
    pub addr: SocketAddr,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl TestServer {
    /// Stop the test listener.
    pub fn stop(mut self) {
        if let Some(tx) = self.shutdown.take()
            && tx.send(()).is_err()
        {
            tracing::debug!("proxy test server already stopped");
        }
    }
}

/// Bind `127.0.0.1:0` (or the configured address) without the instance lock.
///
/// # Errors
///
/// Returns a bind or client-build error.
pub async fn spawn_test(mut config: ProxyConfig) -> Result<TestServer, VivariumError> {
    let listener = TcpListener::bind(config.listen).await?;
    let addr = listener.local_addr()?;
    config.listen = addr;
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        drop(serve_with_shutdown(config, listener, rx).await);
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    Ok(TestServer {
        addr,
        shutdown: Some(tx),
    })
}

async fn serve_with_shutdown(
    config: ProxyConfig,
    listener: TcpListener,
    shutdown: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), VivariumError> {
    let state = build_state(config.clone())?;
    let mut children = Children::default();
    children
        .reconcile(
            &config.providers,
            &config.log_dir,
            &state.client,
            &state.health,
        )
        .await;
    let app = server::router(state);
    let server = axum::serve(listener, app).with_graceful_shutdown(async {
        drop(shutdown.await);
    });
    server
        .await
        .map_err(|error| VivariumError::Other(format!("proxy server: {error}")))?;
    children.stop_all().await;
    Ok(())
}

fn build_state(config: ProxyConfig) -> Result<Arc<AppState>, VivariumError> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(|error| VivariumError::Other(format!("http client: {error}")))?;
    let log = AccessLog::open(&config.log_dir)?;
    let health = Arc::new(HealthTable::default());
    let bearer = server::load_bearer(&config)?;
    Ok(Arc::new(AppState {
        config,
        client,
        log,
        health,
        bearer,
    }))
}

async fn serve(config: ProxyConfig, listener: TcpListener) -> Result<(), VivariumError> {
    let state = build_state(config.clone())?;
    let client = state.client.clone();
    let health = Arc::clone(&state.health);
    let mut children = Children::default();
    children
        .reconcile(&config.providers, &config.log_dir, &client, &health)
        .await;
    let app = server::router(state);
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tracing::info!(listen = %config.listen, "vivi proxy listening");
    tokio::select! {
        result = axum::serve(listener, app) => {
            result.map_err(|error| VivariumError::Other(format!("proxy server: {error}")))?;
        }
        () = supervisor_loop(&mut children, &config, &client, &health, &mut interval, &mut terminate) => {}
    }
    children.stop_all().await;
    Ok(())
}

#[cfg(unix)]
async fn supervisor_loop(
    children: &mut Children,
    config: &ProxyConfig,
    client: &reqwest::Client,
    health: &HealthTable,
    interval: &mut tokio::time::Interval,
    terminate: &mut tokio::signal::unix::Signal,
) {
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return,
            _ = terminate.recv() => return,
            _ = interval.tick() => {
                children
                    .reconcile(&config.providers, &config.log_dir, client, health)
                    .await;
            }
        }
    }
}

#[cfg(not(unix))]
async fn supervisor_loop(
    children: &mut Children,
    config: &ProxyConfig,
    client: &reqwest::Client,
    health: &HealthTable,
    interval: &mut tokio::time::Interval,
    _terminate: &mut (),
) {
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return,
            _ = interval.tick() => {
                children
                    .reconcile(&config.providers, &config.log_dir, client, health)
                    .await;
            }
        }
    }
}

fn announce_ready(addr: SocketAddr) -> Result<(), VivariumError> {
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{READY} {addr}")?;
    stdout.flush()?;
    Ok(())
}

fn take_lock(config_path: &Path) -> Result<fs::File, VivariumError> {
    let path = paths::lock_path(config_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    file.try_lock()
        .map_err(|_| VivariumError::Message("another Vivi proxy is running".into()))?;
    Ok(file)
}

/// Test helper: take the instance lock for a config path.
///
/// # Errors
///
/// Returns a lock-contention error when another process holds it.
pub fn take_lock_for_test(config_path: &Path) -> Result<fs::File, VivariumError> {
    take_lock(config_path)
}
