#![allow(dead_code)]
//! The standalone server process: launch the real vivi binary as
//! `vivi server run`, hold it under a bounded readiness deadline, kill and
//! reap it on success, failure, or unwind, and account for its CPU.
//!
//! This is the measurement counterpart of `tests/server_cli.rs`: the same
//! real binary and the same environment-only configuration, but kept alive
//! as a child of the test instead of run to exit. The binary is
//! `CARGO_BIN_EXE_vivi` — the link `cargo test` itself produced, so a
//! measurement can never boot a stale artifact; a run that needs
//! release-codegen numbers should build one and boot it through these same
//! helpers.
//!
//! Isolation: the child inherits neither server settings nor server
//! secrets. [`ServerSpec`] names the data directory (`DATA_DIR`), the
//! config directory (`VIVI_CONFIG_DIR`), the bind address (`BIND`), and a
//! free loopback port (`PORT`); every other server variable — including
//! `CREDENTIALS_DIRECTORY` and the three secret names — is removed from the
//! child's environment, so a disposable server runs on its own defaults
//! with no host credentials within reach. Only a loopback server is ever
//! started.
//!
//! CPU accounting: sysinfo exposes a process's CPU only as a per-refresh
//! percentage, but the macOS and Linux backends derive that percentage from
//! real kernel accounting deltas (task totals, `utime`+`stime`). So
//! [`CpuSampler`] integrates the measured percentage over the measured
//! refresh interval, holding the interval at or above sysinfo's
//! `MINIMUM_CPU_UPDATE_INTERVAL` so every refresh reports a fresh real
//! window. The sampler only ever measures the PID it is given — the
//! server's, never the test/writer process. Accounting is never
//! substituted: on a platform whose sysinfo backend does not derive usage
//! from real accounting, sampling is an explicit
//! [`AccountingError::Unsupported`], and a process that disappears mid-run
//! is an explicit [`AccountingError::Gone`] — never elapsed wall time or a
//! guessed CPU percentage.
//!
//! Include from a test target with
//! `#[path = "support/hosted_process.rs"] mod hosted_process;`.

use std::io::Read;
use std::io::Write;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sysinfo::{MINIMUM_CPU_UPDATE_INTERVAL, Pid, ProcessesToUpdate, System};

/// The binary under test: the vivi binary the test run linked.
const VIVI_BIN: &str = env!("CARGO_BIN_EXE_vivi");

/// Server settings the child must not inherit: the harness sets exactly the
/// values its [`ServerSpec`] names, and the rest stay unset so the server
/// boots on its own defaults.
const INHERITED_SETTINGS: &[&str] = &[
    "DATA_DIR",
    "VIVI_CONFIG_DIR",
    "BIND",
    "PORT",
    "SEND_RATE_PER_MINUTE",
    "HOSTED_DEFAULT_IDENTITY",
    "HOST_PROJECTS_ROOT",
    "HOST_LANES_ROOT",
    "HOST_GIT_URL_PREFIX",
    "HOST_DEFAULT_POOL",
    "RUST_LOG",
];

/// Secrets and credential plumbing the child must not inherit: a disposable
/// server runs with no keys at all and never sees the host's credentials.
const INHERITED_SECRETS: &[&str] = &[
    "STORE_ENCRYPTION_KEY",
    "MODERATION_TOKEN",
    "HOST_OPERATOR_TOKEN",
    "CREDENTIALS_DIRECTORY",
];

/// Cadence of the readiness poll.
const POLL: Duration = Duration::from_millis(25);

/// Per-attempt connect and read timeout for one readiness probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How much recent stderr a failure report carries.
const STDERR_TAIL: usize = 4_096;

/// Most stderr the harness keeps in memory.
const STDERR_KEEP: usize = 64 * 1024;

/// Smallest gap between two CPU refreshes: above sysinfo's
/// `MINIMUM_CPU_UPDATE_INTERVAL` (200 ms on macOS and Linux), so sysinfo's
/// strict `elapsed() > MINIMUM` check always passes and every refresh
/// computes usage over a fresh real window instead of reusing the previous
/// one.
pub const MIN_SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

const _: () = assert!(
    MINIMUM_CPU_UPDATE_INTERVAL.as_millis() <= 200,
    "MIN_SAMPLE_INTERVAL must stay above sysinfo's refresh floor"
);

/// Where and how the disposable server listens.
#[derive(Clone, Debug)]
pub struct ServerSpec {
    /// `DATA_DIR`: required by the server, and absolute.
    pub data_dir: std::path::PathBuf,
    /// `VIVI_CONFIG_DIR`: the child's own registry home, never the host's.
    pub config_dir: std::path::PathBuf,
    /// `BIND`: loopback — a disposable server never faces outward.
    pub bind: IpAddr,
    /// `PORT`: a free loopback port at construction time.
    pub port: u16,
}

impl ServerSpec {
    /// A spec for `data_dir` and `config_dir` on a free loopback port.
    pub fn new(data_dir: std::path::PathBuf, config_dir: std::path::PathBuf) -> Self {
        Self {
            data_dir,
            config_dir,
            bind: IpAddr::from([127, 0, 0, 1]),
            port: free_loopback_port(),
        }
    }
}

/// A loopback port that was free a moment ago.
#[must_use]
pub fn free_loopback_port() -> u16 {
    TcpListener::bind((IpAddr::from([127, 0, 0, 1]), 0))
        .expect("bind a loopback probe port")
        .local_addr()
        .expect("probe port address")
        .port()
}

/// Whether something is accepting connections on `bind:port` right now.
pub fn accepts(bind: IpAddr, port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from((bind, port)), PROBE_TIMEOUT).is_ok()
}

/// Whether `GET /healthz` on `bind:port` answers `HTTP/1.1 200` — the
/// server's own always-on readiness route, which only answers once the
/// router is serving.
pub fn health_ok(bind: IpAddr, port: u16) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&SocketAddr::from((bind, port)), PROBE_TIMEOUT)
    else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(PROBE_TIMEOUT));
    let request =
        format!("GET /healthz HTTP/1.1\r\nHost: {bind}:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = Vec::new();
    match stream.read_to_end(&mut response) {
        Ok(_) => response.starts_with(b"HTTP/1.1 200"),
        Err(_) => false,
    }
}

/// Whether `pid` is a live process right now (a reaped PID is gone).
pub fn process_alive(pid: u32) -> bool {
    let mut system = System::new();
    let target = Pid::from_u32(pid);
    system.refresh_processes(ProcessesToUpdate::Some(&[target]), true);
    system.process(target).is_some()
}

/// Why a server never became ready.
#[derive(Debug)]
pub enum Unready {
    /// The process exited before it was ready; the server prints its own
    /// boot errors on stderr, which the harness kept.
    Exited { status: String, stderr: String },
    /// The deadline passed with the process alive but not ready.
    TimedOut { after: Duration, stderr: String },
}

/// The disposable server process: the real vivi binary running
/// `vivi server run` on a loopback port, reaped by [`Drop`] no matter how
/// the test ends.
pub struct HostedServer {
    spec: ServerSpec,
    child: Option<Child>,
    /// The server's OS process id: the only PID CPU accounting points at.
    pub pid: u32,
    stderr: Arc<Mutex<Vec<u8>>>,
}

impl HostedServer {
    /// Spawn the real binary as `vivi server run` under `spec`. The child
    /// gets exactly `spec`'s settings, inherits no server secrets, and
    /// logs to stderr, which the harness drains (bounded) so a chatty
    /// server cannot wedge on a full pipe.
    pub fn spawn(spec: ServerSpec) -> Self {
        assert!(
            spec.data_dir.is_absolute(),
            "DATA_DIR must be absolute: {}",
            spec.data_dir.display()
        );
        std::fs::create_dir_all(&spec.config_dir).expect("create the isolated config dir");
        let mut command = Command::new(VIVI_BIN);
        command
            .args(["server", "run"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        for name in INHERITED_SETTINGS.iter().chain(INHERITED_SECRETS) {
            command.env_remove(name);
        }
        command
            .env("DATA_DIR", &spec.data_dir)
            .env("VIVI_CONFIG_DIR", &spec.config_dir)
            .env("BIND", spec.bind.to_string())
            .env("PORT", spec.port.to_string());
        let mut child = command
            .spawn()
            .unwrap_or_else(|err| panic!("spawn {VIVI_BIN} server run: {err}"));
        let pid = child.id();
        let stderr = Arc::new(Mutex::new(Vec::new()));
        if let Some(pipe) = child.stderr.take() {
            let sink = Arc::clone(&stderr);
            std::thread::spawn(move || drain_stderr(pipe, sink));
        }
        Self {
            spec,
            child: Some(child),
            pid,
            stderr,
        }
    }

    /// The spec the server was spawned under.
    pub fn spec(&self) -> &ServerSpec {
        &self.spec
    }

    /// Wait until the server answers `GET /healthz` with `200`, the child
    /// exits (an error, with its stderr), or the deadline passes. Returns
    /// with the child still running: shutdown is the caller's business,
    /// and `Drop` covers every exit path anyway.
    ///
    /// # Errors
    /// The process exited, or stayed unready past `deadline`.
    pub fn wait_ready(&mut self, deadline: Duration) -> Result<(), Unready> {
        self.wait_until(deadline, |spec| health_ok(spec.bind, spec.port))
    }

    /// Wait until the port accepts connections — weaker than
    /// [`HostedServer::wait_ready`], for probes of servers that serve
    /// nothing this harness knows how to ask.
    ///
    /// # Errors
    /// The process exited, or stayed unready past `deadline`.
    pub fn wait_listening(&mut self, deadline: Duration) -> Result<(), Unready> {
        self.wait_until(deadline, |spec| accepts(spec.bind, spec.port))
    }

    fn wait_until(
        &mut self,
        deadline: Duration,
        probe: impl Fn(&ServerSpec) -> bool,
    ) -> Result<(), Unready> {
        let until = std::time::Instant::now() + deadline;
        loop {
            if let Some(status) = self.poll_exit() {
                return Err(Unready::Exited {
                    status,
                    stderr: self.stderr_tail(STDERR_TAIL),
                });
            }
            if probe(&self.spec) {
                // The probe was answered; it must have been this process
                // that answered, not one that died mid-probe.
                if let Some(status) = self.poll_exit() {
                    return Err(Unready::Exited {
                        status,
                        stderr: self.stderr_tail(STDERR_TAIL),
                    });
                }
                return Ok(());
            }
            if std::time::Instant::now() >= until {
                return Err(Unready::TimedOut {
                    after: deadline,
                    stderr: self.stderr_tail(STDERR_TAIL),
                });
            }
            std::thread::sleep(POLL);
        }
    }

    fn poll_exit(&mut self) -> Option<String> {
        let child = self.child.as_mut()?;
        child
            .try_wait()
            .expect("poll the server process")
            .map(|status| status.to_string())
    }

    /// The last `limit` bytes of the server's stderr, lossily decoded —
    /// boot errors reach the reader through this.
    #[must_use]
    pub fn stderr_tail(&self, limit: usize) -> String {
        let sink = self.stderr.lock().expect("stderr lock");
        let start = sink.len().saturating_sub(limit);
        String::from_utf8_lossy(&sink[start..]).into_owned()
    }

    /// Kill and reap the server. Idempotent; also what [`Drop`] runs.
    pub fn shutdown(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for HostedServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Drain the server's stderr into a bounded buffer, so the child can log
/// freely while the harness keeps only the recent tail.
fn drain_stderr(mut pipe: std::process::ChildStderr, sink: Arc<Mutex<Vec<u8>>>) {
    let mut buf = [0u8; 8_192];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let mut sink = sink.lock().expect("stderr lock");
                sink.extend_from_slice(&buf[..n]);
                let excess = sink.len().saturating_sub(STDERR_KEEP);
                if excess > 0 {
                    sink.drain(..excess);
                }
            }
        }
    }
}

/// Why CPU accounting refused to produce a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountingError {
    /// The process is gone: no accounting, and no pretending otherwise.
    Gone { pid: u32 },
    /// This platform's sysinfo backend does not derive CPU usage from real
    /// accounting, so there is nothing honest to integrate.
    Unsupported { os: &'static str },
}

/// One cumulative-CPU reading of the server process.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// Cumulative CPU seconds the sampler has integrated so far.
    pub cpu_seconds: f64,
    /// When the reading was taken.
    pub at: std::time::Instant,
}

/// CPU burned and wall time elapsed between two [`Sample`]s.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuDelta {
    pub cpu_seconds: f64,
    pub wall_seconds: f64,
}

/// A window that does not advance: no rate is defined, and none is guessed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeltaError(pub &'static str);

impl CpuDelta {
    /// The window between two samples of the same sampler, in sample order.
    ///
    /// # Errors
    /// `end` does not come strictly after `start`, so the wall delta would
    /// be zero or negative.
    pub fn between(start: Sample, end: Sample) -> Result<Self, DeltaError> {
        if end.at <= start.at {
            return Err(DeltaError("the second sample must come after the first"));
        }
        Ok(Self {
            cpu_seconds: end.cpu_seconds - start.cpu_seconds,
            wall_seconds: end.at.duration_since(start.at).as_secs_f64(),
        })
    }

    /// Average cores busy over the window: CPU seconds per wall second.
    #[must_use]
    pub fn average_cores(&self) -> f64 {
        self.cpu_seconds / self.wall_seconds
    }

    /// The same rate as a percentage of one core.
    #[must_use]
    pub fn percent_of_one_core(&self) -> f64 {
        self.average_cores() * 100.0
    }
}

/// Cumulative CPU accounting for one foreign PID: the server's, never the
/// test/writer process's. Integrates sysinfo's per-refresh CPU percentage —
/// which the macOS and Linux backends derive from kernel accounting
/// deltas — over the measured refresh interval, holding the interval at or
/// above [`MIN_SAMPLE_INTERVAL`].
pub struct CpuSampler {
    system: System,
    pid: Pid,
    cumulative: f64,
    last_refresh: std::time::Instant,
}

impl CpuSampler {
    /// Attach to `pid` and prime the accounting baseline. The first value
    /// arrives with the next [`CpuSampler::sample`].
    ///
    /// # Errors
    /// The platform has no honest accounting, or `pid` is not live.
    pub fn attach(pid: u32) -> Result<Self, AccountingError> {
        if !accounting_supported() {
            return Err(AccountingError::Unsupported {
                os: std::env::consts::OS,
            });
        }
        let mut sampler = Self {
            system: System::new(),
            pid: Pid::from_u32(pid),
            cumulative: 0.0,
            last_refresh: std::time::Instant::now(),
        };
        sampler.refresh()?;
        Ok(sampler)
    }

    /// Take one reading: hold the refresh cadence, refresh, and integrate
    /// the measured usage over the measured interval.
    ///
    /// # Errors
    /// The process disappeared, or the platform has no honest accounting.
    pub fn sample(&mut self) -> Result<Sample, AccountingError> {
        let since_last = self.last_refresh.elapsed();
        if since_last < MIN_SAMPLE_INTERVAL {
            std::thread::sleep(MIN_SAMPLE_INTERVAL - since_last);
        }
        self.refresh()?;
        let interval = self.last_refresh.elapsed();
        self.last_refresh = std::time::Instant::now();
        // refresh() just proved the process is there; sysinfo derives this
        // percentage from real accounting deltas on supported platforms.
        let usage = self
            .system
            .process(self.pid)
            .map_or(0.0, |process| f64::from(process.cpu_usage()));
        self.cumulative += usage / 100.0 * interval.as_secs_f64();
        Ok(Sample {
            cpu_seconds: self.cumulative,
            at: self.last_refresh,
        })
    }

    /// Cumulative CPU seconds integrated so far (zero until the first
    /// sample; the priming refresh carries no window).
    #[must_use]
    pub fn cumulative(&self) -> f64 {
        self.cumulative
    }

    /// The server's process id.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid.as_u32()
    }

    fn refresh(&mut self) -> Result<(), AccountingError> {
        self.system
            .refresh_processes(ProcessesToUpdate::Some(&[self.pid]), true);
        if self.system.process(self.pid).is_none() {
            return Err(AccountingError::Gone {
                pid: self.pid.as_u32(),
            });
        }
        Ok(())
    }
}

/// Platforms whose sysinfo backend computes `cpu_usage()` from kernel
/// accounting deltas. Anything else refuses: explicit, never substituted.
fn accounting_supported() -> bool {
    matches!(
        std::env::consts::OS,
        "linux" | "macos" | "windows" | "freebsd"
    )
}
