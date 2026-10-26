//! Bounded runtime teardown for the server, and the serving loop's
//! signal-relative drain window. The runtime that drives the serving
//! future is consumed explicitly when the future completes, so unfinished
//! blocking work cannot extend process lifetime. The drain window starts
//! at the first shutdown signal — never at serve time — and is recorded
//! once; it is never restarted.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::VivariumError;

/// How [`serve_with_shutdown`] finished serving. Both outcomes are
/// expected; neither fails the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DrainOutcome {
    /// Serving finished and every connection completed inside the window.
    Drained,
    /// The drain deadline passed while connections were still open and the
    /// server stopped anyway — expected under a long-poll client, not an
    /// error.
    Expired,
}

/// Build the multi-thread runtime, drive `future` to completion on it,
/// and return its output. The runtime is then consumed with
/// [`shutdown_timeout`](tokio::runtime::Runtime::shutdown_timeout) at
/// zero: unfinished blocking work must not extend process lifetime.
/// Runtime-construction failures surface as [`VivariumError`].
pub(super) fn run<F>(future: F) -> Result<F::Output, VivariumError>
where
    F: Future,
{
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let output = runtime.block_on(future);
    runtime.shutdown_timeout(Duration::ZERO);
    Ok(output)
}

/// Serve `app` on `listener` until `shutdown` fires, then drain for at
/// most `drain` and stop. There is no server-lifetime timeout while
/// running. On the first signal the one monotonic deadline is recorded,
/// Axum's graceful shutdown is triggered, and the background wrappers are
/// aborted so they cannot schedule another pass; the same serving future
/// keeps polling until it completes or the deadline passes. Wrappers are
/// also aborted on any other serving exit, without an indefinite join.
/// Signal monitoring is first-polled before the serving future, so a
/// signal arriving before the first request is answered is not missed.
pub(super) async fn serve_with_shutdown(
    listener: TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()>,
    background: Vec<JoinHandle<()>>,
    drain: Duration,
) -> Result<DrainOutcome, VivariumError> {
    let gate = Arc::new(Notify::new());
    let graceful = {
        let gate = Arc::clone(&gate);
        async move { gate.notified().await }
    };
    // `with_graceful_shutdown` is an `IntoFuture`, not a `Future`; pin
    // the future it becomes so the same serving future can keep polling
    // through the drain.
    let serving = axum::serve(listener, app)
        .with_graceful_shutdown(graceful)
        .into_future();
    tokio::pin!(serving);
    let signals = shutdown;
    tokio::pin!(signals);

    // Biased, signals first: every poll touches the signal future before
    // the serving future, so monitoring is installed before the server can
    // answer a readiness request.
    tokio::select! {
        biased;
        _ = &mut signals => {
            tracing::info!("shutdown signal received; draining for {drain:?}");
            let deadline = tokio::time::Instant::now() + drain;
            gate.notify_one();
            abort_background(&background);
            drain_to_deadline(serving.as_mut(), &background, deadline).await
        }
        result = &mut serving => {
            abort_background(&background);
            finish_serving(result)
        }
    }
}

/// Keep polling the already-running serving future until it completes or
/// the recorded deadline passes. The deadline is never restarted here.
async fn drain_to_deadline<S>(
    serving: Pin<&mut S>,
    background: &[JoinHandle<()>],
    deadline: tokio::time::Instant,
) -> Result<DrainOutcome, VivariumError>
where
    S: Future<Output = io::Result<()>>,
{
    tokio::select! {
        biased;
        result = serving => {
            abort_background(background);
            finish_serving(result)
        }
        _ = tokio::time::sleep_until(deadline) => {
            tracing::info!("drain deadline expired; closing remaining connections");
            Ok(DrainOutcome::Expired)
        }
    }
}

/// Map a serving-future exit: transport errors stay errors, a clean exit
/// drained every connection.
fn finish_serving(result: io::Result<()>) -> Result<DrainOutcome, VivariumError> {
    result?;
    tracing::info!("serving stopped; connections drained");
    Ok(DrainOutcome::Drained)
}

/// Stop the background wrappers so they cannot schedule another pass.
/// Aborted, never joined: an in-flight pass must not extend the drain.
fn abort_background(background: &[JoinHandle<()>]) {
    for handle in background {
        handle.abort();
    }
}

/// Cross-platform shutdown signals: Unix SIGTERM and Ctrl-C everywhere.
/// A registration failure is reported and that signal is then ignored
/// (the future never completes); the other signal still drains.
pub(super) async fn shutdown_signals() {
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
}

#[cfg(test)]
#[path = "shutdown_test.rs"]
mod tests;
