//! The `ssh` command line as a subprocess, one call at a time.
//!
//! Every call is `ssh` with the options a batch job needs (no prompt, a
//! connect timeout, no forwards, no terminal) and one remote command. Calls
//! to the same host share one connection through a control socket in a
//! directory every vivi process of this user shares (see
//! [`shared_control_dir`]), so the pump's many short `vivi pool ...`
//! subprocesses reuse one login per host; the connection ends after
//! [`PERSIST_SECS`] idle. Without that directory a runner keeps a private one
//! whose connection ends with it. A master that refuses a new session is
//! closed, then the call is retried once without multiplexing.
//! [`Runner`] is the seam tests replace: its one argument is the remote
//! command line.

use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::docker_cli::{RunOutput, Runner, finish};
use vivi_error::VivariumError;

/// Seconds `ssh` waits to connect before it gives up (no key, no route).
const CONNECT_TIMEOUT_SECS: &str = "10";
/// Bound the best-effort shutdown of a master that refused a session.
const MASTER_EXIT_TIMEOUT: Duration = Duration::from_secs(5);
/// Seconds an idle shared connection lives on after the last call.
pub(crate) const PERSIST_SECS: &str = "60";
/// Longest control socket directory a Unix socket path can take with room to
/// spare (the limit is about 104 bytes on some systems, and `ssh` appends a
/// random suffix while binding).
const MAX_CONTROL_DIR: usize = 60;

/// How much more of the socket path a shared socket's `%C` name takes than a
/// private socket's `m`, plus the directory's own `vivi-ssh-<uid>` name.
const SHARED_NAME_ROOM: usize = 40 + 20;

/// Numbers the socket directories of the runners of one process.
static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

/// The real `ssh` binary against one host.
#[derive(Debug)]
pub struct SshCli {
    host: String,
    /// Options after the standard ones: a port and a pinned host key.
    extra: Vec<String>,
    /// Directory holding the shared connection's control socket; none when
    /// no private directory could be made and every call connects on its own.
    control_dir: Option<PathBuf>,
    /// Whether `control_dir` is this runner's own (removed, with its
    /// connection, when the runner drops) rather than the shared one.
    owns_control_dir: bool,
}

impl SshCli {
    /// A runner for `host`, sharing one connection between calls when a
    /// private socket directory can be made.
    #[must_use]
    pub fn new(host: &str) -> Self {
        let (control_dir, owns_control_dir) = match shared_control_dir() {
            Some(dir) => (Some(dir), false),
            None => (control_dir(), true),
        };
        Self {
            host: host.to_string(),
            extra: Vec::new(),
            control_dir,
            owns_control_dir,
        }
    }

    /// A runner for `host` on `port` that trusts exactly the host keys in the
    /// file `known_hosts` and nothing else: strict checking, no other
    /// known-hosts file, no learning. A changed or missing key fails the call.
    /// Its connection is never shared.
    #[must_use]
    pub fn pinned(host: &str, port: u16, known_hosts: &std::path::Path) -> Self {
        // A private socket: a shared connection to the same address opened
        // without the pin would carry this runner's calls past its check.
        let mut cli = Self {
            host: host.to_string(),
            extra: Vec::new(),
            control_dir: control_dir(),
            owns_control_dir: true,
        };
        cli.extra = vec![
            "-p".to_string(),
            port.to_string(),
            "-o".to_string(),
            format!("UserKnownHostsFile={}", known_hosts.display()),
            "-o".to_string(),
            "GlobalKnownHostsFile=/dev/null".to_string(),
            "-o".to_string(),
            "StrictHostKeyChecking=yes".to_string(),
            "-o".to_string(),
            "CheckHostIP=no".to_string(),
            "-o".to_string(),
            "UpdateHostKeys=no".to_string(),
        ];
        cli
    }

    /// The process for one remote command line.
    #[must_use]
    pub fn command(&self, remote: &str) -> Command {
        self.session_command(remote, true)
    }

    fn session_command(&self, remote: &str, multiplex: bool) -> Command {
        let mut command = Command::new("ssh");
        if !multiplex {
            // Put overrides first: OpenSSH uses the first value of an option.
            // Bypass even a master another process opened during recovery.
            command.args([
                "-S",
                "none",
                "-o",
                "ControlMaster=no",
                "-o",
                "ControlPersist=no",
            ]);
        }
        command.args(self.options());
        command.arg("--").arg(&self.host).arg(remote);
        command
    }

    fn master_exit_command(&self) -> Command {
        let mut command = Command::new("ssh");
        // Keep the same control path (including %C expansion), port, user
        // and pinned-host options as the failed call.
        command.args(self.options());
        command.args(["-O", "exit", "--", &self.host]);
        command
    }

    /// Retry only a refused mux session: no remote command started, so even
    /// a mutating call and its stdin can safely be replayed. Other failures
    /// (including generic SSH exit 255) may have run a command; leave them be.
    fn run_with(
        &self,
        remote: &str,
        stdin: Option<&[u8]>,
        timeout: Duration,
        mut execute: impl FnMut(Command, Option<&[u8]>, Duration) -> Result<RunOutput, VivariumError>,
    ) -> Result<RunOutput, VivariumError> {
        let out = execute(self.command(remote), stdin, timeout)?;
        if !refused_mux_session(&out) {
            return Ok(out);
        }
        let closed = execute(self.master_exit_command(), None, MASTER_EXIT_TIMEOUT);
        tracing::warn!(
            host = %self.host,
            master_closed = closed.as_ref().is_ok_and(RunOutput::succeeded),
            "SSH master refused a session; closing it and retrying once on a fresh connection"
        );
        // Do not recurse: a second refusal is returned exactly as before.
        execute(self.session_command(remote, false), stdin, timeout)
    }

    /// Everything before the host: batch mode and a connect timeout so a
    /// missing key fails at once instead of asking, nothing that changes the
    /// remote command (forwards, terminal, a configured `RemoteCommand`), and
    /// the shared connection.
    #[must_use]
    pub fn options(&self) -> Vec<String> {
        let mut options: Vec<String> = [
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            &format!("ConnectTimeout={CONNECT_TIMEOUT_SECS}"),
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=4",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "RemoteCommand=none",
            "-o",
            "RequestTTY=no",
            "-o",
            "LogLevel=ERROR",
        ]
        .map(str::to_string)
        .to_vec();
        options.extend(self.extra.iter().cloned());
        if let Some(dir) = &self.control_dir {
            options.extend(control_options(&self.control_path(dir)));
        }
        options
    }

    /// The control socket in `dir`: one per destination in the shared
    /// directory (`%C`, ssh's hash of host, port and user), the single `m` in
    /// a private one.
    fn control_path(&self, dir: &std::path::Path) -> PathBuf {
        dir.join(if self.owns_control_dir { "m" } else { "%C" })
    }
}

/// `ssh` options that share one connection per destination through
/// `control_path`, kept open [`PERSIST_SECS`] after the last call.
#[must_use]
pub(crate) fn control_options(control_path: &std::path::Path) -> [String; 6] {
    [
        "-o".to_string(),
        "ControlMaster=auto".to_string(),
        "-o".to_string(),
        format!("ControlPath={}", control_path.display()),
        "-o".to_string(),
        format!("ControlPersist={PERSIST_SECS}"),
    ]
}

/// The control socket directory every vivi process of this user shares:
/// `vivi-ssh-<uid>` in the temporary directory (or `/tmp` when that path
/// leaves too little room for a socket), made mode 0700. None unless it is a real directory
/// owned by this user with no access for anyone else: a socket there lets its
/// user run commands as this user on the remote host.
#[must_use]
pub(crate) fn shared_control_dir() -> Option<PathBuf> {
    // A shared socket is named `%C` (40 hex characters) where a private one
    // is `m`, so its directory gets that much less room: macOS's per-user
    // temporary directory is already too long, and there ssh quietly
    // connects without sharing.
    let mut base = std::env::temp_dir();
    if base.as_os_str().len() > MAX_CONTROL_DIR - SHARED_NAME_ROOM {
        base = PathBuf::from("/tmp");
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    let dir = base.join(format!("vivi-ssh-{uid}"));
    drop(std::fs::DirBuilder::new().mode(0o700).create(&dir));
    private_to(&dir, uid).then_some(dir)
}

/// Whether `dir` is a real directory (not a symlink) owned by `uid` that no
/// one else can enter.
fn private_to(dir: &std::path::Path, uid: u32) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    std::fs::symlink_metadata(dir).is_ok_and(|meta| {
        meta.is_dir() && meta.uid() == uid && meta.permissions().mode() & 0o077 == 0
    })
}

/// A fresh private directory for the control socket, or none (calls then
/// each open their own connection).
fn control_dir() -> Option<PathBuf> {
    let mut base = std::env::temp_dir();
    if base.as_os_str().len() > MAX_CONTROL_DIR {
        base = PathBuf::from("/tmp");
    }
    let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let dir = base.join(format!("vivi-ssh-{}-{n}", std::process::id()));
    let made = std::fs::DirBuilder::new().mode(0o700).create(&dir);
    made.is_ok().then_some(dir)
}

impl Runner for SshCli {
    fn run(
        &self,
        args: &[String],
        stdin: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<RunOutput, VivariumError> {
        let remote = args.first().map_or("", String::as_str);
        self.run_with(remote, stdin, timeout, |command, input, limit| {
            execute(command, input, limit, &self.host)
        })
    }
}

/// OpenSSH's mux client exits nonzero (normally 255) before starting the
/// remote command when the master cannot open a session. The exit code alone
/// also covers authentication, network and potentially remote failures.
fn refused_mux_session(out: &RunOutput) -> bool {
    !out.succeeded()
        && out.stderr.contains(
            "mux_client_request_session: session request failed: Session open refused by peer",
        )
}

fn execute(
    mut command: Command,
    stdin: Option<&[u8]>,
    timeout: Duration,
    host: &str,
) -> Result<RunOutput, VivariumError> {
    let child = command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            VivariumError::Message(format!(
                "failed to run `ssh` (is OpenSSH installed and on PATH?): {e}"
            ))
        })?;
    finish(child, stdin, timeout, &format!("ssh {host}"))
}

impl Drop for SshCli {
    /// End the shared connection and remove its socket directory.
    fn drop(&mut self) {
        // The shared directory's connections serve other processes too; they
        // end on their own once idle.
        if !self.owns_control_dir {
            return;
        }
        let Some(dir) = self.control_dir.take() else {
            return;
        };
        if dir.join("m").exists() {
            Command::new("ssh")
                .args(["-O", "exit", "-o"])
                .arg(format!("ControlPath={}", dir.join("m").display()))
                .args(["--", &self.host])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(drop)
                .unwrap_or_default();
        }
        std::fs::remove_dir_all(&dir).map(drop).unwrap_or_default();
    }
}

#[cfg(test)]
#[path = "ssh_cli_test.rs"]
mod tests;
