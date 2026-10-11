//! The `docker` command line as a subprocess, one call at a time.
//!
//! The daemon address (`[pool.docker].host`) travels in `DOCKER_HOST`, so a
//! local and a remote daemon go through the same code. Nothing here knows
//! about pools; [`Runner`] is the seam tests replace.

use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::error::VivariumError;

/// How long an ordinary call may take.
pub const CALL_TIMEOUT: Duration = Duration::from_mins(2);
/// How long a pull may take.
pub const PULL_TIMEOUT: Duration = Duration::from_mins(20);

/// What one `docker` call produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    /// Exit code; `-1` when the process was killed by a signal.
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl RunOutput {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.code == 0
    }

    #[must_use]
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_string()
    }
}

/// Runs `docker` with arguments, optional standard input, and a time limit.
/// A failing exit code is a result, not an error: only a call that could not
/// run, or ran out of time, is an `Err`.
pub trait Runner: Send + Sync {
    /// # Errors
    /// Returns an error if the program cannot run or exceeds `timeout`.
    fn run(
        &self,
        args: &[String],
        stdin: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<RunOutput, VivariumError>;
}

/// The real `docker` binary against the configured daemon.
#[derive(Debug, Clone)]
pub struct DockerCli {
    bin: String,
    host: Option<String>,
}

impl DockerCli {
    #[must_use]
    pub fn new(host: Option<&str>) -> Self {
        Self {
            bin: "docker".to_string(),
            host: host.map(str::to_string),
        }
    }

    /// The process for `args`: the daemon host goes in `DOCKER_HOST`; the rest
    /// of the environment is inherited, which is how a `docker create -e NAME`
    /// finds the value of `NAME` without it ever appearing in an argument.
    #[must_use]
    pub fn command(&self, args: &[String]) -> Command {
        let mut command = Command::new(&self.bin);
        command.args(args);
        if let Some(host) = &self.host {
            command.env("DOCKER_HOST", host);
        }
        command
    }
}

impl DockerCli {
    fn spawn(&self, args: &[String], piped_stdin: bool) -> Result<Child, VivariumError> {
        let mut command = self.command(args);
        command
            .stdin(if piped_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.spawn().map_err(|e| {
            VivariumError::Message(format!(
                "failed to run `{}` (is Docker installed and on PATH?): {e}",
                self.bin
            ))
        })
    }
}

impl Runner for DockerCli {
    fn run(
        &self,
        args: &[String],
        stdin: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<RunOutput, VivariumError> {
        let mut child = self.spawn(args, stdin.is_some())?;
        let feeder = child
            .stdin
            .take()
            .zip(stdin.map(<[u8]>::to_vec))
            .map(|(mut pipe, bytes)| {
                std::thread::spawn(move || {
                    // A child that exits early closes the pipe; its exit code
                    // and stderr already say why.
                    pipe.write_all(&bytes).map(drop).unwrap_or_default();
                })
            });
        let out = drain(child.stdout.take());
        let err = drain(child.stderr.take());
        let status = wait_within(&mut child, timeout, args)?;
        if let Some(feeder) = feeder {
            feeder.join().map(drop).unwrap_or_default();
        }
        Ok(RunOutput {
            code: status.code().unwrap_or(-1),
            stdout: out.join().unwrap_or_default(),
            stderr: String::from_utf8_lossy(&err.join().unwrap_or_default()).into_owned(),
        })
    }
}

/// Wait for the child, killing it when `timeout` passes.
fn wait_within(
    child: &mut Child,
    timeout: Duration,
    args: &[String],
) -> Result<ExitStatus, VivariumError> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if started.elapsed() >= timeout => {
                // Reap the child; its exit code is irrelevant after a kill.
                child
                    .kill()
                    .and_then(|()| child.wait())
                    .map(drop)
                    .unwrap_or_default();
                return Err(VivariumError::Message(format!(
                    "`docker {}` did not finish within {}s",
                    args.first().map_or("", String::as_str),
                    timeout.as_secs()
                )));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(15)),
            Err(e) => return Err(VivariumError::Message(format!("docker wait failed: {e}"))),
        }
    }
}

/// Read a pipe to its end on its own thread, so a full pipe never stalls the
/// child.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe
            && pipe.read_to_end(&mut bytes).is_err()
        {
            bytes.clear();
        }
        bytes
    })
}

#[cfg(test)]
#[path = "docker_cli_test.rs"]
mod tests;
