//! Independent stream-drain tests: a response body that never completes
//! must keep flowing through the signal-relative drain window and be
//! force-closed when `run` consumes its runtime at the drain deadline.
//! The client stays on the parent thread while `run` owns the serving
//! runtime on a dedicated thread, so closure of the socket after runtime
//! teardown is observable from outside.

use std::io::{ErrorKind, Read, Write};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::routing::get;
use bytes::Bytes;
use tokio::sync::Notify;
use tokio::sync::mpsc::error::TryRecvError;

use super::{DrainOutcome, endless_wrapper, run, serve_with_shutdown};

/// Bounds for acknowledgements, reads and flag spins on the parent.
const ACK_LIMIT: Duration = Duration::from_secs(5);
const READ_LIMIT: Duration = Duration::from_secs(5);
/// Bound on the wait for the serving thread to finish after the signal.
const TEARDOWN_LIMIT: Duration = Duration::from_secs(10);
/// Safety net so a wedged parent cannot leave the serving thread running
/// forever: the real signal always fires long before this.
const SERVE_CLEANUP_LIMIT: Duration = Duration::from_secs(15);

/// Shared with the streaming handler: producer acknowledgements travel to
/// the parent through a Tokio channel, the release opens the gate.
#[derive(Clone)]
struct StreamState {
    ack: tokio::sync::mpsc::UnboundedSender<&'static str>,
    release: Arc<Notify>,
}

/// Body stages: chunk one at once, then hold the gate; chunk two once
/// released, then park forever so neither producer nor connection ever
/// completes on its own.
enum Stage {
    First(StreamState),
    Hold(StreamState),
    Parked,
}

/// One step of the never-completing body stream.
async fn stream_step(stage: Stage) -> Option<(Result<Bytes, std::io::Error>, Stage)> {
    match stage {
        Stage::First(state) => {
            let _ = state.ack.send("first");
            Some((
                Ok(Bytes::from_static(b"stream-chunk-one\n")),
                Stage::Hold(state),
            ))
        }
        Stage::Hold(state) => {
            state.release.notified().await;
            let _ = state.ack.send("second");
            Some((Ok(Bytes::from_static(b"stream-chunk-two\n")), Stage::Parked))
        }
        Stage::Parked => {
            std::future::pending::<Option<(Result<Bytes, std::io::Error>, Stage)>>().await
        }
    }
}

/// A response whose body never ends: the producer stays alive until the
/// runtime tears it down.
async fn unfinished_stream(State(state): State<StreamState>) -> Body {
    Body::from_stream(futures_util::stream::unfold(
        Stage::First(state),
        |stage| async move { stream_step(stage).await },
    ))
}

/// Connect from the parent thread with every later read bounded.
fn bounded_connect(addr: SocketAddr) -> std::net::TcpStream {
    let conn = std::net::TcpStream::connect(addr).expect("client connects");
    conn.set_read_timeout(Some(READ_LIMIT))
        .expect("client read timeout");
    conn
}

/// Wait on the parent thread for the producer's next acknowledgement.
fn bounded_ack(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<&'static str>,
    limit: Duration,
) -> &'static str {
    let deadline = Instant::now() + limit;
    loop {
        match rx.try_recv() {
            Ok(tag) => return tag,
            Err(TryRecvError::Disconnected) => {
                panic!("producer channel closed before acknowledging")
            }
            Err(TryRecvError::Empty) => {}
        }
        if Instant::now() >= deadline {
            panic!("acknowledgement never arrived within {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Spin on a plain flag from the parent thread until it flips or `limit`
/// passes.
fn bounded_flag(flag: &AtomicBool, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if flag.load(Ordering::SeqCst) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

/// Read raw bytes until `marker` appears; every wait is bounded by the
/// socket read timeout and the cumulative deadline.
fn read_until_marker(conn: &mut std::net::TcpStream, marker: &str) -> String {
    let deadline = Instant::now() + READ_LIMIT;
    let mut buf = Vec::new();
    loop {
        if String::from_utf8_lossy(&buf).contains(marker) {
            return String::from_utf8_lossy(&buf).into_owned();
        }
        if Instant::now() >= deadline {
            panic!(
                "never saw {marker}; got so far: {}",
                String::from_utf8_lossy(&buf)
            );
        }
        let mut chunk = [0u8; 1024];
        match conn.read(&mut chunk) {
            Ok(0) => panic!("connection closed before {marker}"),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(err)
                if err.kind() == ErrorKind::Interrupted
                    || err.kind() == ErrorKind::WouldBlock
                    || err.kind() == ErrorKind::TimedOut => {}
            Err(err) => panic!("read failed before {marker}: {err}"),
        }
    }
}

/// After runtime teardown the client must see EOF or a reset: a helper
/// return that left the socket open times out here and fails.
fn assert_closed(conn: &mut std::net::TcpStream) {
    let mut chunk = [0u8; 512];
    loop {
        match conn.read(&mut chunk) {
            Ok(0) => return,
            Ok(_) => continue, // leftover framing; closure still owed
            Err(err) if err.kind() == ErrorKind::ConnectionReset => return,
            Err(err)
                if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::TimedOut =>
            {
                panic!("socket stayed open after runtime teardown")
            }
            Err(err) => panic!("unexpected read failure at teardown: {err}"),
        }
    }
}

/// Drive `run` + `serve_with_shutdown` on a dedicated thread: the runtime
/// it owns is invisible to the parent, so the client there observes real
/// teardown. The drain-proof wrapper is spawned inside that runtime (its
/// `tokio::spawn` needs one) and reports through `cancelled`. The outcome
/// lands on `done` once the runtime is consumed.
fn serve_until_signalled(
    std_listener: std::net::TcpListener,
    app: Router,
    signal_rx: tokio::sync::oneshot::Receiver<()>,
    cancelled: Arc<AtomicBool>,
    drain: Duration,
    done: mpsc::Sender<DrainOutcome>,
) {
    std::thread::spawn(move || {
        let outcome = run(async move {
            let listener = tokio::net::TcpListener::from_std(std_listener)
                .expect("listener joins the runtime");
            let background = vec![endless_wrapper(cancelled)];
            serve_with_shutdown(
                listener,
                app,
                async move {
                    let _ = tokio::time::timeout(SERVE_CLEANUP_LIMIT, signal_rx).await;
                },
                background,
                drain,
            )
            .await
        })
        .expect("runtime builds")
        .expect("drain outcome");
        let _ = done.send(outcome);
    });
}

#[test]
fn response_stream_survives_until_the_drain_deadline() {
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback bind");
    std_listener
        .set_nonblocking(true)
        .expect("listener set non-blocking");
    let addr = std_listener.local_addr().expect("local addr");
    let (ack_tx, mut ack_rx) = tokio::sync::mpsc::unbounded_channel::<&'static str>();
    let (signal_tx, signal_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel();
    let release = Arc::new(Notify::new());
    let cancelled = Arc::new(AtomicBool::new(false));
    let app = Router::new()
        .route("/stream", get(unfinished_stream))
        .with_state(StreamState {
            ack: ack_tx,
            release: Arc::clone(&release),
        });
    let drain = Duration::from_millis(750);
    serve_until_signalled(
        std_listener,
        app,
        signal_rx,
        Arc::clone(&cancelled),
        drain,
        done_tx,
    );

    // The client stays on the parent thread: only std IO here, so any
    // close must come from the server's runtime teardown.
    let mut conn = bounded_connect(addr);
    conn.write_all(b"GET /stream HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .expect("request written");
    assert_eq!(bounded_ack(&mut ack_rx, ACK_LIMIT), "first");
    let first = read_until_marker(&mut conn, "stream-chunk-one");
    assert!(first.contains("200"), "streaming headers: {first}");

    let signalled = Instant::now();
    let _ = signal_tx.send(());
    assert!(
        bounded_flag(&cancelled, ACK_LIMIT),
        "draining began: wrapper aborted at the signal"
    );
    release.notify_one();
    assert_eq!(
        bounded_ack(&mut ack_rx, ACK_LIMIT),
        "second",
        "producer survived into the drain"
    );
    read_until_marker(&mut conn, "stream-chunk-two");

    let outcome = done_rx
        .recv_timeout(TEARDOWN_LIMIT)
        .expect("serving thread finished inside its cleanup bound");
    let took = signalled.elapsed();
    assert_eq!(outcome, DrainOutcome::Expired);
    assert!(
        took >= drain - Duration::from_millis(50),
        "the window was enforced, took {took:?}"
    );
    assert!(
        took < drain + Duration::from_secs(2),
        "completion stayed near the deadline, took {took:?}"
    );
    assert_closed(&mut conn);
}
