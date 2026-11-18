//! Run one external command with a time limit, capturing its output. A
//! command that outlives its limit is killed (the child is killed when its
//! handle drops) and reported with `rc: None`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::config::expand;
use crate::error::RunError;

#[derive(Debug, Clone, Default)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub stdin: Option<String>,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    /// `None` when the command was killed at its time limit or by a signal.
    pub rc: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.rc == Some(0)
    }

    /// Standard output and error together, for classification and reports.
    pub fn text(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }

    /// The last `max` bytes of `text()`, on a character boundary.
    pub fn tail(&self, max: usize) -> String {
        tail(&self.text(), max)
    }
}

pub fn tail(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.len() <= max {
        return text.to_string();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &text[start..])
}

impl Cmd {
    pub fn new(program: &str, args: &[&str], timeout: Duration) -> Cmd {
        Cmd {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            timeout,
            ..Cmd::default()
        }
    }

    pub fn cwd(mut self, dir: PathBuf) -> Cmd {
        self.cwd = Some(dir);
        self
    }

    pub fn env(mut self, key: &str, value: &str) -> Cmd {
        self.env.push((key.to_string(), value.to_string()));
        self
    }

    pub fn stdin(mut self, text: String) -> Cmd {
        self.stdin = Some(text);
        self
    }

    /// `program args…`, for messages. Never includes stdin or env values.
    pub fn display(&self) -> String {
        let mut words = vec![self.program.clone()];
        words.extend(self.args.iter().take(6).cloned());
        words.join(" ")
    }
}

/// Run `cmd` to completion or its time limit.
pub async fn run(cmd: &Cmd) -> Result<Output, RunError> {
    let program = expand(&cmd.program);
    let mut command = Command::new(&program);
    command
        .args(&cmd.args)
        .envs(cmd.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(if cmd.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = &cmd.cwd {
        command.current_dir(dir);
    }
    let mut child = command.spawn().map_err(|source| RunError::Spawn {
        program: cmd.display(),
        source,
    })?;
    if let (Some(text), Some(mut pipe)) = (&cmd.stdin, child.stdin.take()) {
        let io = |source| RunError::Io {
            program: cmd.display(),
            source,
        };
        pipe.write_all(text.as_bytes()).await.map_err(io)?;
        drop(pipe);
    }
    match tokio::time::timeout(cmd.timeout, child.wait_with_output()).await {
        Err(_) => Ok(Output {
            rc: None,
            stdout: String::new(),
            stderr: format!("{}: killed after {:?}", cmd.display(), cmd.timeout),
        }),
        Ok(Err(source)) => Err(RunError::Io {
            program: cmd.display(),
            source,
        }),
        Ok(Ok(out)) => Ok(Output {
            rc: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
    }
}
