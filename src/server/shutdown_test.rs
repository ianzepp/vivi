//! Test entry for the bounded runtime teardown ([`super`]): the drain
//! deadline tests below, blocking-work subprocess tests in the runtime
//! child module, and stream-drain tests in their own child module for the
//! independent stream-test slice.

#[path = "shutdown_stream_test.rs"]
mod shutdown_stream_test;

mod shutdown_runtime_test;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::routing::get;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::{DrainOutcome, run, serve_with_shutdown};

/// Drop of this guard records that the wrapper future was cancelled: an
/// aborted task's future is dropped, guard and all.
struct CancelFlag(Arc<AtomicBool>);

impl Drop for CancelFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A background wrapper that would schedule another pass forever unless
/// the drain aborts it.
fn endless_wrapper(cancelled: Arc<AtomicBool>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let _guard = CancelFlag(cancelled);
        loop {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
}

/// A real loopback listener bound inside the runtime context.
fn loopback_listener() -> tokio::net::TcpListener {
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback bind");
    std_listener
        .set_nonblocking(true)
        .expect("listener set non-blocking");
    tokio::net::TcpListener::from_std(std_listener).expect("listener joins the runtime")
}

/// GET /healthz over a real socket: the raw response or an io error.
async fn health_request(addr: SocketAddr) -> std::io::Result<String> {
    let mut stream = tokio::net::TcpStream::connect(addr).await?;
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    Ok(String::from_utf8_lossy(&raw).into_owned())
}

/// The first successful /healthz answer, retrying while the listener is
/// still coming up.
async fn first_health(addr: SocketAddr) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match health_request(addr).await {
            Ok(body) => return body,
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(err) => panic!("health never answered: {err}"),
        }
    }
}

/// Wait until the wrapper future observed its cancellation.
async fn wrapper_cancelled(cancelled: &AtomicBool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !cancelled.load(Ordering::SeqCst) {
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    true
}

async fn healthz() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "ok": true }))
}

#[test]
fn drain_timeout_starts_only_after_shutdown() {
    run(async {
        let listener = loopback_listener();
        let addr = listener.local_addr().expect("local addr");
        let (signal_tx, signal_rx) = tokio::sync::oneshot::channel::<()>();
        let signal = async move {
            let _ = signal_rx.await;
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let background = vec![endless_wrapper(Arc::clone(&cancelled))];
        // The window is far shorter than the serving time before the
        // signal: the deadline must not have started with the server.
        let drain = Duration::from_millis(250);
        let server = tokio::spawn(serve_with_shutdown(
            listener,
            Router::new().route("/healthz", get(healthz)),
            signal,
            background,
            drain,
        ));

        let ready = first_health(addr).await;
        assert!(ready.contains("200"), "readiness answered: {ready}");
        assert!(ready.contains("\"ok\":true"), "health body: {ready}");
        tokio::time::sleep(drain * 2).await;
        let later = health_request(addr)
            .await
            .expect("still serving a full drain length after start");
        assert!(later.contains("\"ok\":true"), "health body: {later}");

        let signalled = Instant::now();
        signal_tx.send(()).expect("signal delivered");
        let outcome = server
            .await
            .expect("server task joins")
            .expect("drain outcome");
        assert_eq!(outcome, DrainOutcome::Drained);
        assert!(
            signalled.elapsed() < drain,
            "drain finished inside the window, took {:?}",
            signalled.elapsed()
        );
        assert!(
            wrapper_cancelled(&cancelled).await,
            "wrapper was cancelled when draining began"
        );
    })
    .expect("runtime builds");
}

/// State for the pending handler: it acks once it is parked in the
/// request it never answers.
#[derive(Clone)]
struct PendingAck(Arc<Notify>);

async fn pending_handler(State(ack): State<PendingAck>) {
    ack.0.notify_one();
    std::future::pending::<()>().await;
}

#[test]
fn drain_deadline_expires_over_a_pending_handler() {
    run(async {
        let listener = loopback_listener();
        let addr = listener.local_addr().expect("local addr");
        let parked = Arc::new(Notify::new());
        let app = Router::new()
            .route("/pending", get(pending_handler))
            .with_state(PendingAck(Arc::clone(&parked)));
        let (signal_tx, signal_rx) = tokio::sync::oneshot::channel::<()>();
        let signal = async move {
            let _ = signal_rx.await;
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let background = vec![endless_wrapper(Arc::clone(&cancelled))];
        let drain = Duration::from_millis(250);
        let server = tokio::spawn(serve_with_shutdown(
            listener, app, signal, background, drain,
        ));

        // A request the handler never answers; wait until it is parked.
        let mut conn = tokio::net::TcpStream::connect(addr).await.expect("connect");
        conn.write_all(b"GET /pending HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("request written");
        tokio::time::timeout(Duration::from_secs(5), parked.notified())
            .await
            .expect("pending handler parked");

        signal_tx.send(()).expect("signal delivered");
        let signalled = Instant::now();
        let outcome = server
            .await
            .expect("server task joins")
            .expect("clean exit");
        assert_eq!(outcome, DrainOutcome::Expired);
        assert!(
            signalled.elapsed() >= drain - Duration::from_millis(50),
            "the window was enforced, took {:?}",
            signalled.elapsed()
        );
        assert!(
            wrapper_cancelled(&cancelled).await,
            "wrapper was cancelled when draining began"
        );
    })
    .expect("runtime builds");
}
