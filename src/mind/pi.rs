//! The pi harness adapter: one persistent session behind a trait.
//!
//! Checked live against pi 0.73.1 (`pi --mode rpc`, 2026-10-25): pi reads
//! LF-delimited JSON commands on stdin and writes LF-delimited JSON records
//! on stdout. Commands carry an optional `id`; each gets one `response`
//! record repeating it (`{"type":"response","command":..,"success":..}`).
//! Session events carry no id. A `prompt` response only means accepted;
//! text streams as `message_update` events whose `assistantMessageEvent`
//! is a `text_delta`, and `agent_end` closes the run, also after a provider
//! error (its last message then has `stopReason` `error` and an
//! `errorMessage`). pi 0.73.1 never emits `agent_settled`. Message `content`
//! is a string or an array of `{type,text}` blocks. `get_state` reports
//! `sessionId` and `sessionFile`. Sessions are JSONL files
//! `<stamp>_<sessionId>.jsonl` under `--session-dir`, written lazily at the
//! first message, and `--session <id>` exits ("No session found") when no
//! such file exists, so a recorded id is only passed when its file is there.
//!
//! [`ProcessPi`] is the subprocess adapter; [`FakePi`] backs the tests.

use std::collections::VecDeque;
use std::future::Future;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc::UnboundedSender;

use crate::VivariumError;

use super::config::MindConfig;

/// Longest one turn may run before the adapter gives up on it.
pub const TURN_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Longest a non-turn command (`get_state`, `get_messages`) may wait for
/// its response. Every wait is bounded: a hung pi must not hold the turn
/// lock forever.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(2 * 60);

/// What a turn streams to an attached client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEvent {
    Text(String),
    Settled,
}

/// The whole text of a finished turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnSummary {
    pub text: String,
}

/// One persistent harness session.
pub trait PiSession: Send {
    /// Send a prompt and stream its turn until pi settles.
    fn prompt(
        &mut self,
        message: String,
        events: Option<UnboundedSender<TurnEvent>>,
    ) -> impl Future<Output = Result<TurnSummary, VivariumError>> + Send;

    /// Compact the session; returns pi's summary text.
    fn compact(&mut self) -> impl Future<Output = Result<String, VivariumError>> + Send;

    /// The last `limit` messages, one line each.
    fn history(
        &mut self,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<String>, VivariumError>> + Send;
}

/// A record read from pi's stdout.
#[derive(Debug, Clone, PartialEq)]
pub enum PiRecord {
    Response {
        id: Option<String>,
        command: String,
        success: bool,
        error: Option<String>,
        data: Value,
    },
    TextDelta(String),
    /// The run ended; `error` carries a provider error or an abort.
    Settled {
        error: Option<String>,
    },
    Other(String),
}

/// One command line for pi's stdin, LF-terminated.
#[must_use]
pub fn command_line(id: &str, kind: &str, fields: Value) -> String {
    let mut record = json!({ "id": id, "type": kind });
    if let (Some(record), Value::Object(fields)) = (record.as_object_mut(), fields) {
        record.extend(fields);
    }
    format!("{record}\n")
}

/// Parse one stdout line. Unknown record types are [`PiRecord::Other`].
///
/// # Errors
/// Fails on a line that is not a JSON object.
pub fn parse_record(line: &str) -> Result<PiRecord, VivariumError> {
    let value: Value = serde_json::from_str(line.trim_end_matches('\r'))
        .map_err(|error| VivariumError::Parse(format!("pi record: {error}")))?;
    let kind = value["type"].as_str().unwrap_or_default();
    Ok(match kind {
        "response" => PiRecord::Response {
            id: value["id"].as_str().map(str::to_string),
            command: value["command"].as_str().unwrap_or_default().to_string(),
            success: value["success"].as_bool().unwrap_or(false),
            error: value["error"].as_str().map(str::to_string),
            data: value["data"].clone(),
        },
        "message_update" => {
            let event = &value["assistantMessageEvent"];
            match (event["type"].as_str(), event["delta"].as_str()) {
                (Some("text_delta"), Some(delta)) => PiRecord::TextDelta(delta.to_string()),
                _ => PiRecord::Other(kind.to_string()),
            }
        }
        "agent_end" | "agent_settled" => PiRecord::Settled {
            error: run_error(&value["messages"]),
        },
        other => PiRecord::Other(other.to_string()),
    })
}

/// The error of a run whose last message stopped on `error` or `aborted`.
fn run_error(messages: &Value) -> Option<String> {
    let last = messages.as_array()?.last()?;
    match last["stopReason"].as_str()? {
        reason @ ("error" | "aborted") => Some(
            last["errorMessage"]
                .as_str()
                .map_or_else(|| reason.to_string(), str::to_string),
        ),
        _ => None,
    }
}

/// Whether `dir` holds the session file for `id` (`<stamp>_<id>.jsonl`).
#[must_use]
pub fn session_exists(dir: &std::path::Path, id: &str) -> bool {
    let suffix = format!("_{id}.jsonl");
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().ends_with(&suffix))
    })
}

/// The pi subprocess, spoken to over stdin/stdout.
pub struct ProcessPi {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

impl ProcessPi {
    /// Start `pi --mode rpc --no-tools` in the session dir, resuming
    /// `session` when given and its file exists (else pi starts a fresh
    /// one). `--no-tools` keeps pi's own bash and edit tools off, so the
    /// reply envelope is the only way a turn acts. The environment is
    /// cleared to `PATH`, `HOME` and `VIVI_PROXY_URL`: the mind passes no
    /// model keys.
    ///
    /// # Errors
    /// Fails when pi cannot be started.
    pub fn spawn(config: &MindConfig, session: Option<&str>) -> Result<Self, VivariumError> {
        std::fs::create_dir_all(&config.paths.session_dir)?;
        let mut command = Command::new(&config.pi_command);
        command
            .args(["--mode", "rpc", "--no-tools", "--session-dir"])
            .arg(&config.paths.session_dir)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &config.paths.home)
            .env("VIVI_PROXY_URL", &config.proxy_url)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if let Some(session) = session.filter(|id| session_exists(&config.paths.session_dir, id)) {
            command.args(["--session", session]);
        }
        if let Some(model) = &config.model {
            command.args(["--model", model]);
        }
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| pipe_error("stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| pipe_error("stdout"))?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
            next_id: 0,
        })
    }

    /// Ask pi which session it is in (`get_state` data carries `sessionId`,
    /// else `sessionFile`). The service stores the answer and
    /// passes it back as `--session` on the next start.
    ///
    /// # Errors
    /// Fails when pi does not answer.
    pub async fn session_id(&mut self) -> Result<Option<String>, VivariumError> {
        let data = self.request("get_state", COMMAND_TIMEOUT).await?;
        Ok(data["sessionId"]
            .as_str()
            .or_else(|| data["sessionFile"].as_str())
            .map(str::to_string))
    }

    /// Whether the child has exited.
    pub fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    async fn send(&mut self, kind: &str, fields: Value) -> Result<String, VivariumError> {
        self.next_id = self.next_id.saturating_add(1);
        let id = format!("mind-{}", self.next_id);
        self.stdin
            .write_all(command_line(&id, kind, fields).as_bytes())
            .await?;
        self.stdin.flush().await?;
        Ok(id)
    }

    /// Send a field-less command and wait at most `limit` for its response.
    async fn request(&mut self, kind: &str, limit: Duration) -> Result<Value, VivariumError> {
        let id = self.send(kind, json!({})).await?;
        tokio::time::timeout(limit, self.response(&id, None))
            .await
            .map_err(|_| VivariumError::Other(format!("pi {kind} timed out")))?
    }

    async fn next_record(&mut self) -> Result<PiRecord, VivariumError> {
        match self.stdout.next_line().await? {
            Some(line) => parse_record(&line),
            None => Err(VivariumError::Other("pi exited".into())),
        }
    }

    /// Read until the response to `id`, forwarding text on the way.
    async fn response(
        &mut self,
        id: &str,
        events: Option<&UnboundedSender<TurnEvent>>,
    ) -> Result<Value, VivariumError> {
        loop {
            match self.next_record().await? {
                PiRecord::Response {
                    id: Some(got),
                    success,
                    error,
                    data,
                    command,
                } if got == id => {
                    return if success {
                        Ok(data)
                    } else {
                        Err(VivariumError::Other(format!(
                            "pi {command} failed: {}",
                            error.unwrap_or_default()
                        )))
                    };
                }
                PiRecord::TextDelta(text) => forward(events, TurnEvent::Text(text)),
                _ => {}
            }
        }
    }

    async fn run_turn(
        &mut self,
        message: String,
        events: Option<UnboundedSender<TurnEvent>>,
    ) -> Result<TurnSummary, VivariumError> {
        let id = self.send("prompt", json!({ "message": message })).await?;
        let data = self.response(&id, events.as_ref()).await?;
        let mut summary = TurnSummary::default();
        if data["disposition"].as_str() == Some("handled") {
            return Ok(summary);
        }
        loop {
            match self.next_record().await? {
                PiRecord::TextDelta(text) => {
                    summary.text.push_str(&text);
                    forward(events.as_ref(), TurnEvent::Text(text));
                }
                PiRecord::Settled { error } => {
                    forward(events.as_ref(), TurnEvent::Settled);
                    return match error {
                        Some(error) => {
                            Err(VivariumError::Other(format!("pi turn failed: {error}")))
                        }
                        None => Ok(summary),
                    };
                }
                _ => {}
            }
        }
    }
}

impl PiSession for ProcessPi {
    async fn prompt(
        &mut self,
        message: String,
        events: Option<UnboundedSender<TurnEvent>>,
    ) -> Result<TurnSummary, VivariumError> {
        tokio::time::timeout(TURN_TIMEOUT, self.run_turn(message, events))
            .await
            .map_err(|_| VivariumError::Other("pi turn timed out".into()))?
    }

    async fn compact(&mut self) -> Result<String, VivariumError> {
        // Compaction summarises with a model call, so it gets a turn's bound.
        let data = self.request("compact", TURN_TIMEOUT).await?;
        Ok(data["summary"].as_str().unwrap_or_default().to_string())
    }

    async fn history(&mut self, limit: usize) -> Result<Vec<String>, VivariumError> {
        let data = self.request("get_messages", COMMAND_TIMEOUT).await?;
        Ok(history_lines(&data, limit))
    }
}

/// One line per message of `get_messages` data
/// (`{"messages":[{"role":..,"content":..}]}`): the text blocks joined,
/// else the error message, else the raw content.
#[must_use]
pub fn history_lines(data: &Value, limit: usize) -> Vec<String> {
    let messages = data["messages"].as_array().cloned().unwrap_or_default();
    let skip = messages.len().saturating_sub(limit);
    messages
        .iter()
        .skip(skip)
        .map(|message| {
            let role = message["role"].as_str().unwrap_or("?");
            let text = match &message["content"] {
                Value::String(text) => text.clone(),
                Value::Array(blocks) if blocks.iter().any(|block| block["text"].is_string()) => {
                    blocks
                        .iter()
                        .filter_map(|block| block["text"].as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                }
                other => message["errorMessage"]
                    .as_str()
                    .map_or_else(|| other.to_string(), |error| format!("[error] {error}")),
            };
            format!("{role}: {}", text.replace('\n', " "))
        })
        .collect()
}

fn forward(events: Option<&UnboundedSender<TurnEvent>>, event: TurnEvent) {
    if let Some(events) = events
        && events.send(event).is_err()
    {
        tracing::debug!("attached client went away mid-turn");
    }
}

fn pipe_error(which: &str) -> VivariumError {
    VivariumError::Other(format!("pi {which} was not piped"))
}

/// A scripted session for tests: replies come from a queue, prompts are
/// recorded.
#[derive(Debug, Default)]
pub struct FakePi {
    pub prompts: Vec<String>,
    pub replies: VecDeque<String>,
    pub compactions: usize,
}

impl FakePi {
    /// A fake whose turns answer with `replies` in order, then `no-op`.
    #[must_use]
    pub fn with_replies(replies: &[&str]) -> Self {
        Self {
            replies: replies.iter().map(|reply| (*reply).to_string()).collect(),
            ..Self::default()
        }
    }
}

impl PiSession for FakePi {
    async fn prompt(
        &mut self,
        message: String,
        events: Option<UnboundedSender<TurnEvent>>,
    ) -> Result<TurnSummary, VivariumError> {
        self.prompts.push(message);
        let text = self
            .replies
            .pop_front()
            .unwrap_or_else(|| String::from("no-op"));
        forward(events.as_ref(), TurnEvent::Text(text.clone()));
        forward(events.as_ref(), TurnEvent::Settled);
        Ok(TurnSummary { text })
    }

    async fn compact(&mut self) -> Result<String, VivariumError> {
        self.compactions = self.compactions.saturating_add(1);
        Ok(format!("compacted {} prompts", self.prompts.len()))
    }

    async fn history(&mut self, limit: usize) -> Result<Vec<String>, VivariumError> {
        let skip = self.prompts.len().saturating_sub(limit);
        Ok(self
            .prompts
            .iter()
            .skip(skip)
            .map(|prompt| format!("user: {}", prompt.replace('\n', " ")))
            .collect())
    }
}

#[cfg(test)]
#[path = "pi_test.rs"]
mod tests;
