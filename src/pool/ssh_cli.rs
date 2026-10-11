//! The `ssh` command line as a subprocess, one call at a time.
//!
//! Every call is `ssh` with the options a batch job needs (no prompt, a
//! connect timeout, no forwards, no terminal) and one remote command. Calls
//! to the same host share one connection through a private control socket in
//! the user's temporary directory; the master ends with the provider.
//! [`Runner`] is the seam tests replace: its one argument is the remote
//! command line.

use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::docker_cli::{RunOutput, Runner, finish};
use crate::error::VivariumError;

/// Seconds `ssh` waits to connect before it gives up (no key, no route).
const CONNECT_TIMEOUT_SECS: &str = "10";
/// Seconds an idle shared connection lives on after the last call.
const PERSIST_SECS: &str = "60";
/// Longest control socket directory a Unix socket path can take with room to
/// spare (the limit is about 104 bytes on some systems, and `ssh` appends a
/// random suffix while binding).
const MAX_CONTROL_DIR: usize = 60;

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
}

impl SshCli {
    /// A runner for `host`, sharing one connection between calls when a
    /// private socket directory can be made.
    #[must_use]
    pub fn new(host: &str) -> Self {
        Self {
            host: host.to_string(),
            extra: Vec::new(),
            control_dir: control_dir(),
        }
    }

    /// A runner for `host` on `port` that trusts exactly the host keys in the
    /// file `known_hosts` and nothing else: strict checking, no other
    /// known-hosts file, no learning. A changed or missing key fails the call.
    #[must_use]
    pub fn pinned(host: &str, port: u16, known_hosts: &std::path::Path) -> Self {
        let mut cli = Self::new(host);
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
        let mut command = Command::new("ssh");
        command.args(self.options());
        command.arg("--").arg(&self.host).arg(remote);
        command
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
            options.extend([
                "-o".to_string(),
                "ControlMaster=auto".to_string(),
                "-o".to_string(),
                format!("ControlPath={}", dir.join("m").display()),
                "-o".to_string(),
                format!("ControlPersist={PERSIST_SECS}"),
            ]);
        }
        options
    }
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
        let child = self
            .command(remote)
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
        finish(child, stdin, timeout, &format!("ssh {}", self.host))
    }
}

impl Drop for SshCli {
    /// End the shared connection and remove its socket directory.
    fn drop(&mut self) {
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
