#![cfg(unix)]
#![allow(dead_code)]
//! A real-server fixture for Unix shutdown tests: launch the actual `vivi`
//! binary as `vivi server run`, isolated from the host environment, wait
//! for `/healthz` over a real socket, signal it with the Unix `kill`
//! utility, and time the exit.
//!
//! Include it from a Unix-only test target with
//! `#[path = "server_shutdown/support.rs"] mod support;`. The facilities
//! are general on purpose — bounded request writing, bounded pending/EOF
//! checks, and signal-relative exit timing — so later targets can reuse
//! the same fixture for authenticated long-poll requests. Nothing here
//! assumes an idle-only request.

use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A known-valid `STORE_ENCRYPTION_KEY`: standard base64 of the 32 bytes
/// `0x00..=0x1f`. The child boots with a valid key regardless of what the
/// host environment holds.
pub const STORE_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

/// Ceiling on one child's signal-to-exit time. An idle server exits well
/// inside it; only a wedged child needs the full window.
pub const PROMPT_EXIT_CEILING: Duration = Duration::from_secs(5);

/// Ceiling on one readiness attempt (polls until then, then fresh port).
const READY_CEILING: Duration = Duration::from_secs(10);
/// Ceiling on one bounded request: connect, complete write, read to EOF.
const REQUEST_CEILING: Duration = Duration::from_secs(5);
/// Fresh-port attempts before giving up on the reserve-then-bind race.
const PORT_ATTEMPTS: usize = 5;
/// Pause between readiness polls and between exit polls.
const POLL: Duration = Duration::from_millis(25);

/// A running, isolated `vivi server run` child.
pub struct Server {
    child: Child,
    addr: SocketAddr,
    scratch: PathBuf,
}

impl Server {
    /// Launch an isolated `vivi server run` and wait until `/healthz`
    /// answers `200`.
    ///
    /// Each attempt reserves an ephemeral port and hands it to the child.
    /// Reserving and releasing a port races with the rest of the machine,
    /// so a lost port means a retry on a fresh one, a bounded number of
    /// times. A child that leaves before readiness for any other reason is
    /// reported with its captured output.
    pub fn spawn_ready() -> Server {
        for _ in 0..PORT_ATTEMPTS {
            let mut attempt = Server::spawn_at(reserve_port());
            match wait_ready(attempt.addr, &mut attempt.child) {
                Ok(()) => return attempt,
                Err(ReadyFailure::Exited(status)) => {
                    let output = attempt.output();
                    if !status.success() && output.contains("cannot bind") {
                        // The reserved port was lost to another process
                        // between release and bind; only a fresh port helps.
                        continue;
                    }
                    panic!("vivi server run left before readiness ({status}):\n{output}");
                }
                Err(ReadyFailure::Unready) => continue,
            }
        }
        panic!("no reserved port reached the child in {PORT_ATTEMPTS} fresh-port tries");
    }

    /// Launch one child with a fully child-local environment.
    fn spawn_at(port: u16) -> Server {
        let scratch = std::env::temp_dir().join(unique_tag());
        // An empty credential directory: no inherited systemd credential
        // file can satisfy a secret lookup.
        let credentials = scratch.join("credentials");
        fs::create_dir_all(&credentials).expect("create child credential dir");
        let mut command = Command::new(env!("CARGO_BIN_EXE_vivi"));
        command
            .args(["server", "run"])
            // Child-local settings only; the parent environment is never
            // changed. DATA_DIR and VIVI_CONFIG_DIR are absolute, and
            // VIVI_CONFIG_DIR keeps the host registry out of the real one.
            .env("BIND", "127.0.0.1")
            .env("PORT", port.to_string())
            .env("DATA_DIR", scratch.join("data"))
            .env("VIVI_CONFIG_DIR", scratch.join("config"))
            .env("CREDENTIALS_DIRECTORY", &credentials)
            .env("STORE_ENCRYPTION_KEY", STORE_KEY)
            .env_remove("MODERATION_TOKEN")
            .env_remove("HOST_OPERATOR_TOKEN")
            .env_remove("HOSTED_DEFAULT_IDENTITY")
            .env_remove("SEND_RATE_PER_MINUTE")
            .env_remove("RUST_LOG")
            // File-backed output: it stays readable after the child is
            // gone, so a failure can report everything the child said.
            .stdout(Stdio::from(
                File::create(scratch.join("stdout.log")).expect("create stdout log"),
            ))
            .stderr(Stdio::from(
                File::create(scratch.join("stderr.log")).expect("create stderr log"),
            ));
        let child = command.spawn().expect("spawn vivi server run");
        Server {
            child,
            addr: SocketAddr::from(([127, 0, 0, 1], port)),
            scratch,
        }
    }

    /// The address the server answers on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The child, for callers that poll it directly.
    pub fn child(&mut self) -> &mut Child {
        &mut self.child
    }

    /// The captured child output so far (stdout, then stderr), labeled.
    /// File-backed, so a dead child's output stays readable.
    pub fn output(&self) -> String {
        let mut combined = String::new();
        for name in ["stdout.log", "stderr.log"] {
            let content = fs::read_to_string(self.scratch.join(name)).unwrap_or_default();
            combined.push_str(&format!("--- {name} ---\n{content}"));
        }
        combined
    }

    /// Send `signal` (`"TERM"`, `"INT"`, ...) with the Unix `kill`
    /// utility: no signal crate, no parent-side globals.
    pub fn send_signal(&self, signal: &str) {
        let status = Command::new("kill")
            .arg(format!("-{signal}"))
            .arg(self.child.id().to_string())
            .status()
            .expect("run kill(1)");
        assert!(
            status.success(),
            "kill -{signal} {} failed",
            self.child.id()
        );
    }

    /// Send `signal` and wait a bounded time for the child to exit.
    /// Returns the exit status and the signal-to-exit duration.
    pub fn signal_and_wait(&mut self, signal: &str) -> (ExitStatus, Duration) {
        let sent_at = Instant::now();
        self.send_signal(signal);
        let deadline = sent_at + PROMPT_EXIT_CEILING;
        loop {
            if let Some(status) = self.child.try_wait().expect("poll server child") {
                return (status, sent_at.elapsed());
            }
            assert!(
                Instant::now() < deadline,
                "server still running after {:?} past SIG{signal}:\n{}",
                PROMPT_EXIT_CEILING,
                self.output()
            );
            std::thread::sleep(POLL);
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Only a failed or panicked test leaves a live child behind; a
        // normal run reaps it first, so this kills and waits exactly when
        // a child would otherwise leak.
        if self.child.try_wait().expect("poll server child").is_none() {
            let _ = Command::new("kill")
                .arg("-KILL")
                .arg(self.child.id().to_string())
                .status();
            let _ = self.child.wait();
        }
    }
}

/// Why one readiness attempt failed.
enum ReadyFailure {
    /// The child left before readiness; the caller reports the output.
    Exited(ExitStatus),
    /// No `200` within the ceiling: likely a lost reserved port.
    Unready,
}

/// Poll `/healthz` until the child answers `200`, the child leaves, or the
/// readiness ceiling passes.
fn wait_ready(addr: SocketAddr, child: &mut Child) -> Result<(), ReadyFailure> {
    let request = "GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    let deadline = Instant::now() + READY_CEILING;
    loop {
        if let Some(status) = child.try_wait().expect("poll server child") {
            return Err(ReadyFailure::Exited(status));
        }
        if let Ok(response) = get(addr, request) {
            if response_status(&response) == "200" {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err(ReadyFailure::Unready);
        }
        std::thread::sleep(POLL);
    }
}

/// One bounded HTTP/1.1 request: the complete `request` is written and the
/// response is read to EOF. The raw response text, or the io error.
pub fn get(addr: SocketAddr, request: &str) -> std::io::Result<String> {
    let mut stream = connect(addr)?;
    write_complete(&mut stream, request.as_bytes())?;
    read_to_eof_bounded(&mut stream, REQUEST_CEILING)
        .map(|raw| String::from_utf8_lossy(&raw).into_owned())
}

/// Connect to `addr` with a bounded connect time. Timeouts for reads and
/// writes are left to each operation.
pub fn connect(addr: SocketAddr) -> std::io::Result<TcpStream> {
    TcpStream::connect_timeout(&addr, REQUEST_CEILING)
}

/// Write a complete request: every byte, with a bounded write.
pub fn write_complete(stream: &mut TcpStream, request: &[u8]) -> std::io::Result<()> {
    stream.set_write_timeout(Some(REQUEST_CEILING))?;
    stream.write_all(request)
}

/// Read until EOF, bounded overall by `ceiling`. The bytes read, or a
/// timeout error when the peer keeps the connection open past the ceiling.
pub fn read_to_eof_bounded(stream: &mut TcpStream, ceiling: Duration) -> std::io::Result<Vec<u8>> {
    let deadline = Instant::now() + ceiling;
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(std::io::Error::new(
                ErrorKind::TimedOut,
                "no EOF before the ceiling",
            ));
        }
        stream.set_read_timeout(Some(remaining))?;
        match stream.read(&mut buf) {
            Ok(0) => return Ok(raw),
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(std::io::Error::new(
                    ErrorKind::TimedOut,
                    "no EOF before the ceiling",
                ));
            }
            Err(err) => return Err(err),
        }
    }
}

/// The connection stays quiet for the whole `window`: no bytes and no EOF
/// arrive. Panics when the peer answers or closes inside the window.
pub fn assert_pending(stream: &mut TcpStream, window: Duration) {
    let deadline = Instant::now() + window;
    let mut buf = [0u8; 64];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return; // quiet for the whole window: still pending
        }
        stream
            .set_read_timeout(Some(remaining))
            .expect("set read timeout");
        match stream.read(&mut buf) {
            Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return;
            }
            Ok(0) => panic!("connection reached EOF within {window:?}; expected pending"),
            Ok(n) => panic!("connection sent {n} bytes within {window:?}; expected pending"),
            Err(err) => panic!("pending read failed: {err}"),
        }
    }
}

/// The HTTP status token of a raw response: `"200"` for `HTTP/1.1 200 OK`.
pub fn response_status(response: &str) -> &str {
    response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("")
}

/// Reserve an ephemeral loopback port and release it again. The gap before
/// the child binds the same port is a race, so [`Server::spawn_ready`]
/// retries on a fresh reservation instead of trusting one.
fn reserve_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve a loopback port");
    listener.local_addr().expect("reserved port address").port()
}

/// A fresh scratch name per spawn, so two children never share a directory.
fn unique_tag() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "vivi-server-shutdown-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
